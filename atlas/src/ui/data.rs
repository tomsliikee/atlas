//! The panel that floats over the canvas on the right: a table's rows, or
//! the SQL console.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use atlas_core::guard::{self, Refusal};
use atlas_core::sql::{self, Filter, Op, Sort};
use atlas_core::store::{self, History};
use gtk::prelude::*;
use gtk::{gio, glib};
use sourceview5::prelude::*;

use super::Ctx;
use super::grid::{Grid, Row};
use crate::i18n::{ntr, tr};
use crate::runtime;

const PAGE: u64 = 200;

type NameFn = Box<dyn Fn(&str)>;
const CONSOLE_ROWS: usize = 1000;

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
    filter_bar: gtk::Revealer,
    filter_rows: gtk::Box,
    filters: RefCell<Vec<FilterRow>>,
    grid: Grid,
    detail: gtk::Revealer,
    detail_list: gtk::ListBox,
    browse: RefCell<Browse>,
    /// Bumped on every new query, so late answers to old ones are dropped.
    generation: Cell<u64>,
    console_buffer: sourceview5::Buffer,
    console_grid: Grid,
    console_status: gtk::Label,
    history: RefCell<History>,
    history_list: gtk::ListBox,
    navigate: RefCell<Option<NameFn>>,
    close: RefCell<Option<Box<dyn Fn()>>>,
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
        copy_menu.append(Some(&tr("Copy as CSV")), Some("panel.copy-csv"));
        copy_menu.append(Some(&tr("Copy as SQL INSERT")), Some("panel.copy-sql"));
        let copy = gtk::MenuButton::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(tr("Copy rows (selected, or all loaded)"))
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
        let header = gtk::Box::builder().spacing(4).css_classes(["panel-header"]).build();
        header.append(&titles);
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
        let data_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        data_page.append(&filter_bar);
        data_page.append(&body);

        // Console.
        let language = {
            // The default manager lives as long as the process; a local one
            // would be gone before the highlighting engine asks it.
            let manager = sourceview5::LanguageManager::default();
            let mut paths: Vec<String> = manager.search_path().iter().map(|s| s.to_string()).collect();
            paths.insert(0, crate::data_dir().join("sourceview/language-specs").to_string_lossy().into_owned());
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            manager.set_search_path(&refs);
            manager.language("atlas-sql")
        };
        let console_buffer = sourceview5::Buffer::new(None);
        if let Some(l) = &language {
            console_buffer.set_language(Some(l));
        }
        console_buffer.set_highlight_matching_brackets(false);
        let editor = sourceview5::View::builder()
            .buffer(&console_buffer)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(14)
            .right_margin(14)
            .css_classes(["console"])
            .build();
        let editor_scroll = gtk::ScrolledWindow::builder().child(&editor).min_content_height(120).vexpand(true).build();
        let run = gtk::Button::builder().label(tr("Run")).tooltip_text("Ctrl+Enter").css_classes(["solid", "small"]).build();
        let history_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["history"]).build();
        let history_scroll = gtk::ScrolledWindow::builder()
            .child(&history_list)
            .max_content_height(360)
            .propagate_natural_height(true)
            .min_content_width(420)
            .build();
        let history_pop = gtk::Popover::builder().child(&history_scroll).build();
        let history = gtk::MenuButton::builder()
            .icon_name("document-open-recent-symbolic")
            .tooltip_text(tr("History"))
            .popover(&history_pop)
            .css_classes(["flat", "circular"])
            .build();
        let console_status = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["panel-subtitle"]).build();
        let console_bar = gtk::Box::builder().spacing(8).css_classes(["console-bar"]).build();
        console_bar.append(&console_status);
        console_bar.append(&history);
        console_bar.append(&run);
        let console_grid = Grid::new();
        let console_top = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        console_top.append(&editor_scroll);
        console_top.append(&console_bar);
        let console_page = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&console_top)
            .end_child(&console_grid.widget)
            .resize_start_child(false)
            .shrink_start_child(false)
            .position(220)
            .build();

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
        stack.add_named(&console_page, Some("sql"));
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
            filter_bar,
            filter_rows,
            filters: RefCell::default(),
            grid,
            detail,
            detail_list,
            browse: RefCell::default(),
            generation: Cell::new(0),
            console_buffer,
            console_grid,
            console_status,
            history: RefCell::default(),
            history_list,
            navigate: RefCell::default(),
            close: RefCell::default(),
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
        run.connect_clicked(move |_| {
            if let Some(p) = p.upgrade() {
                p.run_console();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let p = Rc::downgrade(&panel);
        keys.connect_key_pressed(move |_, key, _, mods| {
            if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) && matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                if let Some(p) = p.upgrade() {
                    p.run_console();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        editor.add_controller(keys);
        let p = Rc::downgrade(&panel);
        history_pop.connect_show(move |_| {
            if let Some(p) = p.upgrade() {
                p.fill_history();
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
        panel.widget.insert_action_group("panel", Some(&actions));

        let p = Rc::downgrade(&panel);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(p) = p.upgrade() {
                p.apply_scheme();
            }
        });
        panel.apply_scheme();
        panel
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

    fn apply_scheme(&self) {
        let manager = sourceview5::StyleSchemeManager::default();
        let dir = crate::data_dir().join("sourceview/styles");
        manager.prepend_search_path(&dir.to_string_lossy());
        let id = if adw::StyleManager::default().is_dark() { "atlas-dark" } else { "atlas-light" };
        if let Some(scheme) = manager.scheme(id) {
            self.console_buffer.set_style_scheme(Some(&scheme));
        }
    }

    /// Shows the rows of `table`, optionally filtered.
    pub fn open_table(self: &Rc<Self>, table: &str, filters: Vec<Filter>) {
        if self.ctx.session.borrow().is_none() {
            self.title.set_text(table);
            self.subtitle.set_text(&tr("Structure only"));
            self.stack.set_visible_child_name("none");
            return;
        }
        self.stack.set_visible_child_name("data");
        self.filter_button.set_visible(true);
        self.detail_button.set_visible(true);
        self.title.set_text(table);
        self.subtitle.set_text(&tr("Loading…"));
        // The table first: the filter rows list its columns.
        *self.browse.borrow_mut() = Browse { table: table.to_owned(), filters: filters.clone(), ..Default::default() };
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
        let key = self.ctx.key.borrow().clone();
        if let Some(k) = key {
            *self.history.borrow_mut() = store::load(&super::config_dir().join("history").join(store::key_file(&k)));
        }
        self.title.set_text(&tr("SQL"));
        let read_only = tr("Read-only");
        self.subtitle.set_text(&match self.ctx.session.borrow().as_ref() {
            Some(s) => format!("{} · {read_only}", s.name),
            None => String::new(),
        });
        self.filter_button.set_visible(false);
        self.detail_button.set_visible(false);
        if self.ctx.session.borrow().is_none() {
            self.stack.set_visible_child_name("none");
            return;
        }
        self.stack.set_visible_child_name("sql");
    }

    pub fn focus_editor(&self) {
        if let Some(page) = self.stack.child_by_name("sql") {
            // Paned → Box → ScrolledWindow → View
            if let Some(view) = page
                .first_child()
                .and_then(|b| b.first_child())
                .and_then(|s| s.first_child())
                .and_then(|v| v.downcast::<sourceview5::View>().ok())
            {
                view.grab_focus();
            }
        }
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
        let Some(row) = row else {
            self.detail.set_reveal_child(false);
            return;
        };
        let columns = self.grid.columns();
        for (name, value) in columns.iter().zip(row.iter()) {
            let label = gtk::Label::builder().label(name).xalign(0.0).css_classes(["detail-name"]).build();
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
            let item = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
            item.append(&label);
            item.append(&v);
            self.detail_list.append(&item);
        }
        self.detail.set_reveal_child(true);
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
            (&self.console_grid, "query".to_owned())
        } else {
            (&self.grid, self.browse.borrow().table.clone())
        };
        let rows = grid.chosen_rows();
        let columns = grid.columns();
        let dialect = self.ctx.session.borrow().as_ref().map_or(sql::Dialect::MySql, |s| s.dialect);
        let text = if as_sql { sql::inserts(dialect, &table, &columns, &rows) } else { sql::csv(&columns, &rows) };
        self.widget.clipboard().set_text(&text);
        self.ctx.toast(&ntr("Copied {} row", "Copied {} rows", rows.len() as u32).replace("{}", &rows.len().to_string()));
    }

    pub fn set_console_text(&self, sql: &str) {
        self.console_buffer.set_text(sql);
    }

    pub fn run_console(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let (start, end) = self.console_buffer.bounds();
        let sql = self.console_buffer.text(&start, &end, false).to_string();
        if let Err(refusal) = guard::check(session.dialect, &sql) {
            let text = match refusal {
                Refusal::Empty => return,
                Refusal::Several => tr("One statement at a time."),
                Refusal::Writes(what) => tr("Atlas only reads. This statement would change the database ({what}).").replace("{what}", &what),
                Refusal::Unparsed(e) => tr("Atlas could not tell whether this only reads, so it was not sent: {e}").replace("{e}", &e),
            };
            self.console_status.set_text(&text);
            return;
        }
        {
            let mut h = self.history.borrow_mut();
            h.push(&sql);
            if let Some(k) = self.ctx.key.borrow().as_ref() {
                let _ = store::save(&super::config_dir().join("history").join(store::key_file(k)), &*h);
            }
        }
        self.console_status.set_text(&tr("Running…"));
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let p = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let started = Instant::now();
            let result = runtime::run(async move { session.query(sql, CONSOLE_ROWS).await }).await;
            let ms = started.elapsed().as_millis();
            let Some(p) = p.upgrade() else { return };
            match result {
                Ok(set) => {
                    let n = set.rows.len();
                    let mut status = ntr("{} row", "{} rows", n as u32).replace("{}", &group(n as u64));
                    if set.truncated {
                        status = tr("first {} rows").replace("{}", &group(n as u64));
                    }
                    p.console_status.set_text(&format!("{status} · {ms} ms"));
                    p.console_grid.set(&set.columns, set.rows, &[], false);
                }
                Err(e) => p.console_status.set_text(&e),
            }
        });
    }

    fn fill_history(self: &Rc<Self>) {
        while let Some(child) = self.history_list.first_child() {
            self.history_list.remove(&child);
        }
        let queries = self.history.borrow().queries.clone();
        if queries.is_empty() {
            self.history_list.append(&gtk::Label::builder().label(tr("Nothing run yet")).css_classes(["hint"]).margin_top(12).margin_bottom(12).build());
        }
        for q in queries {
            let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
            let button = gtk::Button::builder()
                .child(&gtk::Label::builder().label(&one_line).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(60).css_classes(["mono"]).build())
                .css_classes(["flat"])
                .build();
            let buffer = self.console_buffer.clone();
            button.connect_clicked(move |b| {
                buffer.set_text(&q);
                if let Some(pop) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                    pop.popdown();
                }
            });
            self.history_list.append(&button);
        }
    }
}

/// 1234567 → "1,234,567" (or "1.234.567" in German), by the locale's rule.
fn group(n: u64) -> String {
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
