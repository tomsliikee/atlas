//! The table editor: columns, indexes and foreign keys of one table, new or
//! existing. "Review" turns the edit into SQL (`atlas_core::migrate`), shows
//! it, and runs it in one transaction where the database allows that.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use atlas_core::migrate::{self, TableEdit};
use atlas_core::schema::{Column, FkAction, Index, Relation, Table};
use gtk::glib;

use super::Ctx;
use crate::i18n::tr;

struct ColumnRow {
    widget: gtk::Box,
    /// The name before the edit; None for a new column.
    original: Option<String>,
    name: gtk::Entry,
    data_type: gtk::Entry,
    nullable: gtk::CheckButton,
    primary: gtk::CheckButton,
    default: gtk::Entry,
    auto: gtk::CheckButton,
    base: Column,
}

struct IndexRow {
    widget: gtk::Box,
    name: gtk::Entry,
    columns: gtk::Entry,
    unique: gtk::CheckButton,
    kind: String,
}

struct FkRow {
    widget: gtk::Box,
    name: gtk::Entry,
    columns: gtk::Entry,
    target: gtk::DropDown,
    target_columns: gtk::Entry,
    on_delete: gtk::DropDown,
    on_update: gtk::DropDown,
}

/// The edited table, its renamed columns (old, new) and its foreign keys.
type Edited = (Table, Vec<(String, String)>, Vec<Relation>);

struct Editor {
    ctx: Rc<Ctx>,
    old: Option<Table>,
    old_fks: Vec<Relation>,
    tables: Vec<String>,
    name: adw::EntryRow,
    columns: RefCell<Vec<ColumnRow>>,
    column_box: gtk::Box,
    indexes: RefCell<Vec<IndexRow>>,
    index_box: gtk::Box,
    fks: RefCell<Vec<FkRow>>,
    fk_box: gtk::Box,
}

fn entry(text: &str, placeholder: &str, width: i32) -> gtk::Entry {
    gtk::Entry::builder().text(text).placeholder_text(placeholder).width_chars(width).build()
}

fn remove_button() -> gtk::Button {
    gtk::Button::builder().icon_name("list-remove-symbolic").tooltip_text(tr("Remove")).css_classes(["flat", "circular"]).build()
}

fn split(text: &str) -> Vec<String> {
    text.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()
}

fn actions_model() -> gtk::StringList {
    let mut names = vec![tr("(default)")];
    names.extend(FkAction::ALL.iter().map(|a| a.sql().to_owned()));
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

fn action_of(d: &gtk::DropDown) -> Option<FkAction> {
    (d.selected() as usize).checked_sub(1).and_then(|i| FkAction::ALL.get(i).copied())
}

fn select_action(d: &gtk::DropDown, a: Option<FkAction>) {
    d.set_selected(a.and_then(|a| FkAction::ALL.iter().position(|x| *x == a)).map_or(0, |i| i as u32 + 1));
}

fn section(title: &str, content: &gtk::Box, add_label: &str) -> (gtk::Box, gtk::Button) {
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    outer.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["section-title"]).build());
    outer.append(content);
    let add = gtk::Button::builder().label(add_label).halign(gtk::Align::Start).css_classes(["quiet", "small"]).build();
    outer.append(&add);
    (outer, add)
}

