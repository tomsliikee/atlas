//! Getting data in and out as files: CSV into a table, an SQL script into
//! the database, and Atlas' own backup (a plain SQL script). Imports run in
//! one transaction with a stop button; nothing is half written.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use atlas_core::sql::{self, Dialect};
use atlas_core::{backup, csv, dump};
use gtk::{gio, glib};

use super::Ctx;
use crate::db::{Progress, Session};
use crate::i18n::{ntr, tr};
use crate::runtime;

/// Rows per INSERT when importing.
const BATCH: usize = 200;

fn pick_file(parent: &impl IsA<gtk::Widget>, title: &str, patterns: &[&str], done: impl FnOnce(PathBuf) + 'static) {
    let Some(window) = parent.root().and_downcast::<gtk::Window>() else { return };
    let filter = gtk::FileFilter::new();
    for p in patterns {
        filter.add_pattern(p);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title(title).filters(&filters).modal(true).build();
    dialog.open(Some(&window), gio::Cancellable::NONE, move |r| {
        if let Ok(file) = r
            && let Some(path) = file.path()
        {
            done(path);
        }
    });
}

/// Polls `progress` into `label` until the dialog is gone.
fn watch(label: &gtk::Label, progress: &Progress, total: usize, unit: &'static str) {
    let (weak, p) = (label.downgrade(), progress.clone());
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let Some(label) = weak.upgrade() else { return glib::ControlFlow::Break };
        label.set_text(&format!("{} / {} {}", super::data::group(p.done() as u64), super::data::group(total as u64), tr(unit)));
        glib::ControlFlow::Continue
    });
}

/// Runs `statements` with a progress dialog; `done` gets the result.
fn run_with_progress(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    session: Session,
    statements: Vec<String>,
    unit: &'static str,
    done: impl FnOnce(Result<u64, String>) + 'static,
) {
    let progress = Progress::default();
    let (dialog, label) = super::review::progress_dialog(parent, heading, &progress);
    watch(&label, &progress, statements.len(), unit);
    let p = progress.clone();
    glib::spawn_future_local(async move {
        let result = runtime::run(async move { session.execute_all(statements, false, p).await }).await;
        dialog.set_can_close(true);
        dialog.force_close();
        done(result);
    });
}

// ---------------------------------------------------------------- CSV

/// Asks for a CSV file and opens the import dialog for it. `table` is
/// preselected when given.
pub fn csv_import(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>, table: Option<String>) {
    let p = parent.clone().upcast::<gtk::Widget>();
    pick_file(parent, &tr("Import CSV"), &["*.csv", "*.tsv", "*.txt"], move |path| match std::fs::read_to_string(&path) {
        Ok(text) => csv_dialog(&p, ctx, table, &path, text),
        Err(e) => super::review::failed(&p, &tr("Could not read the file"), &e.to_string()),
    });
}

