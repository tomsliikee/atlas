//! Server information (version, sizes, who is connected) and, on a
//! writable session, users and their privileges.

use std::rc::Rc;

use adw::prelude::*;
use atlas_core::admin::{self, Account, On};
use atlas_core::sql::Dialect;
use gtk::glib;

use super::Ctx;
use super::grid::Grid;
use crate::i18n::tr;
use crate::runtime;

fn page_dialog(title: &str, content: &impl IsA<gtk::Widget>, width: i32, height: i32) -> (adw::Dialog, adw::HeaderBar) {
    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    let dialog = adw::Dialog::builder().title(title).content_width(width).content_height(height).child(&toolbar).build();
    (dialog, header)
}

/// Version, table sizes and the process list, with a refresh button.
pub fn server_info(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>) {
    let Some(session) = ctx.session.borrow().clone() else { return };
    let version = gtk::Label::builder().xalign(0.0).selectable(true).css_classes(["mono"]).build();
    let sizes = Grid::new();
    let processes = Grid::new();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();
    content.append(&version);
    content.append(&gtk::Label::builder().label(tr("Size per table")).xalign(0.0).css_classes(["section-title"]).build());
    content.append(&sizes.widget);
    if admin::processes_query(session.dialect).is_some() {
        content.append(&gtk::Label::builder().label(tr("Connections")).xalign(0.0).css_classes(["section-title"]).build());
        content.append(&processes.widget);
    }
    let (dialog, header) = page_dialog(&tr("Server"), &content, 760, 620);
    let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text(tr("Refresh")).build();
    header.pack_start(&refresh);
    let load = Rc::new(move || {
        let (s, v, sz, pr) = (session.clone(), version.clone(), sizes.clone(), processes.clone());
        glib::spawn_future_local(async move {
            let d = s.dialect;
            let result = runtime::run(async move {
                let version = s.version().await.unwrap_or_default();
                let mut sizes = s.query(admin::sizes_query(d).into(), 10_000).await;
                if d == Dialect::Sqlite && sizes.is_err() {
                    // Without the dbstat table: the whole file.
                    sizes = s.query("SELECT 'all tables', page_count * page_size, 0, NULL FROM pragma_page_count(), pragma_page_size()".into(), 1).await;
                }
                let processes = match admin::processes_query(d) {
                    Some(q) => s.query(q.into(), 1000).await.ok(),
                    None => None,
                };
                (version, sizes, processes)
            })
            .await;
            let (version, sizes, processes) = result;
            v.set_text(&version);
            match sizes {
                Ok(set) => {
                    let rows = set
                        .rows
                        .into_iter()
                        .map(|r| {
                            let n = |i: usize| r.get(i).cloned().flatten().and_then(|v| v.parse::<f64>().ok()).map(|v| admin::human_size(v.max(0.0) as u64));
                            vec![r.first().cloned().flatten(), n(1), n(2), r.get(3).cloned().flatten()]
                        })
                        .collect();
                    sz.set(&[tr("Table"), tr("Data"), tr("Indexes"), tr("Rows (about)")], rows, &[], false);
                }
                Err(e) => sz.set(&[tr("Error")], vec![vec![Some(e)]], &[], false),
            }
            if let Some(set) = processes {
                pr.set(&set.columns, set.rows, &[], false);
            }
        });
    });
    load();
    let l = load.clone();
    refresh.connect_clicked(move |_| l());
    dialog.present(Some(parent));
}