/// Opens the editor for `table`, or for a new table when None.
pub fn open(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>, table: Option<&str>) {
    let Some(schema) = ctx.schema.borrow().clone() else { return };
    let old = table.and_then(|t| schema.table(t)).cloned();
    let old_fks = old.as_ref().map(|t| migrate::fks_of(&schema.relations, &t.name)).unwrap_or_default();
    let tables: Vec<String> = schema.tables.iter().map(|t| t.name.clone()).collect();
    let name = adw::EntryRow::builder().title(tr("Table name")).text(old.as_ref().map_or("", |t| t.name.as_str())).build();
    let editor = Rc::new(Editor {
        ctx: ctx.clone(),
        old: old.clone(),
        old_fks: old_fks.clone(),
        tables,
        name,
        columns: RefCell::default(),
        column_box: gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build(),
        indexes: RefCell::default(),
        index_box: gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build(),
        fks: RefCell::default(),
        fk_box: gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build(),
    });
    match &old {
        Some(t) => {
            for c in &t.columns {
                editor.add_column(Some(c.clone()));
            }
            for i in &t.indexes {
                editor.add_index(Some(i));
            }
            for r in &old_fks {
                editor.add_fk(Some(r));
            }
        }
        None => {
            let mut id = Column { name: "id".into(), data_type: "integer".into(), primary: true, auto_increment: true, ..Default::default() };
            id.nullable = false;
            editor.add_column(Some(id));
        }
    }

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let name_group = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    name_group.append(&editor.name);
    content.append(&name_group);
    let head = gtk::Label::builder()
        .label(tr("name · type · NULL · key · auto · default"))
        .xalign(0.0)
        .css_classes(["hint"])
        .build();
    let (cols, add_col) = section(&tr("Columns"), &editor.column_box, &tr("Add column"));
    cols.insert_child_after(&head, cols.first_child().as_ref());
    content.append(&cols);
    let (idx, add_idx) = section(&tr("Indexes"), &editor.index_box, &tr("Add index"));
    content.append(&idx);
    let (fks, add_fk) = section(&tr("Foreign keys"), &editor.fk_box, &tr("Add foreign key"));
    content.append(&fks);

    let e = Rc::downgrade(&editor);
    add_col.connect_clicked(move |_| {
        if let Some(e) = e.upgrade() {
            e.add_column(None);
        }
    });
    let e = Rc::downgrade(&editor);
    add_idx.connect_clicked(move |_| {
        if let Some(e) = e.upgrade() {
            e.add_index(None);
        }
    });
    let e = Rc::downgrade(&editor);
    add_fk.connect_clicked(move |_| {
        if let Some(e) = e.upgrade() {
            e.add_fk(None);
        }
    });

    let scroll = gtk::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let review = gtk::Button::builder().label(tr("Review…")).css_classes(["suggested-action"]).build();
    let cancel = gtk::Button::builder().label(tr("Cancel")).build();
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&review);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroll));
    let dialog = adw::Dialog::builder()
        .title(if old.is_some() { tr("Edit Table") } else { tr("New Table") })
        .content_width(820)
        .content_height(680)
        .child(&toolbar)
        .build();
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let e = editor.clone();
    review.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            e.review(&dialog);
        }
    ));
    dialog.present(Some(parent));
}

impl Editor {
    fn add_column(self: &Rc<Self>, c: Option<Column>) {
        let base = c.clone().unwrap_or(Column { nullable: true, data_type: "varchar(255)".into(), ..Default::default() });
        let row = ColumnRow {
            widget: gtk::Box::builder().spacing(6).build(),
            original: c.as_ref().map(|c| c.name.clone()),
            name: entry(&base.name, &tr("name"), 14),
            data_type: entry(&base.data_type, &tr("type"), 14),
            nullable: gtk::CheckButton::builder().active(base.accepts_null()).tooltip_text(tr("May be NULL")).build(),
            primary: gtk::CheckButton::builder().active(base.primary).tooltip_text(tr("Primary key")).build(),
            auto: gtk::CheckButton::builder().active(base.auto_increment).tooltip_text(tr("Numbered by the database")).build(),
            default: entry(base.default.as_deref().unwrap_or(""), &tr("default"), 12),
            base,
        };
        row.name.set_hexpand(true);
        for w in [row.name.upcast_ref::<gtk::Widget>(), row.data_type.upcast_ref(), row.nullable.upcast_ref(), row.primary.upcast_ref(), row.auto.upcast_ref(), row.default.upcast_ref()] {
            row.widget.append(w);
        }
        let remove = remove_button();
        row.widget.append(&remove);
        let (e, w) = (Rc::downgrade(self), row.widget.clone());
        remove.connect_clicked(move |_| {
            if let Some(e) = e.upgrade() {
                e.column_box.remove(&w);
                e.columns.borrow_mut().retain(|r| r.widget != w);
            }
        });
        self.column_box.append(&row.widget);
        self.columns.borrow_mut().push(row);
    }

