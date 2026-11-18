//! "Compare with…": the open schema against a dump, an SQLite file or a
//! saved connection. Lists what differs and offers the SQL that makes the
//! open database look like the other one.

use std::rc::Rc;

use adw::prelude::*;
use atlas_core::diff::{self, SchemaDiff};
use atlas_core::schema::Schema;
use atlas_core::store::{self, Connection};
use gtk::{gio, glib};

use super::{Ctx, config_dir};
use crate::db::{self, Target};
use crate::i18n::tr;
use crate::runtime;

/// Asks what to compare with.
pub fn open(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>) {
    if ctx.schema.borrow().is_none() {
        return;
    }
    let dialog = adw::AlertDialog::new(Some(&tr("Compare With…")), Some(&tr("Another version of this database: a dump, an SQLite file, or a saved connection.")));
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    let file = adw::ActionRow::builder().title(tr("SQL dump or SQLite file…")).activatable(true).build();
    list.append(&file);
    let saved: Vec<Connection> = store::load(&config_dir().join("connections.json"));
    for c in &saved {
        let row = adw::ActionRow::builder().title(&c.name).subtitle(c.subtitle()).activatable(true).build();
        list.append(&row);
    }
    dialog.set_extra_child(Some(&list));
    dialog.add_response("cancel", &tr("Cancel"));
    let p = parent.clone().upcast::<gtk::Widget>();
    let d = dialog.clone();
    list.connect_row_activated(move |_, row| {
        d.close();
        let index = row.index();
        if index == 0 {
            pick_file(&p, ctx.clone());
        } else if let Some(conn) = saved.get(index as usize - 1).cloned() {
            let (p2, c2) = (p.clone(), ctx.clone());
            let c3 = conn.clone();
            super::connect::password(&p, &conn, move |password| {
                let (p3, ctx3) = (p2.clone(), c2.clone());
                let conn = c3.clone();
                load(&p3, ctx3, conn.name.clone(), async move {
                    let s = db::connect(Target::Server(conn), password).await?;
                    let schema = s.schema().await;
                    s.close().await;
                    schema
                });
            });
        }
    });
    dialog.present(Some(parent));
}

/// Compares with a file without asking for it (scripted runs).
pub fn with_file(parent: &gtk::Widget, ctx: Rc<Ctx>, path: std::path::PathBuf) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    load(parent, ctx, name, async move {
        let text = std::fs::read(&path).map_err(|e| e.to_string())?;
        Ok(atlas_core::dump::parse("other", &String::from_utf8_lossy(&text)).0)
    });
}

fn pick_file(parent: &gtk::Widget, ctx: Rc<Ctx>) {
    let Some(window) = parent.root().and_downcast::<gtk::Window>() else { return };
    let dialog = gtk::FileDialog::builder().title(tr("Compare With File")).modal(true).build();
    let p = parent.clone();
    dialog.open(Some(&window), gio::Cancellable::NONE, move |r| {
        let Ok(file) = r else { return };
        let Some(path) = file.path() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        load(&p, ctx, name, async move {
            let mut head = [0u8; 16];
            let sqlite = std::fs::File::open(&path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok()
                && &head == b"SQLite format 3\0";
            if sqlite {
                let s = db::connect(Target::File(path), String::new()).await?;
                let schema = s.schema().await;
                s.close().await;
                schema
            } else {
                let text = std::fs::read(&path).map_err(|e| e.to_string())?;
                Ok(atlas_core::dump::parse("other", &String::from_utf8_lossy(&text)).0)
            }
        });
    });
}

fn load(parent: &gtk::Widget, ctx: Rc<Ctx>, name: String, other: impl std::future::Future<Output = Result<Schema, String>> + Send + 'static) {
    let p = parent.clone();
    glib::spawn_future_local(async move {
        match runtime::run(other).await {
            Ok(other) => show(&p, ctx, &name, other),
            Err(e) => super::review::failed(&p, &tr("Could not read the other schema"), &e),
        }
    });
}

fn describe(d: &SchemaDiff) -> String {
    let mut out = String::new();
    for t in &d.added_tables {
        out.push_str(&format!("+ {t}\n"));
    }
    for t in &d.removed_tables {
        out.push_str(&format!("− {t}\n"));
    }
    for t in &d.changed_tables {
        out.push_str(&format!("~ {}\n", t.name));
        for c in &t.added_columns {
            out.push_str(&format!("    + {c}\n"));
        }
        for c in &t.removed_columns {
            out.push_str(&format!("    − {c}\n"));
        }
        for (c, what) in &t.changed_columns {
            out.push_str(&format!("    ~ {c} ({})\n", what.join(", ")));
        }
        for i in &t.added_indexes {
            out.push_str(&format!("    + {} {i}\n", tr("index")));
        }
        for i in &t.removed_indexes {
            out.push_str(&format!("    − {} {i}\n", tr("index")));
        }
        for f in &t.added_fks {
            out.push_str(&format!("    + {f}\n"));
        }
        for f in &t.removed_fks {
            out.push_str(&format!("    − {f}\n"));
        }
        if t.checks_changed {
            out.push_str(&format!("    ~ {}\n", tr("checks")));
        }
    }
    out
}

fn show(parent: &gtk::Widget, ctx: Rc<Ctx>, name: &str, other: Schema) {
    let Some(mine) = ctx.schema.borrow().clone() else { return };
    let d = diff::diff(&mine, &other);
    if d.is_empty() {
        ctx.toast(&tr("No differences in structure"));
        return;
    }
    let dialect = ctx.session.borrow().as_ref().map_or(atlas_core::sql::Dialect::MySql, |s| s.dialect);
    let migration = diff::migration(dialect, &mine, &other);
    let dialog = adw::AlertDialog::new(
        Some(&tr("{this} and {other}").replace("{this}", &mine.name).replace("{other}", name)),
        Some(&tr("+ only in {other} · − only here · ~ different. The SQL makes this database look like {other}.").replace("{other}", name)),
    );
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    let (list, _) = super::review::sql_view(&describe(&d));
    let (sql, _) = super::review::sql_view(&migration.text());
    content.append(&list);
    content.append(&sql);
    content.set_size_request(560, -1);
    dialog.set_extra_child(Some(&content));
    dialog.add_response("close", &tr("Close"));
    dialog.add_response("copy", &tr("Copy SQL"));
    if ctx.writable() {
        dialog.add_response("apply", &tr("Apply…"));
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Destructive);
    }
    let p = parent.clone();
    let text = migration.text();
    dialog.connect_response(None, move |_, r| match r {
        "copy" => {
            p.clipboard().set_text(&text);
            ctx.toast(&tr("Copied"));
        }
        "apply" => {
            let Some(session) = ctx.session.borrow().clone() else { return };
            let c = ctx.clone();
            let (statements, fk_off) = (migration.statements.clone(), migration.foreign_keys_off);
            super::review::review(&p, &tr("Apply the Differences?"), &tr("Tables only in this database are dropped with their rows."), &text, &tr("Apply"), true, move || {
                super::review::run_batch(session, statements, fk_off, move |r| match r {
                    Ok(_) => {
                        c.toast(&tr("Structure changed"));
                        c.schema_changed();
                    }
                    Err(e) => c.toast(&e),
                });
            });
        }
        _ => {}
    });
    dialog.present(Some(parent));
}
