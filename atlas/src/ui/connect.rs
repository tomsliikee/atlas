//! Adding or editing a server connection, asking for its password, and
//! saying in plain words why a connection failed.
//!
//! Passwords are never stored: the field says so, and the dialog for a
//! saved connection asks every time.

use std::rc::Rc;

use adw::prelude::*;
use atlas_core::problem::{self, Problem};
use atlas_core::sql::Dialect;
use atlas_core::store::{Connection, Ssh};
use gtk::glib;

use crate::i18n::tr;
use crate::{db, runtime};

const KINDS: [Dialect; 2] = [Dialect::MySql, Dialect::Postgres];

fn entry(title: &str, text: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).text(text).build()
}

/// The fields of the dialog.
#[derive(Clone)]
struct Form {
    id: Option<String>,
    kind: adw::ComboRow,
    name: adw::EntryRow,
    host: adw::EntryRow,
    port: adw::EntryRow,
    user: adw::EntryRow,
    database: adw::EntryRow,
    password: adw::PasswordEntryRow,
    socket: adw::EntryRow,
    ssh: adw::ExpanderRow,
    ssh_host: adw::EntryRow,
    ssh_user: adw::EntryRow,
    ssh_port: adw::EntryRow,
}

impl Form {
    fn connection(&self) -> Connection {
        let dialect = KINDS[self.kind.selected() as usize % KINDS.len()];
        let database = self.database.text().trim().to_owned();
        let host = self.host.text().trim().to_owned();
        let name = self.name.text().trim().to_owned();
        Connection {
            id: self.id.clone().unwrap_or_else(new_id),
            name: if name.is_empty() { format!("{database} @ {host}") } else { name },
            dialect,
            port: self.port.text().trim().parse::<u16>().unwrap_or(Connection::default_port(dialect)),
            host,
            user: self.user.text().trim().to_owned(),
            database,
            socket: Some(self.socket.text().trim().to_owned()).filter(|s| !s.is_empty()),
            ssh: self
                .ssh
                .enables_expansion()
                .then(|| Ssh {
                    host: self.ssh_host.text().trim().to_owned(),
                    user: Some(self.ssh_user.text().trim().to_owned()).filter(|s| !s.is_empty()),
                    port: self.ssh_port.text().trim().parse().ok(),
                })
                .filter(|s| !s.host.is_empty()),
        }
    }
}