    fn add_index(self: &Rc<Self>, i: Option<&Index>) {
        let row = IndexRow {
            widget: gtk::Box::builder().spacing(6).build(),
            name: entry(i.map_or("", |i| i.name.as_str()), &tr("name"), 16),
            columns: entry(&i.map(|i| i.columns.join(", ")).unwrap_or_default(), &tr("columns, comma separated"), 24),
            unique: gtk::CheckButton::builder().label(tr("unique")).active(i.is_some_and(|i| i.unique)).build(),
            kind: i.map(|i| i.kind.clone()).unwrap_or_default(),
        };
        row.columns.set_hexpand(true);
        row.widget.append(&row.name);
        row.widget.append(&row.columns);
        row.widget.append(&row.unique);
        let remove = remove_button();
        row.widget.append(&remove);
        let (e, w) = (Rc::downgrade(self), row.widget.clone());
        remove.connect_clicked(move |_| {
            if let Some(e) = e.upgrade() {
                e.index_box.remove(&w);
                e.indexes.borrow_mut().retain(|r| r.widget != w);
            }
        });
        self.index_box.append(&row.widget);
        self.indexes.borrow_mut().push(row);
    }

    fn add_fk(self: &Rc<Self>, r: Option<&Relation>) {
        let refs: Vec<&str> = self.tables.iter().map(String::as_str).collect();
        let target = gtk::DropDown::from_strings(&refs);
        if let Some(i) = r.and_then(|r| self.tables.iter().position(|t| *t == r.to_table)) {
            target.set_selected(i as u32);
        }
        let on_delete = gtk::DropDown::builder().model(&actions_model()).tooltip_text(tr("On delete")).build();
        let on_update = gtk::DropDown::builder().model(&actions_model()).tooltip_text(tr("On update")).build();
        select_action(&on_delete, r.and_then(|r| r.on_delete));
        select_action(&on_update, r.and_then(|r| r.on_update));
        let row = FkRow {
            widget: gtk::Box::builder().spacing(6).build(),
            name: entry(r.and_then(|r| r.name.as_deref()).unwrap_or(""), &tr("name"), 12),
            columns: entry(&r.map(|r| r.from_columns.join(", ")).unwrap_or_default(), &tr("columns"), 12),
            target,
            target_columns: entry(&r.map(|r| r.to_columns.join(", ")).unwrap_or_else(|| "id".into()), &tr("target columns"), 10),
            on_delete,
            on_update,
        };
        row.columns.set_hexpand(true);
        for w in [
            row.name.upcast_ref::<gtk::Widget>(),
            row.columns.upcast_ref(),
            &gtk::Label::new(Some("→")).upcast(),
            row.target.upcast_ref(),
            row.target_columns.upcast_ref(),
            row.on_delete.upcast_ref(),
            row.on_update.upcast_ref(),
        ] {
            row.widget.append(w);
        }
        let remove = remove_button();
        row.widget.append(&remove);
        let (e, w) = (Rc::downgrade(self), row.widget.clone());
        remove.connect_clicked(move |_| {
            if let Some(e) = e.upgrade() {
                e.fk_box.remove(&w);
                e.fks.borrow_mut().retain(|r| r.widget != w);
            }
        });
        self.fk_box.append(&row.widget);
        self.fks.borrow_mut().push(row);
    }

    /// The table as the form describes it, and the renamed columns.
    fn result(&self) -> Result<Edited, String> {
        let name = self.name.text().trim().to_owned();
        if name.is_empty() {
            return Err(tr("The table needs a name."));
        }
        let mut table = self.old.clone().unwrap_or_default();
        table.name = name.clone();
        table.columns.clear();
        let mut renames = Vec::new();
        for r in self.columns.borrow().iter() {
            let n = r.name.text().trim().to_owned();
            if n.is_empty() {
                return Err(tr("Every column needs a name."));
            }
            if table.columns.iter().any(|c| c.name == n) {
                return Err(tr("Two columns are called {name}.").replace("{name}", &n));
            }
            let default = r.default.text().trim().to_owned();
            let mut c = r.base.clone();
            c.name = n.clone();
            c.data_type = r.data_type.text().trim().to_owned();
            c.primary = r.primary.is_active();
            c.nullable = r.nullable.is_active() && !c.primary;
            c.auto_increment = r.auto.is_active();
            c.default = (!default.is_empty()).then_some(default);
            if c.data_type.is_empty() {
                return Err(tr("Column {name} needs a type.").replace("{name}", &n));
            }
            if let Some(o) = &r.original
                && *o != n
            {
                renames.push((o.clone(), n.clone()));
            }
            table.columns.push(c);
        }
        if table.columns.is_empty() {
            return Err(tr("A table needs at least one column."));
        }
        // Unique sets that lost a column go with it.
        let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let renamed = |c: &String| renames.iter().find(|(o, _)| o == c).map_or(c.clone(), |(_, n)| n.clone());
        table.unique = table.unique.iter().map(|s| s.iter().map(renamed).collect::<Vec<_>>()).filter(|s: &Vec<String>| s.iter().all(|c| names.contains(c))).collect();
        table.indexes = self
            .indexes
            .borrow()
            .iter()
            .map(|r| Index { name: r.name.text().trim().to_owned(), columns: split(&r.columns.text()), unique: r.unique.is_active(), kind: r.kind.clone() })
            .filter(|i| !i.columns.is_empty())
            .map(|mut i| {
                if i.name.is_empty() {
                    i.name = format!("{name}_{}_idx", i.columns.join("_"));
                }
                i
            })
            .collect();
        let fks = self
            .fks
            .borrow()
            .iter()
            .filter_map(|r| {
                let to_table = self.tables.get(r.target.selected() as usize)?.clone();
                let n = r.name.text().trim().to_owned();
                Some(Relation {
                    name: (!n.is_empty()).then_some(n),
                    from_table: name.clone(),
                    from_columns: split(&r.columns.text()),
                    to_table,
                    to_columns: split(&r.target_columns.text()),
                    on_delete: action_of(&r.on_delete),
                    on_update: action_of(&r.on_update),
                    ..Default::default()
                })
            })
            .filter(|r| !r.from_columns.is_empty() && r.from_columns.len() == r.to_columns.len())
            .collect();
        Ok((table, renames, fks))
    }

