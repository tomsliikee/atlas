//! The panel that floats over the canvas on the right: a table's rows or
//! structure, or the SQL console.
//!
//! On a session opened for editing, the rows can be changed in the detail
//! pane beside the grid. Changes collect until the user reviews and commits
//! them; then they go in one transaction.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use atlas_core::edit::{self, Changes, Values};
use atlas_core::sql::{self, Filter, Op, Sort};
use gtk::{gio, glib};

use super::Ctx;
use super::console::Console;
use super::grid::{CellState, Grid, Row};
use crate::i18n::{ntr, tr};
use crate::runtime;

const PAGE: u64 = 200;

type NameFn = Box<dyn Fn(&str)>;
type TableFn = Box<dyn Fn(&str)>;

#[derive(Default)]
struct Browse {
    table: String,
    filters: Vec<Filter>,
    sort: Option<Sort>,
    loaded: u64,
    done: bool,
    loading: bool,
    total: Option<u64>,
}

/// A handle for widgets that record edits.
#[derive(Clone)]
struct Edits {
    changes: std::rc::Weak<RefCell<Option<Changes>>>,
    grid: Grid,
    bar: gtk::Revealer,
    label: gtk::Label,
}

impl Edits {
    fn change(&self, f: impl FnOnce(&mut Changes)) {
        let Some(changes) = self.changes.upgrade() else { return };
        let n = {
            let mut c = changes.borrow_mut();
            let Some(c) = c.as_mut() else { return };
            f(c);
            c.count()
        };
        self.grid.refresh();
        self.bar.set_reveal_child(n > 0);
        self.label.set_text(&ntr("{} change", "{} changes", n as u32).replace("{}", &n.to_string()));
    }
}

/// A dump has no dialect of its own; the types usually tell.
fn guess_dialect(schema: &atlas_core::schema::Schema) -> sql::Dialect {
    let types: String = schema.tables.iter().flat_map(|t| &t.columns).map(|c| c.data_type.to_ascii_lowercase()).collect::<Vec<_>>().join(" ");
    if types.contains("unsigned") || types.contains("tinyint") || types.contains("enum(") || types.contains("datetime") {
        sql::Dialect::MySql
    } else if types.contains("character varying") || types.contains("timestamp with") || types.contains("jsonb") || types.contains("serial") {
        sql::Dialect::Postgres
    } else {
        sql::Dialect::Sqlite
    }
}

struct FilterRow {
    widget: gtk::Box,
    column: gtk::DropDown,
    op: gtk::DropDown,
    value: gtk::Entry,
}

pub struct DataPanel {
    pub widget: gtk::Box,
    ctx: Rc<Ctx>,
    stack: gtk::Stack,
    title: gtk::Label,
    subtitle: gtk::Label,
    filter_button: gtk::ToggleButton,
    detail_button: gtk::ToggleButton,
    view_switch: gtk::Box,
    rows_button: gtk::ToggleButton,
    structure_button: gtk::ToggleButton,
    add_button: gtk::Button,
    delete_button: gtk::Button,
    edit_bar: gtk::Revealer,
    edit_label: gtk::Label,
    /// Pending edits of the open table; None when it cannot be edited.
    changes: Rc<RefCell<Option<Changes>>>,
    /// The row shown in the detail pane.
    detail_row: RefCell<Option<Row>>,
    filter_bar: gtk::Revealer,
    filter_rows: gtk::Box,
    filters: RefCell<Vec<FilterRow>>,
    grid: Grid,
    detail: gtk::Revealer,
    detail_list: gtk::ListBox,
    browse: RefCell<Browse>,
    /// Bumped on every new query, so late answers to old ones are dropped.
    generation: Cell<u64>,
    pub console: Rc<Console>,
    navigate: RefCell<Option<NameFn>>,
    close: RefCell<Option<Box<dyn Fn()>>>,
    edit_table: RefCell<Option<TableFn>>,
    drop_table: RefCell<Option<TableFn>>,
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).css_classes(["flat", "circular"]).build()
}

fn op_label(op: Op) -> String {
    match op {
        Op::Contains => tr("contains"),
        Op::Equals => "=".into(),
        Op::NotEquals => "≠".into(),
        Op::Less => "<".into(),
        Op::Greater => ">".into(),
        Op::IsNull => tr("is NULL"),
        Op::IsNotNull => tr("is not NULL"),
    }
}