/// Shows the dialog; `done` gets the connection and the typed password.
pub fn edit(parent: &impl IsA<gtk::Widget>, existing: Option<&Connection>, done: impl Fn(Connection, String) + 'static) {
    let c = existing.cloned();
    let text = |f: &dyn Fn(&Connection) -> String, default: &str| c.as_ref().map_or_else(|| default.to_owned(), f);
    let form = Form {
        id: c.as_ref().map(|c| c.id.clone()),
        kind: adw::ComboRow::builder()
            .title(tr("Database"))
            .model(&gtk::StringList::new(&["MariaDB / MySQL", "PostgreSQL"]))
            .build(),
        name: entry(&tr("Name"), &text(&|c| c.name.clone(), "")),
        host: entry(&tr("Host"), &text(&|c| c.host.clone(), "localhost")),
        port: entry(&tr("Port"), &text(&|c| c.port.to_string(), "3306")),
        user: entry(&tr("User"), &text(&|c| c.user.clone(), "")),
        database: entry(&tr("Database name"), &text(&|c| c.database.clone(), "")),
        password: adw::PasswordEntryRow::builder().title(tr("Password (not saved)")).build(),
        socket: entry(&tr("Unix socket (optional)"), &text(&|c| c.socket.clone().unwrap_or_default(), "")),
        ssh: adw::ExpanderRow::builder()
            .title(tr("SSH tunnel"))
            .subtitle(tr("Through the system ssh: ~/.ssh/config and the agent apply"))
            .show_enable_switch(true)
            .enable_expansion(c.as_ref().is_some_and(|c| c.ssh.is_some()))
            .build(),
        ssh_host: entry(&tr("SSH host or alias"), &text(&|c| c.ssh.as_ref().map(|s| s.host.clone()).unwrap_or_default(), "")),
        ssh_user: entry(&tr("SSH user (optional)"), &text(&|c| c.ssh.as_ref().and_then(|s| s.user.clone()).unwrap_or_default(), "")),
        ssh_port: entry(
            &tr("SSH port (optional)"),
            &text(&|c| c.ssh.as_ref().and_then(|s| s.port).map(|p| p.to_string()).unwrap_or_default(), ""),
        ),
    };
    form.port.set_input_purpose(gtk::InputPurpose::Digits);
    if let Some(c) = &c {
        form.kind.set_selected(KINDS.iter().position(|k| *k == c.dialect).unwrap_or(0) as u32);
    }
    // Follow the default port while the user has not typed their own.
    form.kind.connect_selected_notify(glib::clone!(
        #[weak(rename_to = port)]
        form.port,
        move |k| {
            let text = port.text();
            if text.is_empty() || text == "3306" || text == "5432" {
                let d = KINDS[k.selected() as usize % KINDS.len()];
                port.set_text(&Connection::default_port(d).to_string());
            }
        }
    ));
    form.ssh.add_row(&form.ssh_host);
    form.ssh.add_row(&form.ssh_user);
    form.ssh.add_row(&form.ssh_port);

    // The list button beside the database name: asks the server which
    // databases this user may open, with what is typed so far.
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["db-list"]).build();
    let list_scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .propagate_natural_height(true)
        .max_content_height(320)
        .min_content_width(260)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let popover = gtk::Popover::builder().child(&list_scroll).build();
    let pick = gtk::MenuButton::builder()
        .icon_name("view-list-bullet-symbolic")
        .tooltip_text(tr("Databases this user may open"))
        .popover(&popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    form.database.add_suffix(&pick);
    let f = form.clone();
    popover.connect_show(move |pop| {
        fill_list(&list, pop, &f);
    });

    let server = adw::PreferencesGroup::new();
    for row in [
        form.kind.upcast_ref::<gtk::Widget>(),
        form.name.upcast_ref(),
        form.host.upcast_ref(),
        form.port.upcast_ref(),
        form.user.upcast_ref(),
        form.password.upcast_ref(),
        form.database.upcast_ref(),
    ] {
        server.add(row);
    }
    let more = adw::PreferencesGroup::builder().description(tr("Atlas opens every session read-only.")).build();
    more.add(&form.socket);
    more.add(&form.ssh);
    let page = adw::PreferencesPage::new();
    page.add(&server);
    page.add(&more);

    let connect = gtk::Button::builder().label(tr("Connect")).css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    let cancel = gtk::Button::builder().label(tr("Cancel")).build();
    header.pack_start(&cancel);
    header.pack_end(&connect);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(if c.is_some() { tr("Edit Connection") } else { tr("New Connection") })
        .content_width(460)
        .content_height(640)
        .child(&toolbar)
        .default_widget(&connect)
        .build();
    dialog.set_focus(Some(if c.is_some() { form.password.upcast_ref::<gtk::Widget>() } else { form.name.upcast_ref::<gtk::Widget>() }));
    for row in [&form.name, &form.host, &form.port, &form.user, &form.database, &form.socket, &form.ssh_host, &form.ssh_user, &form.ssh_port] {
        row.set_activates_default(true);
    }
    form.password.set_activates_default(true);

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let done = Rc::new(done);
    connect.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let conn = form.connection();
            dialog.close();
            done(conn, form.password.text().to_string());
        }
    ));
    dialog.present(Some(parent));
}

/// Loads the databases into the popover's list; a click puts the name into
/// the field.
fn fill_list(list: &gtk::ListBox, popover: &gtk::Popover, form: &Form) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let note = |text: &str| gtk::Label::builder().label(text).wrap(true).max_width_chars(34).xalign(0.0).css_classes(["hint"]).margin_top(10).margin_bottom(10).margin_start(10).margin_end(10).build();
    list.append(&note(&tr("Asking the server…")));
    let conn = form.connection();
    let password = form.password.text().to_string();
    let (list, popover, field) = (list.clone(), popover.clone(), form.database.clone());
    glib::spawn_future_local(async move {
        let result = runtime::run(db::list_databases(conn.clone(), password)).await;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        match result {
            Ok(names) if names.is_empty() => list.append(&note(&tr("This user may not open any database."))),
            Ok(names) => {
                for name in names {
                    let button = gtk::Button::builder()
                        .child(&gtk::Label::builder().label(&name).xalign(0.0).build())
                        .css_classes(["flat"])
                        .build();
                    let (popover, field) = (popover.clone(), field.clone());
                    button.connect_clicked(move |_| {
                        field.set_text(&name);
                        popover.popdown();
                    });
                    list.append(&button);
                }
            }
            Err(e) => {
                let (heading, body) = describe(&problem::classify(&e), &conn);
                list.append(&note(&format!("{heading}. {body}")));
            }
        }
    });
}