/// Users with their grants; create, drop, set password, grant, revoke.
/// Every statement is shown before it runs.
pub fn users(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>) {
    let Some(session) = ctx.session.borrow().clone() else { return };
    let Some(query) = admin::users_query(session.dialect) else { return };
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::Single).build();
    let grants = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).css_classes(["mono", "hint"]).build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&list).min_content_height(200).vexpand(true).build();
    content.append(&scroll);
    content.append(&gtk::Label::builder().label(tr("Privileges")).xalign(0.0).css_classes(["section-title"]).build());
    content.append(&grants);
    let actions = gtk::Box::builder().spacing(8).build();
    let grant = gtk::Button::builder().label(tr("Grant…")).css_classes(["quiet", "small"]).build();
    let revoke = gtk::Button::builder().label(tr("Revoke…")).css_classes(["quiet", "small"]).build();
    let password = gtk::Button::builder().label(tr("Set Password…")).css_classes(["quiet", "small"]).build();
    let drop = gtk::Button::builder().label(tr("Drop…")).css_classes(["quiet", "small"]).build();
    for b in [&grant, &revoke, &password, &drop] {
        b.set_sensitive(false);
        actions.append(b);
    }
    content.append(&actions);
    let (dialog, header) = page_dialog(&tr("Users"), &content, 620, 640);
    let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("New user")).build();
    header.pack_start(&add);

    let accounts: Rc<std::cell::RefCell<Vec<Account>>> = Rc::default();
    let dialect = session.dialect;
    let reload: Rc<dyn Fn()> = {
        let (s, list, accounts) = (session.clone(), list.clone(), accounts.clone());
        Rc::new(move || {
            let (s, list, accounts) = (s.clone(), list.clone(), accounts.clone());
            glib::spawn_future_local(async move {
                let r = runtime::run(async move { s.query(query.into(), 10_000).await }).await;
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                accounts.borrow_mut().clear();
                match r {
                    Ok(set) => {
                        for row in set.rows {
                            let name = row.first().cloned().flatten().unwrap_or_default();
                            let host = row.get(1).cloned().flatten().unwrap_or_default();
                            let item = adw::ActionRow::builder().title(&name).subtitle(&host).build();
                            list.append(&item);
                            let host = if dialect == Dialect::MySql { host } else { String::new() };
                            accounts.borrow_mut().push(Account { name, host });
                        }
                    }
                    Err(e) => list.append(&gtk::Label::builder().label(e).wrap(true).css_classes(["hint"]).build()),
                }
            });
        })
    };
    reload();

    let selected = {
        let (list, accounts) = (list.clone(), accounts.clone());
        move || list.selected_row().and_then(|r| accounts.borrow().get(r.index() as usize).cloned())
    };
    {
        let (s, grants, buttons) = (session.clone(), grants.clone(), [grant.clone(), revoke.clone(), password.clone(), drop.clone()]);
        let sel = selected.clone();
        list.connect_row_selected(move |_, _| {
            let account = sel();
            for b in &buttons {
                b.set_sensitive(account.is_some());
            }
            let Some(a) = account else { return };
            let Some(q) = admin::grants_query(s.dialect, &a) else { return };
            let (s, grants) = (s.clone(), grants.clone());
            glib::spawn_future_local(async move {
                let r = runtime::run(async move { s.query(q, 1000).await }).await;
                grants.set_text(&match r {
                    Ok(set) => set.rows.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect::<Vec<_>>().join("\n"),
                    Err(e) => e,
                });
            });
        });
    }

    let run: Rc<RunFn> = {
        let (s, reload, ctx) = (session.clone(), reload.clone(), ctx.clone());
        Rc::new(move |w: &gtk::Widget, heading: String, sql: String, shown: String, destructive: bool| {
            let (s, reload, ctx) = (s.clone(), reload.clone(), ctx.clone());
            super::review::review(w, &heading, "", &shown, &tr("Run"), destructive, move || {
                super::review::run_batch(s, vec![sql], false, move |r| match r {
                    Ok(_) => reload(),
                    Err(e) => ctx.toast(&e),
                });
            });
        })
    };

    {
        let (run, d) = (run.clone(), dialog.clone());
        add.connect_clicked(move |_| {
            let (form, name, host, pass) = account_form(dialect);
            let ask = adw::AlertDialog::new(Some(&tr("New User")), None);
            ask.set_extra_child(Some(&form));
            ask.add_response("cancel", &tr("Cancel"));
            ask.add_response("ok", &tr("Review…"));
            ask.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
            let (run, d2) = (run.clone(), d.clone());
            ask.connect_response(None, move |_, r| {
                if r != "ok" || name.text().trim().is_empty() {
                    return;
                }
                let a = Account { name: name.text().trim().into(), host: host.text().trim().into() };
                let s = admin::create_user(dialect, &a, &pass.text());
                run(d2.upcast_ref(), tr("Create User?"), s.sql, s.shown, false);
            });
            ask.present(Some(&d));
        });
    }
    for (button, is_grant) in [(&grant, true), (&revoke, false)] {
        let (run, d, sel, ctx) = (run.clone(), dialog.clone(), selected.clone(), ctx.clone());
        button.connect_clicked(move |_| {
            let Some(a) = sel() else { return };
            let (form, privileges, target) = grant_form(&ctx);
            let ask = adw::AlertDialog::new(Some(&if is_grant { tr("Grant") } else { tr("Revoke") }), Some(&a.name));
            ask.set_extra_child(Some(&form));
            ask.add_response("cancel", &tr("Cancel"));
            ask.add_response("ok", &tr("Review…"));
            ask.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
            let (run, d2, ctx2) = (run.clone(), d.clone(), ctx.clone());
            ask.connect_response(None, move |_, r| {
                if r != "ok" {
                    return;
                }
                let chosen: Vec<&str> = privileges.iter().filter(|(_, c)| c.is_active()).map(|(p, _)| *p).collect();
                if chosen.is_empty() {
                    return;
                }
                let on = target_of(&ctx2, &target, dialect);
                let sql = if is_grant { admin::grant(dialect, &chosen, &on, &a) } else { admin::revoke(dialect, &chosen, &on, &a) };
                run(d2.upcast_ref(), if is_grant { tr("Grant?") } else { tr("Revoke?") }, sql.clone(), sql, !is_grant);
            });
            ask.present(Some(&d));
        });
    }
    {
        let (run, d, sel) = (run.clone(), dialog.clone(), selected.clone());
        password.connect_clicked(move |_| {
            let Some(a) = sel() else { return };
            let entry = gtk::PasswordEntry::builder().show_peek_icon(true).build();
            let ask = adw::AlertDialog::new(Some(&tr("Set Password")), Some(&a.name));
            ask.set_extra_child(Some(&entry));
            ask.add_response("cancel", &tr("Cancel"));
            ask.add_response("ok", &tr("Review…"));
            let (run, d2) = (run.clone(), d.clone());
            ask.connect_response(None, move |_, r| {
                if r == "ok" {
                    let s = admin::set_password(dialect, &a, &entry.text());
                    run(d2.upcast_ref(), tr("Set Password?"), s.sql, s.shown, false);
                }
            });
            ask.present(Some(&d));
        });
    }
    {
        let (run, d, sel) = (run.clone(), dialog.clone(), selected.clone());
        drop.connect_clicked(move |_| {
            let Some(a) = sel() else { return };
            let sql = admin::drop_user(dialect, &a);
            run(d.upcast_ref(), tr("Drop User?"), sql.clone(), sql, true);
        });
    }
    dialog.present(Some(parent));
}