fn csv_dialog(parent: &gtk::Widget, ctx: Rc<Ctx>, table: Option<String>, path: &std::path::Path, text: String) {
    let Some(schema) = ctx.schema.borrow().clone() else { return };
    let tables: Vec<String> =
        schema.tables.iter().filter(|t| t.kind == atlas_core::schema::TableKind::Table).map(|t| t.name.clone()).collect();
    if tables.is_empty() {
        return;
    }
    let separators = [',', ';', '\t'];
    let guessed = csv::guess_separator(&text);
    let text = Rc::new(text);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().title(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).build();
    let refs: Vec<&str> = tables.iter().map(String::as_str).collect();
    let target = adw::ComboRow::builder().title(tr("Into table")).model(&gtk::StringList::new(&refs)).build();
    if let Some(i) = table.and_then(|t| tables.iter().position(|x| *x == t)) {
        target.set_selected(i as u32);
    }
    let sep = adw::ComboRow::builder()
        .title(tr("Separator"))
        .model(&gtk::StringList::new(&[&tr("Comma"), &tr("Semicolon"), &tr("Tab")]))
        .selected(separators.iter().position(|s| *s == guessed).unwrap_or(0) as u32)
        .build();
    let header = adw::SwitchRow::builder().title(tr("First line holds the column names")).active(true).build();
    let empty_null = adw::SwitchRow::builder().title(tr("Empty fields are NULL")).active(true).build();
    for r in [target.upcast_ref::<gtk::Widget>(), sep.upcast_ref(), header.upcast_ref(), empty_null.upcast_ref()] {
        group.add(r);
    }
    page.add(&group);
    let mapping_group = adw::PreferencesGroup::builder().title(tr("Columns")).description(tr("Which table column each CSV column goes into.")).build();
    page.add(&mapping_group);
    let preview_group = adw::PreferencesGroup::builder().title(tr("First rows")).build();
    let preview = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).css_classes(["mono", "hint"]).build();
    preview_group.add(&preview);
    page.add(&preview_group);

    // (csv column title, chosen table column) per CSV column
    let mapping: Rc<RefCell<Vec<adw::ComboRow>>> = Rc::default();
    let rebuild = {
        let (text, mapping, mapping_group, preview) = (text.clone(), mapping.clone(), mapping_group.clone(), preview.clone());
        let (target, sep, header, tables, schema) = (target.clone(), sep.clone(), header.clone(), tables.clone(), schema.clone());
        Rc::new(move || {
            for r in mapping.borrow_mut().drain(..) {
                mapping_group.remove(&r);
            }
            let records = csv::parse(&text, separators[sep.selected() as usize % 3]);
            let Some(first) = records.first() else { return };
            let Some(t) = tables.get(target.selected() as usize).and_then(|n| schema.table(n)) else { return };
            let mut options = vec![tr("(skip)")];
            options.extend(t.columns.iter().map(|c| c.name.clone()));
            let refs: Vec<&str> = options.iter().map(String::as_str).collect();
            for (i, title) in first.iter().enumerate() {
                let name = if header.is_active() { title.clone() } else { format!("{} {}", tr("Column"), i + 1) };
                let row = adw::ComboRow::builder().title(&name).model(&gtk::StringList::new(&refs)).build();
                // Same name, or the same position when there is no header.
                let pick = if header.is_active() {
                    t.columns.iter().position(|c| c.name.eq_ignore_ascii_case(title.trim()))
                } else {
                    (i < t.columns.len()).then_some(i)
                };
                row.set_selected(pick.map_or(0, |p| p as u32 + 1));
                mapping_group.add(&row);
                mapping.borrow_mut().push(row);
            }
            let lines: Vec<String> = records.iter().take(6).map(|r| r.join(" | ")).collect();
            preview.set_text(&lines.join("\n"));
        })
    };
    rebuild();
    for r in [&target, &sep] {
        let f = rebuild.clone();
        r.connect_selected_notify(move |_| f());
    }
    let f = rebuild.clone();
    header.connect_active_notify(move |_| f());

    let import = gtk::Button::builder().label(tr("Import")).css_classes(["suggested-action"]).build();
    let cancel = gtk::Button::builder().label(tr("Cancel")).build();
    let bar = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    bar.pack_start(&cancel);
    bar.pack_end(&import);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&bar);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title(tr("Import CSV")).content_width(560).content_height(680).child(&toolbar).build();
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    import.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let Some(session) = ctx.session.borrow().clone() else { return };
            let Some(table) = tables.get(target.selected() as usize).cloned() else { return };
            let Some(t) = schema.table(&table) else { return };
            let columns: Vec<Option<String>> = mapping
                .borrow()
                .iter()
                .map(|r| (r.selected() as usize).checked_sub(1).and_then(|i| t.columns.get(i)).map(|c| c.name.clone()))
                .collect();
            let mut records = csv::parse(&text, separators[sep.selected() as usize % 3]);
            if header.is_active() && !records.is_empty() {
                records.remove(0);
            }
            let statements = csv_inserts(session.dialect, &table, &columns, &records, empty_null.is_active());
            if statements.is_empty() {
                return;
            }
            dialog.close();
            let n = records.len();
            let c = ctx.clone();
            let parent = dialog.clone();
            run_with_progress(&parent, &tr("Importing…"), session, statements, "statements", move |result| match result {
                Ok(_) => {
                    c.toast(&ntr("Imported {} row", "Imported {} rows", n as u32).replace("{}", &super::data::group(n as u64)));
                    c.schema_changed();
                }
                Err(e) => c.toast(&format!("{}: {e}", tr("Nothing was imported"))),
            });
        }
    ));
    dialog.present(Some(parent));
}

/// Batched INSERTs for the mapped columns. Values go through `literal`, so
/// MariaDB gets them as hex and nothing in the file can break out.
pub fn csv_inserts(dialect: Dialect, table: &str, columns: &[Option<String>], records: &[Vec<String>], empty_null: bool) -> Vec<String> {
    let used: Vec<(usize, &String)> = columns.iter().enumerate().filter_map(|(i, c)| c.as_ref().map(|c| (i, c))).collect();
    if used.is_empty() {
        return Vec::new();
    }
    let head = format!(
        "INSERT INTO {} ({}) VALUES ",
        sql::ident(dialect, table),
        used.iter().map(|(_, c)| sql::ident(dialect, c)).collect::<Vec<_>>().join(", ")
    );
    records
        .chunks(BATCH)
        .map(|chunk| {
            let tuples: Vec<String> = chunk
                .iter()
                .map(|r| {
                    let vals: Vec<String> = used
                        .iter()
                        .map(|(i, _)| match r.get(*i).map(String::as_str) {
                            None => "NULL".to_owned(),
                            Some("") if empty_null => "NULL".to_owned(),
                            Some(v) => sql::literal(dialect, v),
                        })
                        .collect();
                    format!("({})", vals.join(", "))
                })
                .collect();
            format!("{head}{}", tuples.join(", "))
        })
        .collect()
}

// ---------------------------------------------------------------- SQL