/// A heading and a sentence on what to do, for a failed connection.
pub fn describe(problem: &Problem, conn: &Connection) -> (String, String) {
    let user = conn.user.clone();
    let host = conn.ssh.as_ref().map_or_else(|| conn.host.clone(), |s| s.host.clone());
    let fill = |s: String| s.replace("{user}", &user).replace("{host}", &host).replace("{port}", &conn.port.to_string());
    match problem {
        Problem::NoAccess(db) => (
            fill(tr("{user} may not open “{db}”")).replace("{db}", db),
            tr("The database name is probably not the one this user was given. Pick one of the databases below."),
        ),
        Problem::NoDatabase(db) => (
            tr("There is no database “{db}”").replace("{db}", db),
            tr("Check the spelling, or pick one of the databases below."),
        ),
        Problem::Login => (tr("User or password not accepted"), fill(tr("The server rejected {user} with this password. Passwords are not saved, so it has to be typed again."))),
        Problem::HostRefused => (
            fill(tr("The server does not accept {user} from here")),
            tr("The user exists, but is only allowed from other hosts. With an SSH tunnel Atlas connects from the server itself."),
        ),
        Problem::Unreachable => (fill(tr("Nothing answers on {host}:{port}")), tr("Is the database running, and is host and port right? For a remote server, an SSH tunnel may be needed.")),
        Problem::UnknownHost => (fill(tr("The host “{host}” is unknown")), tr("Check the host name.")),
        Problem::Timeout => (fill(tr("{host} did not answer in time")), tr("A firewall may drop the connection, or the server is busy.")),
        Problem::Ssh => (tr("The SSH tunnel did not open"), tr("Atlas uses your ssh-agent; the key must be loaded (ssh-add) and the host must be in known_hosts.")),
        Problem::Other => (tr("Could not connect"), String::new()),
    }
}

/// After a failed connection: the reason in plain words, the server's own
/// message below, and when the database was the problem, the databases this
/// user may open. `retry` gets a changed connection; `edit` opens the form.
pub fn failed(
    parent: &impl IsA<gtk::Widget>,
    conn: &Connection,
    password: &str,
    error: &str,
    retry: impl Fn(Connection, String) + 'static,
    edit_again: impl Fn() + 'static,
) {
    let problem = problem::classify(error);
    let (heading, body) = describe(&problem, conn);
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    let dialog = adw::AlertDialog::builder().heading(&heading).body(&body).extra_child(&content).close_response("close").build();
    dialog.add_responses(&[("edit", &tr("Edit Connection")), ("close", &tr("Close"))]);

    if problem.is_about_database() {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        let waiting = gtk::Label::builder().label(tr("Asking the server…")).css_classes(["hint"]).build();
        content.append(&waiting);
        content.append(&list);
        let retry = Rc::new(retry);
        let (c, p) = (conn.clone(), password.to_owned());
        let weak = dialog.downgrade();
        glib::spawn_future_local(async move {
            let result = runtime::run(db::list_databases(c.clone(), p.clone())).await;
            waiting.set_visible(false);
            let names = match result {
                Ok(n) if !n.is_empty() => n,
                _ => {
                    list.set_visible(false);
                    return;
                }
            };
            for name in names {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&name)).activatable(true).build();
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let (retry, c, p, weak) = (retry.clone(), c.clone(), p.clone(), weak.clone());
                row.connect_activated(move |_| {
                    if let Some(d) = weak.upgrade() {
                        d.force_close();
                    }
                    let mut changed = c.clone();
                    // A name made from the old database follows the new one.
                    if changed.name == format!("{} @ {}", c.database, c.host) {
                        changed.name = format!("{name} @ {}", c.host);
                    }
                    changed.database = name.clone();
                    retry(changed, p.clone());
                });
                list.append(&row);
            }
        });
    }
    // The server's own words, small, for searching or for an admin.
    let detail = gtk::Label::builder()
        .label(error)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .css_classes(["error-detail"])
        .build();
    content.append(&detail);
    dialog.connect_response(None, move |_, response| {
        if response == "edit" {
            edit_again();
        }
    });
    dialog.present(Some(parent));
}

/// Asks for the password of a saved connection.
pub fn password(parent: &impl IsA<gtk::Widget>, conn: &Connection, done: impl Fn(String) + 'static) {
    let entry = gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).build();
    let dialog = adw::AlertDialog::builder()
        .heading(&conn.name)
        .body(tr("Password for {user}. It is not saved.").replace("{user}", &conn.subtitle()))
        .extra_child(&entry)
        .default_response("connect")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", &tr("Cancel")), ("connect", &tr("Connect"))]);
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
    let field = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "connect" {
            done(field.text().to_string());
        }
    });
    dialog.present(Some(parent));
}

fn new_id() -> String {
    format!("{:x}{:08x}", glib::real_time(), glib::random_int())
}