impl DataPanel {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["panel"]).build();

        // Header: title, count, actions.
        let title = gtk::Label::builder().xalign(0.0).css_classes(["panel-title"]).ellipsize(gtk::pango::EllipsizeMode::End).build();
        let subtitle = gtk::Label::builder().xalign(0.0).css_classes(["panel-subtitle"]).build();
        let titles = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).valign(gtk::Align::Center).build();
        titles.append(&title);
        titles.append(&subtitle);
        let filter_button = gtk::ToggleButton::builder()
            .icon_name("atlas-filter-symbolic")
            .tooltip_text(tr("Filter"))
            .css_classes(["flat", "circular"])
            .build();
        let copy_menu = gio::Menu::new();
        let copy_section = gio::Menu::new();
        copy_section.append(Some(&tr("Copy as CSV")), Some("panel.copy-csv"));
        copy_section.append(Some(&tr("Copy as SQL INSERT")), Some("panel.copy-sql"));
        copy_menu.append_section(None, &copy_section);
        let save_section = gio::Menu::new();
        save_section.append(Some(&tr("Save as CSV…")), Some("panel.save-csv"));
        save_section.append(Some(&tr("Save as JSON…")), Some("panel.save-json"));
        save_section.append(Some(&tr("Save as SQL…")), Some("panel.save-sql"));
        copy_menu.append_section(None, &save_section);
        let copy = gtk::MenuButton::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(tr("Copy or save rows"))
            .menu_model(&copy_menu)
            .css_classes(["flat", "circular"])
            .build();
        let detail_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(tr("Row details"))
            .active(true)
            .css_classes(["flat", "circular"])
            .build();
        let close = icon_button("window-close-symbolic", &tr("Close (Esc)"));
        let rows_button = gtk::ToggleButton::builder().label(tr("Rows")).active(true).css_classes(["small"]).build();
        let structure_button = gtk::ToggleButton::builder().label(tr("Structure")).group(&rows_button).css_classes(["small"]).build();
        let view_switch = gtk::Box::builder().css_classes(["linked"]).valign(gtk::Align::Center).margin_end(6).build();
        view_switch.append(&rows_button);
        view_switch.append(&structure_button);
        let add_button = icon_button("list-add-symbolic", &tr("New row"));
        let delete_button = icon_button("user-trash-symbolic", &tr("Delete selected rows"));
        let header = gtk::Box::builder().spacing(4).css_classes(["panel-header"]).build();
        header.append(&titles);
        header.append(&view_switch);
        header.append(&add_button);
        header.append(&delete_button);
        header.append(&filter_button);
        header.append(&copy);
        header.append(&detail_button);
        header.append(&close);
        widget.append(&header);

        // Filters.
        let filter_rows = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        let add = gtk::Button::builder().label(tr("Add filter")).css_classes(["quiet", "small"]).build();
        let apply = gtk::Button::builder().label(tr("Apply")).css_classes(["solid", "small"]).build();
        let filter_actions = gtk::Box::builder().spacing(8).build();
        filter_actions.append(&add);
        filter_actions.append(&apply);
        let filter_inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).css_classes(["filters"]).build();
        filter_inner.append(&filter_rows);
        filter_inner.append(&filter_actions);
        let filter_bar = gtk::Revealer::builder()
            .child(&filter_inner)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(crate::motion::STATE_MS)
            .build();

        // Rows and details.
        let grid = Grid::new();
        let detail_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["detail"]).build();
        let detail_scroll = gtk::ScrolledWindow::builder()
            .child(&detail_list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(280)
            .build();
        let detail = gtk::Revealer::builder()
            .child(&detail_scroll)
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .transition_duration(crate::motion::LAYOUT_MS)
            .reveal_child(false)
            .build();
        let body = gtk::Box::builder().vexpand(true).build();
        body.append(&grid.widget);
        body.append(&detail);
        let edit_label = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["panel-subtitle"]).build();
        let discard = gtk::Button::builder().label(tr("Discard")).css_classes(["quiet", "small"]).build();
        let review = gtk::Button::builder().label(tr("Review…")).css_classes(["quiet", "small"]).build();
        let commit = gtk::Button::builder().label(tr("Commit")).css_classes(["suggested-action", "small"]).build();
        let edit_inner = gtk::Box::builder().spacing(8).css_classes(["edit-bar"]).build();
        edit_inner.append(&edit_label);
        edit_inner.append(&discard);
        edit_inner.append(&review);
        edit_inner.append(&commit);
        let edit_bar = gtk::Revealer::builder()
            .child(&edit_inner)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(crate::motion::STATE_MS)
            .build();
        let data_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        data_page.append(&filter_bar);
        data_page.append(&body);
        data_page.append(&edit_bar);

        // Console. The search path first: the console's buffers ask the
        // default manager for the language.
        {
            // The default manager lives as long as the process; a local one
            // would be gone before the highlighting engine asks it.
            let manager = sourceview5::LanguageManager::default();
            let mut paths: Vec<String> = manager.search_path().iter().map(|s| s.to_string()).collect();
            paths.insert(0, crate::data_dir().join("sourceview/language-specs").to_string_lossy().into_owned());
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            manager.set_search_path(&refs);
        }
        let console = Console::new(ctx.clone());

        // A dump: structure only.
        let empty = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .valign(gtk::Align::Center)
            .margin_start(32)
            .margin_end(32)
            .build();
        empty.append(&gtk::Label::builder().label(tr("No rows in a dump")).css_classes(["statement-small"]).build());
        empty.append(
            &gtk::Label::builder()
                .label(tr("A dump file is read for its structure only. Connect to the database to browse its data."))
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["hint"])
                .build(),
        );

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(crate::motion::STATE_MS)
            .vexpand(true)
            .build();
        stack.add_named(&data_page, Some("data"));
        stack.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("structure"));
        stack.add_named(&console.widget, Some("sql"));
        stack.add_named(&empty, Some("none"));
        widget.append(&stack);

        let panel = Rc::new(DataPanel {
            widget,
            ctx,
            stack,
            title,
            subtitle,
            filter_button: filter_button.clone(),
            detail_button: detail_button.clone(),
            view_switch,
            rows_button: rows_button.clone(),
            structure_button: structure_button.clone(),
            add_button: add_button.clone(),
            delete_button: delete_button.clone(),
            edit_bar,
            edit_label,
            changes: Rc::default(),
            detail_row: RefCell::default(),
            filter_bar,
            filter_rows,
            filters: RefCell::default(),
            grid,
            detail,
            detail_list,
            browse: RefCell::default(),
            generation: Cell::new(0),
            console,
            navigate: RefCell::default(),
            close: RefCell::default(),
            edit_table: RefCell::default(),
            drop_table: RefCell::default(),
        });

        let p = Rc::downgrade(&panel);
        filter_button.connect_toggled(move |b| {
            let Some(p) = p.upgrade() else { return };
            p.filter_bar.set_reveal_child(b.is_active());
            if b.is_active() && p.filters.borrow().is_empty() {
                p.add_filter(None);
            }
        });
        let p = Rc::downgrade(&panel);
        add.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.add_filter(None);
            }
        });
        let p = Rc::downgrade(&panel);
        apply.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.apply_filters();
            }
        });
        let p = Rc::downgrade(&panel);
        detail_button.connect_toggled(move |b| {
            if let Some(p) = p.upgrade() {
                p.detail.set_visible(b.is_active());
            }
        });
        let p = Rc::downgrade(&panel);
        close.connect_clicked(move |_| {
            if let Some(p) = p.upgrade()
                && let Some(f) = p.close.borrow().as_ref()
            {
                f();
            }
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_sort(move |sort| {
            if let Some(p) = p.upgrade() {
                p.browse.borrow_mut().sort = sort;
                p.reload();
            }
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_more(move || {
            if let Some(p) = p.upgrade() {
                p.load_page();
            }
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_selected(move |row| {
            if let Some(p) = p.upgrade() {
                p.show_detail(row);
            }
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_link(move |column, value| {
            if let Some(p) = p.upgrade() {
                p.follow(column, value);
            }
        });
        let p = Rc::downgrade(&panel);
        structure_button.connect_toggled(move |b| {
            if let Some(p) = p.upgrade()
                && b.is_active()
            {
                p.show_structure();
            }
        });
        let p = Rc::downgrade(&panel);
        rows_button.connect_toggled(move |b| {
            if let Some(p) = p.upgrade()
                && b.is_active()
                && p.stack.visible_child_name().as_deref() == Some("structure")
            {
                p.stack.set_visible_child_name("data");
                p.update_tools();
            }
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_state(move |row, column| match p.upgrade() {
            Some(p) => p.cell_state(row, column),
            None => CellState::Plain,
        });
        let p = Rc::downgrade(&panel);
        panel.grid.on_activate(move |row, column| {
            if let Some(p) = p.upgrade() {
                p.grid.select(&row);
                p.detail_button.set_active(true);
                p.focus_field(column);
            }
        });
        let p = Rc::downgrade(&panel);
        add_button.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.new_row();
            }
        });
        let p = Rc::downgrade(&panel);
        delete_button.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.delete_selected();
            }
        });
        let p = Rc::downgrade(&panel);
        discard.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.discard();
            }
        });
        let p = Rc::downgrade(&panel);
        review.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.commit(true);
            }
        });
        let p = Rc::downgrade(&panel);
        commit.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.commit(false);
            }
        });

        let actions = gio::SimpleActionGroup::new();
        for (name, as_sql) in [("copy-csv", false), ("copy-sql", true)] {
            let action = gio::SimpleAction::new(name, None);
            let p = Rc::downgrade(&panel);
            action.connect_activate(move |_, _| {
                if let Some(p) = p.upgrade() {
                    p.copy(as_sql);
                }
            });
            actions.add_action(&action);
        }
        for (name, format) in [
            ("save-csv", super::export::Rows::Csv),
            ("save-json", super::export::Rows::Json),
            ("save-sql", super::export::Rows::Sql),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let p = Rc::downgrade(&panel);
            action.connect_activate(move |_, _| {
                if let Some(p) = p.upgrade() {
                    p.save_rows(format);
                }
            });
            actions.add_action(&action);
        }
        panel.widget.insert_action_group("panel", Some(&actions));

        panel
    }

    pub fn on_edit_table(&self, f: impl Fn(&str) + 'static) {
        *self.edit_table.borrow_mut() = Some(Box::new(f));
    }

    pub fn on_drop_table(&self, f: impl Fn(&str) + 'static) {
        *self.drop_table.borrow_mut() = Some(Box::new(f));
    }

    pub fn on_navigate(&self, f: impl Fn(&str) + 'static) {
        *self.navigate.borrow_mut() = Some(Box::new(f));
    }

    pub fn on_close(&self, f: impl Fn() + 'static) {
        *self.close.borrow_mut() = Some(Box::new(f));
    }

    pub fn mode(&self) -> Option<glib::GString> {
        self.stack.visible_child_name()
    }

    pub fn current_table(&self) -> String {
        self.browse.borrow().table.clone()
    }

    /// Shows the rows of `table`, optionally filtered.
    pub fn open_table(self: &Rc<Self>, table: &str, filters: Vec<Filter>) {
        if self.ctx.session.borrow().is_none() {
            self.title.set_text(table);
            self.subtitle.set_text(&tr("Structure only"));
            *self.browse.borrow_mut() = Browse { table: table.to_owned(), ..Default::default() };
            *self.changes.borrow_mut() = None;
            self.view_switch.set_visible(true);
            self.structure_button.set_active(true);
            self.show_structure();
            return;
        }
        self.stack.set_visible_child_name("data");
        self.view_switch.set_visible(true);
        self.rows_button.set_active(true);
        self.title.set_text(table);
        self.subtitle.set_text(&tr("Loading…"));
        // The table first: the filter rows list its columns.
        *self.browse.borrow_mut() = Browse { table: table.to_owned(), filters: filters.clone(), ..Default::default() };
        let key = if self.ctx.writable() {
            self.ctx.schema.borrow().as_ref().and_then(|s| s.table(table)).and_then(|t| {
                (t.kind == atlas_core::schema::TableKind::Table).then(|| t.row_key()).flatten()
            })
        } else {
            None
        };
        *self.changes.borrow_mut() = key.map(Changes::new);
        self.update_tools();
        self.update_edit_bar();
        while let Some(child) = self.filter_rows.first_child() {
            self.filter_rows.remove(&child);
        }
        self.filters.borrow_mut().clear();
        for f in &filters {
            self.add_filter(Some(f));
        }
        self.filter_button.set_active(!filters.is_empty());
        self.filter_bar.set_reveal_child(!filters.is_empty());
        let (columns, links) = self.columns_of(table);
        self.grid.set(&columns, Vec::new(), &links, true);
        self.grid.clear_sort();
        self.show_detail(None);
        self.reload();
    }

    pub fn open_console(self: &Rc<Self>) {
        self.title.set_text(&tr("SQL"));
        self.subtitle.set_text(&self.ctx.session.borrow().as_ref().map(|s| s.name.clone()).unwrap_or_default());
        self.view_switch.set_visible(false);
        self.filter_button.set_visible(false);
        self.detail_button.set_visible(false);
        self.add_button.set_visible(false);
        self.delete_button.set_visible(false);
        if self.ctx.session.borrow().is_none() {
            self.stack.set_visible_child_name("none");
            return;
        }
        self.console.open();
        self.stack.set_visible_child_name("sql");
    }

    pub fn focus_editor(&self) {
        self.console.focus();
    }

    /// Which header tools fit what is shown.
    fn update_tools(&self) {
        let rows = self.stack.visible_child_name().as_deref() == Some("data");
        let editable = rows && self.changes.borrow().is_some();
        self.filter_button.set_visible(rows);
        self.detail_button.set_visible(rows);
        self.add_button.set_visible(editable);
        self.delete_button.set_visible(editable);
    }

    fn show_structure(self: &Rc<Self>) {
        let table = self.browse.borrow().table.clone();
        let Some(schema) = self.ctx.schema.borrow().clone() else { return };
        let dialect = self.ctx.session.borrow().as_ref().map_or_else(|| guess_dialect(&schema), |s| s.dialect);
        let actions = self.ctx.writable().then(|| {
            let (a, b) = (Rc::downgrade(self), Rc::downgrade(self));
            let (t1, t2) = (table.clone(), table.clone());
            super::structure::Actions {
                edit: Box::new(move || {
                    if let Some(p) = a.upgrade()
                        && let Some(f) = p.edit_table.borrow().as_ref()
                    {
                        f(&t1);
                    }
                }),
                drop: Box::new(move || {
                    if let Some(p) = b.upgrade()
                        && let Some(f) = p.drop_table.borrow().as_ref()
                    {
                        f(&t2);
                    }
                }),
            }
        });
        let page = super::structure::build(&schema, &table, dialect, actions);
        if let Some(old) = self.stack.child_by_name("structure") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("structure"));
        self.stack.set_visible_child_name("structure");
        self.update_tools();
    }

    /// For scripted screenshots: one pending change on the first row.
    pub fn demo_edit(&self, column: &str, value: &str) {
        let Some(row) = self.grid.first_row() else { return };
        let columns = self.grid.columns();
        let Some(i) = columns.iter().position(|c| c == column) else { return };
        let key = self.changes.borrow().as_ref().and_then(|c| edit::key_of(&c.key_columns, &columns, &row));
        let Some(key) = key else { return };
        let original = row.get(i).cloned().flatten();
        self.edits().change(|c| c.set(key, column, Some(value.to_owned()), original.as_deref()));
        self.grid.select(&row);
    }

    pub fn explain_console(&self) {
        self.console.explain();
    }

    /// Switches to the structure of the open table.
    pub fn show_structure_now(&self) {
        self.structure_button.set_active(true);
    }

    /// The structure page again, after the schema was reloaded.
    pub fn refresh_structure(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() == Some("structure") {
            self.show_structure();
        }
    }

    fn cell_state(&self, row: &Row, column: usize) -> CellState {
        let changes = self.changes.borrow();
        let Some(c) = changes.as_ref() else { return CellState::Plain };
        let columns = self.grid.columns();
        let Some(key) = edit::key_of(&c.key_columns, &columns, row) else { return CellState::Plain };
        if c.is_deleted(&key) {
            return CellState::Deleted;
        }
        match columns.get(column).and_then(|name| c.pending(&key, name)) {
            Some(v) => CellState::Changed(v.clone()),
            None => CellState::Plain,
        }
    }

    fn update_edit_bar(&self) {
        let n = self.changes.borrow().as_ref().map_or(0, Changes::count);
        self.edit_bar.set_reveal_child(n > 0);
        self.edit_label.set_text(&ntr("{} change", "{} changes", n as u32).replace("{}", &n.to_string()));
    }

    fn after_edit(&self) {
        self.grid.refresh();
        self.update_edit_bar();
    }

    fn focus_field(&self, column: usize) {
        let mut child = self.detail_list.first_child();
        let mut i = 0;
        while let Some(c) = child {
            if i == column {
                if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>()
                    && let Some(item) = row.child()
                {
                    // name label, then the field
                    if let Some(field) = item.first_child().and_then(|l| l.next_sibling()) {
                        field.grab_focus();
                    }
                }
                return;
            }
            i += 1;
            child = c.next_sibling();
        }
    }

    fn new_row(self: &Rc<Self>) {
        let table = self.browse.borrow().table.clone();
        let Some(t) = self.ctx.schema.borrow().as_ref().and_then(|s| s.table(&table)).cloned() else { return };
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder().description(tr("Leave a field empty for its default.")).build();
        let mut fields = Vec::new();
        for c in &t.columns {
            if c.generated.is_some() {
                continue;
            }
            let mut title = c.name.clone();
            if !c.nullable && c.default.is_none() && !c.auto_increment {
                title.push_str(" *");
            }
            let row = adw::EntryRow::builder().title(&title).build();
            row.set_tooltip_text(Some(&c.data_type));
            group.add(&row);
            fields.push((c.name.clone(), row));
        }
        page.add(&group);
        let dialog = adw::Dialog::builder().title(tr("New Row")).content_width(460).content_height(560).build();
        let add = gtk::Button::builder().label(tr("Add")).css_classes(["suggested-action"]).build();
        let cancel = gtk::Button::builder().label(tr("Cancel")).build();
        let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
        header.pack_start(&cancel);
        header.pack_end(&add);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        dialog.set_child(Some(&toolbar));
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        let p = Rc::downgrade(self);
        add.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                let mut values = Values::new();
                for (name, row) in &fields {
                    let text = row.text().to_string();
                    if !text.is_empty() {
                        values.insert(name.clone(), if text == "NULL" { None } else { Some(text) });
                    }
                }
                if let Some(p) = p.upgrade() {
                    if let Some(c) = p.changes.borrow_mut().as_mut() {
                        c.insert(values);
                    }
                    p.after_edit();
                }
                dialog.close();
            }
        ));
        dialog.present(Some(&self.widget));
    }

    fn delete_selected(&self) {
        let columns = self.grid.columns();
        let rows = self.grid.selected();
        if rows.is_empty() {
            self.ctx.toast(&tr("Select the rows to delete first."));
            return;
        }
        if let Some(c) = self.changes.borrow_mut().as_mut() {
            for r in &rows {
                if let Some(k) = edit::key_of(&c.key_columns, &columns, r) {
                    if c.is_deleted(&k) {
                        c.undelete(&k);
                    } else {
                        c.delete(k);
                    }
                }
            }
        }
        self.after_edit();
    }

    fn discard(&self) {
        if let Some(c) = self.changes.borrow_mut().as_mut() {
            c.clear();
        }
        let row = self.detail_row.borrow().clone();
        self.show_detail(row);
        self.after_edit();
    }

    /// Shows the pending SQL; commits right away unless `review_only`.
    fn commit(self: &Rc<Self>, review_only: bool) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let table = self.browse.borrow().table.clone();
        let statements = match self.changes.borrow().as_ref() {
            Some(c) if !c.is_empty() => c.to_sql(session.dialect, &table),
            _ => return,
        };
        let deletes = self.changes.borrow().as_ref().map_or(0, |c| c.deletes.len());
        let p = Rc::downgrade(self);
        let go = move || {
            let Some(p) = p.upgrade() else { return };
            let p2 = Rc::downgrade(&p);
            super::review::run_batch(session, statements, false, move |result| {
                let Some(p) = p2.upgrade() else { return };
                match result {
                    Ok(n) => {
                        if let Some(c) = p.changes.borrow_mut().as_mut() {
                            c.clear();
                        }
                        p.ctx.toast(&ntr("{} row changed", "{} rows changed", n as u32).replace("{}", &n.to_string()));
                        p.update_edit_bar();
                        p.reload();
                    }
                    Err(e) => super::review::failed(&p.widget, &tr("Nothing was changed"), &e),
                }
            });
        };
        let sql = super::review::script(&self.changes.borrow().as_ref().map(|c| c.to_sql(self.ctx.session.borrow().as_ref().map_or(sql::Dialect::MySql, |s| s.dialect), &table)).unwrap_or_default());
        if review_only || deletes > 0 {
            let body = if deletes > 0 {
                ntr("This deletes {} row. All changes go in one transaction.", "This deletes {} rows. All changes go in one transaction.", deletes as u32)
                    .replace("{}", &deletes.to_string())
            } else {
                tr("All changes go in one transaction.")
            };
            super::review::review(&self.widget, &tr("Commit Changes?"), &body, &sql, &tr("Commit"), deletes > 0, go);
        } else {
            go();
        }
    }

    /// The rows of the open table (all of them, page by page) or of the
    /// console result, written to a file.
    fn save_rows(self: &Rc<Self>, format: super::export::Rows) {
        let dialect = self.ctx.session.borrow().as_ref().map_or(sql::Dialect::MySql, |s| s.dialect);
        if self.stack.visible_child_name().as_deref() == Some("sql") {
            let Some(grid) = self.console.current_grid() else { return };
            super::export::save_rows(&self.widget, &self.ctx, "query", format, dialect, super::export::Source::Loaded(grid.columns(), grid.chosen_rows()));
            return;
        }
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let b = self.browse.borrow();
        let types = self
            .ctx
            .schema
            .borrow()
            .as_ref()
            .and_then(|s| s.table(&b.table))
            .map(|t| t.columns.iter().map(|c| c.data_type.clone()).collect())
            .unwrap_or_default();
        super::export::save_rows(
            &self.widget,
            &self.ctx,
            &b.table,
            format,
            dialect,
            super::export::Source::Table { session, table: b.table.clone(), filters: b.filters.clone(), sort: b.sort.clone(), types },
        );
    }

    fn columns_of(&self, table: &str) -> (Vec<String>, Vec<usize>) {
        let schema = self.ctx.schema.borrow();
        let Some(t) = schema.as_ref().and_then(|s| s.table(table)) else { return (Vec::new(), Vec::new()) };
        let s = schema.as_ref().unwrap_or_else(|| unreachable!());
        let columns: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
        let links = columns.iter().enumerate().filter(|(_, c)| s.is_foreign(table, c, true)).map(|(i, _)| i).collect();
        (columns, links)
    }

    fn add_filter(self: &Rc<Self>, preset: Option<&Filter>) {
        let (columns, _) = self.columns_of(&self.browse.borrow().table);
        let names: Vec<&str> = columns.iter().map(String::as_str).collect();
        let column = gtk::DropDown::from_strings(&names);
        let ops: Vec<String> = Op::ALL.iter().map(|o| op_label(*o)).collect();
        let op_refs: Vec<&str> = ops.iter().map(String::as_str).collect();
        let op = gtk::DropDown::from_strings(&op_refs);
        let value = gtk::Entry::builder().hexpand(true).placeholder_text(tr("Value")).build();
        let remove = icon_button("list-remove-symbolic", &tr("Remove filter"));
        if let Some(f) = preset {
            if let Some(i) = columns.iter().position(|c| *c == f.column) {
                column.set_selected(i as u32);
            }
            if let Some(i) = Op::ALL.iter().position(|o| *o == f.op) {
                op.set_selected(i as u32);
            }
            value.set_text(&f.value);
        }
        op.connect_selected_notify(glib::clone!(
            #[weak]
            value,
            move |op| value.set_sensitive(Op::ALL.get(op.selected() as usize).is_some_and(|o| o.takes_value()))
        ));
        let row = gtk::Box::builder().spacing(6).build();
        row.append(&column);
        row.append(&op);
        row.append(&value);
        row.append(&remove);
        self.filter_rows.append(&row);
        let p = Rc::downgrade(self);
        value.connect_activate(move |_| {
            if let Some(p) = p.upgrade() {
                p.apply_filters();
            }
        });
        let p = Rc::downgrade(self);
        remove.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| {
                let Some(p) = p.upgrade() else { return };
                p.filter_rows.remove(&row);
                p.filters.borrow_mut().retain(|f| f.widget != row);
                p.apply_filters();
            }
        ));
        if preset.is_none() {
            value.grab_focus();
        }
        self.filters.borrow_mut().push(FilterRow { widget: row, column, op, value });
    }

    fn apply_filters(self: &Rc<Self>) {
        let (columns, _) = self.columns_of(&self.browse.borrow().table);
        let filters: Vec<Filter> = self
            .filters
            .borrow()
            .iter()
            .filter_map(|f| {
                let column = columns.get(f.column.selected() as usize)?.clone();
                let op = *Op::ALL.get(f.op.selected() as usize)?;
                let value = f.value.text().to_string();
                (!op.takes_value() || !value.is_empty()).then_some(Filter { column, op, value })
            })
            .collect();
        self.browse.borrow_mut().filters = filters;
        self.reload();
    }

    fn reload(self: &Rc<Self>) {
        {
            let mut b = self.browse.borrow_mut();
            b.loaded = 0;
            b.done = false;
            b.loading = false;
            b.total = None;
        }
        self.generation.set(self.generation.get() + 1);
        self.grid.replace(Vec::new());
        self.load_page();
        self.count();
    }

    fn load_page(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let (sql, generation) = {
            let mut b = self.browse.borrow_mut();
            if b.done || b.loading || b.table.is_empty() {
                return;
            }
            b.loading = true;
            (sql::page(session.dialect, &b.table, &b.filters, b.sort.as_ref(), PAGE, b.loaded), self.generation.get())
        };
        let p = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = runtime::run(async move { session.query(sql, PAGE as usize).await }).await;
            let Some(p) = p.upgrade() else { return };
            if p.generation.get() != generation {
                return;
            }
            let mut b = p.browse.borrow_mut();
            b.loading = false;
            match result {
                Ok(set) => {
                    let n = set.rows.len() as u64;
                    b.loaded += n;
                    b.done = n < PAGE;
                    drop(b);
                    p.grid.append(set.rows);
                    p.update_subtitle();
                }
                Err(e) => {
                    b.done = true;
                    drop(b);
                    p.subtitle.set_text(&tr("Could not read the rows"));
                    p.ctx.toast(&e);
                }
            }
        });
    }

    fn count(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let sql = {
            let b = self.browse.borrow();
            sql::count(session.dialect, &b.table, &b.filters)
        };
        let generation = self.generation.get();
        let p = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = runtime::run(async move { session.scalar(sql).await }).await;
            let Some(p) = p.upgrade() else { return };
            if p.generation.get() != generation {
                return;
            }
            if let Ok(Some(n)) = result {
                p.browse.borrow_mut().total = n.parse().ok();
                p.update_subtitle();
            }
        });
    }

    fn update_subtitle(&self) {
        let b = self.browse.borrow();
        let loaded = b.loaded;
        let text = match b.total {
            Some(total) if total > loaded => tr("{loaded} of {total} rows").replace("{loaded}", &group(loaded)).replace("{total}", &group(total)),
            Some(total) => ntr("{} row", "{} rows", total.min(u32::MAX as u64) as u32).replace("{}", &group(total)),
            None => ntr("{} row", "{} rows", loaded.min(u32::MAX as u64) as u32).replace("{}", &group(loaded)),
        };
        let filtered = if b.filters.is_empty() { String::new() } else { format!(" · {}", tr("filtered")) };
        self.subtitle.set_text(&format!("{text}{filtered}"));
    }

    fn show_detail(&self, row: Option<Row>) {
        while let Some(child) = self.detail_list.first_child() {
            self.detail_list.remove(&child);
        }
        *self.detail_row.borrow_mut() = row.clone();
        let Some(row) = row else {
            self.detail.set_reveal_child(false);
            return;
        };
        let columns = self.grid.columns();
        let table = self.browse.borrow().table.clone();
        let (key, deleted) = {
            let changes = self.changes.borrow();
            let key = changes.as_ref().and_then(|c| edit::key_of(&c.key_columns, &columns, &row));
            let deleted = changes.as_ref().zip(key.as_ref()).is_some_and(|(c, k)| c.is_deleted(k));
            (key, deleted)
        };
        let schema_table = self.ctx.schema.borrow().as_ref().and_then(|s| s.table(&table)).cloned();
        for (i, (name, value)) in columns.iter().zip(row.iter()).enumerate() {
            let label = gtk::Label::builder().label(name).xalign(0.0).css_classes(["detail-name"]).build();
            let item = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
            item.append(&label);
            let column = schema_table.as_ref().and_then(|t| t.column(name));
            let editable = key.is_some()
                && !deleted
                && column.is_some_and(|c| c.generated.is_none() && !sql::is_binary_type(&c.data_type));
            if let (true, Some(key)) = (editable, key.clone()) {
                let pending = self.changes.borrow().as_ref().and_then(|c| c.pending(&key, name).cloned());
                let current = pending.clone().unwrap_or_else(|| value.clone());
                item.append(&self.field(i, name, key, value.clone(), current, column.and_then(|c| c.enum_values.clone()), column.is_some_and(|c| c.accepts_null())));
            } else {
                let text = match value {
                    Some(v) => pretty_json(v).unwrap_or_else(|| v.clone()),
                    None => "NULL".into(),
                };
                let shown: String = text.chars().take(20_000).collect();
                let v = gtk::Label::builder()
                    .label(&shown)
                    .xalign(0.0)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .css_classes(if value.is_some() { vec!["detail-value"] } else { vec!["detail-value", "null"] })
                    .build();
                item.append(&v);
            }
            self.detail_list.append(&item);
        }
        if deleted {
            let keep = gtk::Button::builder().label(tr("Keep this row")).halign(gtk::Align::Start).css_classes(["quiet", "small"]).build();
            let edits = self.edits();
            keep.connect_clicked(move |b| {
                if let Some(k) = &key {
                    edits.change(|c| c.undelete(k));
                }
                b.set_sensitive(false);
            });
            self.detail_list.append(&keep);
        }
        self.detail.set_reveal_child(true);
    }

    /// What field widgets need to record an edit, without holding the panel.
    fn edits(&self) -> Edits {
        Edits {
            changes: Rc::downgrade(&self.changes),
            grid: self.grid.clone(),
            bar: self.edit_bar.clone(),
            label: self.edit_label.clone(),
        }
    }

    /// An editable field for one cell of the detail pane.
    #[allow(clippy::too_many_arguments)]
    fn field(
        &self,
        _index: usize,
        name: &str,
        key: edit::Key,
        original: Option<String>,
        current: Option<String>,
        enum_values: Option<Vec<String>>,
        nullable: bool,
    ) -> gtk::Widget {
        let edits = self.edits();
        let row = gtk::Box::builder().spacing(4).build();
        let null = gtk::ToggleButton::builder()
            .label("NULL")
            .active(current.is_none())
            .visible(nullable)
            .tooltip_text(tr("Set to NULL"))
            .css_classes(["small", "flat"])
            .valign(gtk::Align::Start)
            .build();
        let name = name.to_owned();
        let set = {
            let (edits, name, key, original) = (edits.clone(), name.clone(), key.clone(), original.clone());
            Rc::new(move |value: Option<String>| {
                edits.change(|c| c.set(key.clone(), &name, value, original.as_deref()));
            })
        };
        let field: gtk::Widget = if let Some(values) = enum_values {
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            let drop = gtk::DropDown::from_strings(&refs);
            drop.set_hexpand(true);
            if let Some(i) = current.as_ref().and_then(|v| values.iter().position(|x| x == v)) {
                drop.set_selected(i as u32);
            } else {
                drop.set_selected(gtk::INVALID_LIST_POSITION);
            }
            let (s, n) = (set.clone(), null.clone());
            drop.connect_selected_notify(move |d| {
                if let Some(v) = values.get(d.selected() as usize) {
                    n.set_active(false);
                    s(Some(v.clone()));
                }
            });
            drop.upcast()
        } else {
            let text = current.clone().unwrap_or_default();
            if text.contains('\n') || text.chars().count() > 80 {
                let buffer = gtk::TextBuffer::new(None);
                buffer.set_text(&text);
                let view = gtk::TextView::builder()
                    .buffer(&buffer)
                    .wrap_mode(gtk::WrapMode::WordChar)
                    .monospace(true)
                    .hexpand(true)
                    .top_margin(4)
                    .bottom_margin(4)
                    .left_margin(6)
                    .right_margin(6)
                    .css_classes(["console"])
                    .build();
                let (s, n) = (set.clone(), null.clone());
                buffer.connect_changed(move |b| {
                    let (a, e) = b.bounds();
                    n.set_active(false);
                    s(Some(b.text(&a, &e, false).to_string()));
                });
                view.upcast()
            } else {
                let entry = gtk::Entry::builder().text(&text).hexpand(true).build();
                if current.is_none() {
                    entry.set_placeholder_text(Some("NULL"));
                }
                let (s, n) = (set.clone(), null.clone());
                entry.connect_changed(move |e| {
                    if n.is_active() && e.text().is_empty() {
                        return;
                    }
                    n.set_active(false);
                    s(Some(e.text().to_string()));
                });
                entry.upcast()
            }
        };
        let s = set.clone();
        let f = field.clone();
        null.connect_toggled(move |b| {
            if b.is_active() {
                s(None);
                if let Some(e) = f.downcast_ref::<gtk::Entry>() {
                    e.set_text("");
                    e.set_placeholder_text(Some("NULL"));
                }
            }
        });
        row.append(&field);
        row.append(&null);
        row.upcast()
    }

    /// A click on a foreign key value: the parent table, filtered to it.
    fn follow(self: &Rc<Self>, column: usize, value: String) {
        let table = self.browse.borrow().table.clone();
        let target = {
            let schema = self.ctx.schema.borrow();
            let Some(s) = schema.as_ref() else { return };
            let Some(name) = s.table(&table).and_then(|t| t.columns.get(column)).map(|c| c.name.clone()) else { return };
            s.relation_of(&table, &name, true).and_then(|r| {
                let pos = r.from_columns.iter().position(|c| *c == name)?;
                Some((r.to_table.clone(), r.to_columns.get(pos)?.clone()))
            })
        };
        let Some((to_table, to_column)) = target else { return };
        self.open_table(&to_table, vec![Filter { column: to_column, op: Op::Equals, value }]);
        if let Some(f) = self.navigate.borrow().as_ref() {
            f(&to_table);
        }
    }

    fn copy(&self, as_sql: bool) {
        let (grid, table) = if self.stack.visible_child_name().as_deref() == Some("sql") {
            match self.console.current_grid() {
                Some(g) => (g, "query".to_owned()),
                None => return,
            }
        } else {
            (self.grid.clone(), self.browse.borrow().table.clone())
        };
        let rows = grid.chosen_rows();
        let columns = grid.columns();
        let dialect = self.ctx.session.borrow().as_ref().map_or(sql::Dialect::MySql, |s| s.dialect);
        let text = if as_sql { sql::inserts(dialect, &table, &columns, &rows) } else { sql::csv(&columns, &rows) };
        self.widget.clipboard().set_text(&text);
        self.ctx.toast(&ntr("Copied {} row", "Copied {} rows", rows.len() as u32).replace("{}", &rows.len().to_string()));
    }

    pub fn set_console_text(&self, sql: &str) {
        self.console.set_text(sql);
    }

    pub fn run_console(&self) {
        self.console.run();
    }

    /// The rows again, after something else changed them.
    pub fn reload_rows(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() == Some("data") && !self.browse.borrow().table.is_empty() {
            self.reload();
        }
    }
}

/// 1234567 → "1,234,567" (or "1.234.567" in German), by the locale's rule.
pub fn group(n: u64) -> String {
    let sep = if tr("thousands-separator") == "thousands-separator" { "," } else { "." };
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(c);
    }
    out
}

fn pretty_json(v: &str) -> Option<String> {
    let t = v.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(v).ok()?;
    serde_json::to_string_pretty(&value).ok()
}