/// Asks for an SQL file and runs it, after saying how many statements it has.
pub fn sql_import(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>) {
    let p = parent.clone().upcast::<gtk::Widget>();
    pick_file(parent, &tr("Run SQL File"), &["*.sql", "*.txt"], move |path| {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return super::review::failed(&p, &tr("Could not read the file"), &e.to_string()),
        };
        let split = dump::split(&text);
        let n = split.statements.len();
        let mut body = ntr("The file has {} statement.", "The file has {} statements.", n as u32).replace("{}", &super::data::group(n as u64));
        if split.copy_blocks > 0 {
            body.push(' ');
            body.push_str(&tr("{n} COPY blocks from pg_dump cannot be sent as SQL and are left out.").replace("{n}", &split.copy_blocks.to_string()));
        }
        body.push(' ');
        body.push_str(&tr("They run in one transaction; on the first error nothing stays, except structure changes in MariaDB."));
        let preview: String = split.statements.iter().take(12).map(|s| format!("{};\n", s.lines().next().unwrap_or(""))).collect();
        let (p2, statements) = (p.clone(), split.statements);
        super::review::review(&p, &tr("Run SQL File?"), &body, &preview, &tr("Run"), true, move || {
            let Some(session) = ctx.session.borrow().clone() else { return };
            let c = ctx.clone();
            run_with_progress(&p2, &tr("Running…"), session, statements, "statements", move |result| match result {
                Ok(n) => {
                    c.toast(&ntr("{} row changed", "{} rows changed", n as u32).replace("{}", &super::data::group(n)));
                    c.schema_changed();
                }
                Err(e) => c.toast(&e),
            });
        });
    });
}

// ---------------------------------------------------------------- Backup

/// Writes the whole database as an SQL script.
pub fn backup(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>) {
    let (Some(session), Some(schema)) = (ctx.session.borrow().clone(), ctx.schema.borrow().clone()) else { return };
    let Some(window) = parent.root().and_downcast::<gtk::Window>() else { return };
    let date = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y-%m-%d").ok()).map(|s| s.to_string()).unwrap_or_default();
    let dialog = gtk::FileDialog::builder()
        .title(tr("Back Up Database"))
        .initial_name(format!("{}-{date}.sql", schema.name))
        .modal(true)
        .build();
    let p = parent.clone().upcast::<gtk::Widget>();
    dialog.save(Some(&window), gio::Cancellable::NONE, move |r| {
        let Ok(file) = r else { return };
        let Some(path) = file.path() else { return };
        let progress = Progress::default();
        let (dialog, label) = super::review::progress_dialog(&p, &tr("Backing up…"), &progress);
        let tables = backup::table_order(&schema).len();
        watch(&label, &progress, tables, "tables");
        let pr = progress.clone();
        glib::spawn_future_local(async move {
            let result = runtime::run(async move { write_backup(&session, &schema, &path, &date, &pr).await }).await;
            dialog.set_can_close(true);
            dialog.force_close();
            match result {
                Ok(rows) => ctx.toast(&ntr("Backed up {} row", "Backed up {} rows", rows as u32).replace("{}", &super::data::group(rows))),
                Err(e) => ctx.toast(&e),
            }
        });
    });
}

async fn write_backup(session: &Session, schema: &atlas_core::schema::Schema, path: &std::path::Path, when: &str, progress: &Progress) -> Result<u64, String> {
    use std::io::Write as _;
    let d = session.dialect;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut w = |s: &str| out.write_all(s.as_bytes()).map_err(|e| e.to_string());
    w(&backup::header(d, schema, when))?;
    w(&backup::structure(d, schema))?;
    let mut rows = 0u64;
    for (n, t) in backup::table_order(schema).into_iter().enumerate() {
        if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(tr("Stopped; the file is incomplete."));
        }
        // Ordered by key, so the file is the same for the same data.
        let sort = t.primary_key().first().map(|c| sql::Sort { column: (*c).to_owned(), descending: false });
        let mut offset = 0;
        loop {
            let q = sql::page(d, &t.name, &[], sort.as_ref(), backup::BATCH as u64, offset);
            let set = session.query(q, backup::BATCH).await?;
            let got = set.rows.len();
            w(&backup::rows(d, t, &set.columns, &set.rows))?;
            rows += got as u64;
            offset += got as u64;
            if got < backup::BATCH {
                break;
            }
        }
        progress.done.store(n + 1, std::sync::atomic::Ordering::Relaxed);
    }
    w(&backup::footer(d, schema))?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_rows_become_batched_inserts() {
        let cols = vec![Some("a".to_owned()), None, Some("b".to_owned())];
        let records = vec![vec!["1".into(), "x".into(), "".into()], vec!["it's".into(), "y".into(), "z".into()]];
        let s = csv_inserts(Dialect::Sqlite, "t", &cols, &records, true);
        assert_eq!(s, ["INSERT INTO \"t\" (\"a\", \"b\") VALUES ('1', NULL), ('it''s', 'z')"]);
    }
}