    fn review(self: &Rc<Self>, dialog: &adw::Dialog) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let (table, renames, fks) = match self.result() {
            Ok(r) => r,
            Err(e) => {
                super::review::failed(dialog, &tr("Not yet"), &e);
                return;
            }
        };
        let m = match &self.old {
            Some(old) => migrate::alter_table(
                session.dialect,
                &TableEdit { old, new: &table, renamed_columns: &renames, old_fks: &self.old_fks, new_fks: &fks },
            ),
            None => migrate::create(session.dialect, &table, &fks),
        };
        if m.is_empty() {
            dialog.close();
            return;
        }
        let drops = self.old.as_ref().map_or(0, |o| {
            o.columns.iter().filter(|c| !table.columns.iter().any(|n| n.name == c.name || renames.iter().any(|(a, b)| *a == c.name && *b == n.name))).count()
        });
        let body = if m.foreign_keys_off {
            tr("SQLite cannot change this in place: the table is rebuilt and its rows copied, in one transaction.")
        } else if session.dialect == atlas_core::sql::Dialect::MySql {
            tr("MariaDB commits each structure change by itself; if one fails, the ones before it stay.")
        } else {
            tr("All statements go in one transaction.")
        };
        let (ctx, weak) = (self.ctx.clone(), dialog.downgrade());
        let statements = m.statements.clone();
        let fk_off = m.foreign_keys_off;
        super::review::review(dialog, &tr("Change the Table?"), &body, &m.text(), &tr("Apply"), drops > 0, move || {
            super::review::run_batch(session, statements, fk_off, move |result| match result {
                Ok(_) => {
                    if let Some(d) = weak.upgrade() {
                        d.close();
                    }
                    ctx.toast(&tr("Table changed"));
                    ctx.schema_changed();
                }
                Err(e) => {
                    if let Some(d) = weak.upgrade() {
                        super::review::failed(&d, &tr("The table was not changed"), &e);
                    }
                }
            });
        });
    }
}

/// Asks, then drops `table`.
pub fn drop_table(parent: &impl IsA<gtk::Widget>, ctx: Rc<Ctx>, table: &str) {
    let Some(session) = ctx.session.borrow().clone() else { return };
    let Some(t) = ctx.schema.borrow().as_ref().and_then(|s| s.table(table)).cloned() else { return };
    let m = migrate::drop(session.dialect, &t);
    let body = tr("{name} and all its rows are deleted. This cannot be undone.").replace("{name}", table);
    super::review::review(parent, &tr("Drop Table?"), &body, &m.text(), &tr("Drop"), true, move || {
        let c = ctx.clone();
        super::review::run_batch(session, m.statements, false, move |result| match result {
            Ok(_) => {
                c.toast(&tr("Table dropped"));
                c.schema_changed();
            }
            Err(e) => c.toast(&e),
        });
    });
}