/// Shows a statement (heading, SQL, SQL as shown, destructive), runs it.
type RunFn = dyn Fn(&gtk::Widget, String, String, String, bool);

fn account_form(dialect: Dialect) -> (gtk::Box, gtk::Entry, gtk::Entry, gtk::PasswordEntry) {
    let form = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    let name = gtk::Entry::builder().placeholder_text(tr("Name")).build();
    let host = gtk::Entry::builder().placeholder_text(tr("Host (% for any)")).text("%").visible(dialect == Dialect::MySql).build();
    let pass = gtk::PasswordEntry::builder().placeholder_text(tr("Password")).show_peek_icon(true).build();
    form.append(&name);
    form.append(&host);
    form.append(&pass);
    (form, name, host, pass)
}

type Checks = Vec<(&'static str, gtk::CheckButton)>;

fn grant_form(ctx: &Ctx) -> (gtk::Box, Checks, gtk::DropDown) {
    let form = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    let mut checks = Vec::new();
    for p in admin::PRIVILEGES {
        let c = gtk::CheckButton::builder().label(p).active(p == "SELECT").build();
        form.append(&c);
        checks.push((p, c));
    }
    let mut targets = vec![tr("Every table")];
    if let Some(s) = ctx.schema.borrow().as_ref() {
        targets.extend(s.tables.iter().map(|t| t.name.clone()));
    }
    let refs: Vec<&str> = targets.iter().map(String::as_str).collect();
    let target = gtk::DropDown::from_strings(&refs);
    form.append(&target);
    (form, checks, target)
}

fn target_of(ctx: &Ctx, d: &gtk::DropDown, dialect: Dialect) -> On {
    let schema = ctx.schema.borrow();
    match (d.selected() as usize).checked_sub(1).and_then(|i| schema.as_ref()?.tables.get(i)) {
        Some(t) => On::Table(t.name.clone()),
        None => {
            let db = ctx.session.borrow().as_ref().map(|s| s.name.clone()).unwrap_or_default();
            On::Database(if dialect == Dialect::Postgres { "public".into() } else { db })
        }
    }
}
