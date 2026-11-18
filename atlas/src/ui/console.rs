//! The SQL console: tabs, each with its own editor and result grid, history,
//! saved queries, query plans and autocomplete.
//!
//! On a read-only session only reading statements are sent. On a session
//! opened for editing every statement may run; those that lose data are
//! confirmed first.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use atlas_core::guard::{self, Kind, Refusal};
use atlas_core::plan;
use atlas_core::sql::Dialect;
use atlas_core::store::{self, History, Saved, Tabs};
use gtk::glib;
use sourceview5::prelude::*;

use super::Ctx;
use super::grid::Grid;
use crate::db::Access;
use crate::i18n::{ntr, tr};
use crate::runtime;

const CONSOLE_ROWS: usize = 1000;

pub struct ConsoleTab {
    pub page: adw::TabPage,
    pub buffer: sourceview5::Buffer,
    pub view: sourceview5::View,
    pub grid: Grid,
    status: RefCell<String>,
}

pub struct Console {
    pub widget: gtk::Box,
    ctx: Rc<Ctx>,
    tab_view: adw::TabView,
    tabs: RefCell<Vec<Rc<ConsoleTab>>>,
    status: gtk::Label,
    history: RefCell<History>,
    history_list: gtk::ListBox,
    saved: RefCell<Saved>,
    saved_list: gtk::ListBox,
    generation: Cell<u64>,
    /// While tabs are rebuilt from the stored list, closing them is ours.
    loading: Cell<bool>,
}

fn file(kind: &str, key: &str) -> std::path::PathBuf {
    super::config_dir().join(kind).join(store::key_file(key))
}

fn list_popover(list: &gtk::ListBox) -> gtk::Popover {
    let scroll = gtk::ScrolledWindow::builder()
        .child(list)
        .max_content_height(360)
        .propagate_natural_height(true)
        .min_content_width(420)
        .build();
    gtk::Popover::builder().child(&scroll).build()
}

impl Console {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let tab_view = adw::TabView::builder().vexpand(true).build();
        let tab_bar = adw::TabBar::builder().view(&tab_view).autohide(false).build();
        let add = gtk::Button::builder().icon_name("list-add-symbolic").tooltip_text(tr("New tab")).css_classes(["flat"]).build();
        tab_bar.set_end_action_widget(Some(&add));

        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["panel-subtitle"])
            .build();
        let run = gtk::Button::builder().label(tr("Run")).tooltip_text("Ctrl+Enter").css_classes(["solid", "small"]).build();
        let explain = gtk::Button::builder().label(tr("Explain")).tooltip_text(tr("Show the query plan")).css_classes(["quiet", "small"]).build();
        let history_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["history"]).build();
        let history_pop = list_popover(&history_list);
        let history = gtk::MenuButton::builder()
            .icon_name("document-open-recent-symbolic")
            .tooltip_text(tr("History"))
            .popover(&history_pop)
            .css_classes(["flat", "circular"])
            .build();
        let saved_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["history"]).build();
        let saved_pop = list_popover(&saved_list);
        let saved = gtk::MenuButton::builder()
            .icon_name("starred-symbolic")
            .tooltip_text(tr("Saved queries"))
            .popover(&saved_pop)
            .css_classes(["flat", "circular"])
            .build();
        let bar = gtk::Box::builder().spacing(8).css_classes(["console-bar"]).build();
        bar.append(&status);
        bar.append(&saved);
        bar.append(&history);
        bar.append(&explain);
        bar.append(&run);

        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["console-tabs"]).build();
        widget.append(&tab_bar);
        widget.append(&bar);
        widget.append(&tab_view);

        let console = Rc::new(Console {
            widget,
            ctx,
            tab_view: tab_view.clone(),
            tabs: RefCell::default(),
            status,
            history: RefCell::default(),
            history_list,
            saved: RefCell::default(),
            saved_list,
            generation: Cell::new(0),
            loading: Cell::new(false),
        });

        let c = Rc::downgrade(&console);
        add.connect_clicked(move |_| {
            if let Some(c) = c.upgrade() {
                c.new_tab("");
                c.focus();
                c.save_tabs();
            }
        });
        let c = Rc::downgrade(&console);
        run.connect_clicked(move |_| {
            if let Some(c) = c.upgrade() {
                c.run();
            }
        });
        let c = Rc::downgrade(&console);
        explain.connect_clicked(move |_| {
            if let Some(c) = c.upgrade() {
                c.explain();
            }
        });
        let c = Rc::downgrade(&console);
        history_pop.connect_show(move |_| {
            if let Some(c) = c.upgrade() {
                c.fill_history();
            }
        });
        let c = Rc::downgrade(&console);
        saved_pop.connect_show(move |_| {
            if let Some(c) = c.upgrade() {
                c.fill_saved();
            }
        });
        let c = Rc::downgrade(&console);
        tab_view.connect_selected_page_notify(move |_| {
            if let Some(c) = c.upgrade()
                && let Some(t) = c.current()
            {
                c.status.set_text(&t.status.borrow());
                if !c.loading.get() {
                    c.save_tabs();
                }
            }
        });
        let c = Rc::downgrade(&console);
        tab_view.connect_close_page(move |view, page| {
            let Some(c) = c.upgrade() else { return glib::Propagation::Proceed };
            c.tabs.borrow_mut().retain(|t| t.page != *page);
            view.close_page_finish(page, true);
            if !c.loading.get() {
                if c.tabs.borrow().is_empty() {
                    c.new_tab("");
                }
                c.save_tabs();
            }
            glib::Propagation::Stop
        });
        console
    }

    /// Loads history, saved queries and the tabs of what is open.
    pub fn open(self: &Rc<Self>) {
        let key = self.ctx.key.borrow().clone();
        let tabs: Tabs = match &key {
            Some(k) => {
                *self.history.borrow_mut() = store::load(&file("history", k));
                *self.saved.borrow_mut() = store::load(&file("saved", k));
                store::load(&file("tabs", k))
            }
            None => Tabs::default(),
        };
        self.loading.set(true);
        while let Some(page) = self.tab_view.pages().item(0).and_downcast::<adw::TabPage>() {
            self.tab_view.close_page(&page);
        }
        self.tabs.borrow_mut().clear();
        if tabs.tabs.is_empty() {
            self.new_tab("");
        }
        for text in &tabs.tabs {
            self.new_tab(text);
        }
        if let Some(t) = self.tabs.borrow().get(tabs.selected) {
            self.tab_view.set_selected_page(&t.page);
        }
        self.loading.set(false);
        self.status.set_text(&self.subtitle());
    }

    fn subtitle(&self) -> String {
        match self.ctx.session.borrow().as_ref() {
            Some(s) if s.access == Access::ReadWrite => tr("Changes allowed"),
            Some(_) => tr("Read-only"),
            None => String::new(),
        }
    }

    fn new_tab(self: &Rc<Self>, text: &str) -> Rc<ConsoleTab> {
        let buffer = sourceview5::Buffer::new(None);
        if let Some(l) = sourceview5::LanguageManager::default().language("atlas-sql") {
            buffer.set_language(Some(&l));
        }
        buffer.set_highlight_matching_brackets(false);
        apply_scheme(&buffer);
        buffer.set_text(text);
        let view = sourceview5::View::builder()
            .buffer(&buffer)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(14)
            .right_margin(14)
            .css_classes(["console"])
            .build();
        super::complete::attach(&view, self.ctx.clone());
        let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(100).vexpand(true).build();
        let grid = Grid::new();
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&scroll)
            .end_child(&grid.widget)
            .resize_start_child(false)
            .shrink_start_child(false)
            .position(180)
            .build();
        let page = self.tab_view.append(&paned);
        page.set_title(&tab_title(text));
        let tab = Rc::new(ConsoleTab { page: page.clone(), buffer: buffer.clone(), view: view.clone(), grid, status: RefCell::default() });
        self.tabs.borrow_mut().push(tab.clone());
        self.tab_view.set_selected_page(&page);

        let keys = gtk::EventControllerKey::new();
        let c = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            if mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) && matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                if let Some(c) = c.upgrade() {
                    c.run();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.add_controller(keys);
        let (c, p) = (Rc::downgrade(self), page.downgrade());
        buffer.connect_changed(move |b| {
            let (s, e) = b.bounds();
            if let Some(p) = p.upgrade() {
                p.set_title(&tab_title(&b.text(&s, &e, false)));
            }
            if let Some(c) = c.upgrade()
                && !c.loading.get()
            {
                c.save_tabs();
            }
        });
        let weak = buffer.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(b) = weak.upgrade() {
                apply_scheme(&b);
            }
        });
        tab
    }

    fn save_tabs(&self) {
        let Some(k) = self.ctx.key.borrow().clone() else { return };
        let tabs = self.tabs.borrow();
        let texts: Vec<String> = tabs.iter().map(|t| text_of(&t.buffer)).collect();
        let selected = self
            .tab_view
            .selected_page()
            .and_then(|p| tabs.iter().position(|t| t.page == p))
            .unwrap_or(0);
        let _ = store::save(&file("tabs", &k), &Tabs { tabs: texts, selected });
    }

    pub fn current(&self) -> Option<Rc<ConsoleTab>> {
        let page = self.tab_view.selected_page()?;
        self.tabs.borrow().iter().find(|t| t.page == page).cloned()
    }

    pub fn focus(&self) {
        if let Some(t) = self.current() {
            t.view.grab_focus();
        }
    }

    pub fn set_text(&self, sql: &str) {
        if let Some(t) = self.current() {
            t.buffer.set_text(sql);
        }
    }

    pub fn current_grid(&self) -> Option<Grid> {
        self.current().map(|t| t.grid.clone())
    }

    fn set_status(&self, tab: &ConsoleTab, text: &str) {
        *tab.status.borrow_mut() = text.to_owned();
        if self.current().is_some_and(|c| c.page == tab.page) {
            self.status.set_text(text);
        }
    }

    fn remember(&self, sql: &str) {
        let mut h = self.history.borrow_mut();
        h.push(sql);
        if let Some(k) = self.ctx.key.borrow().as_ref() {
            let _ = store::save(&file("history", k), &*h);
        }
    }

    pub fn run(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let Some(tab) = self.current() else { return };
        let sql = text_of(&tab.buffer);
        let kind = match guard::check_for(session.dialect, &sql, session.access) {
            Ok(k) => k,
            Err(refusal) => {
                let text = match refusal {
                    Refusal::Empty => return,
                    Refusal::Several => tr("One statement at a time."),
                    Refusal::Writes(what) => tr("This connection is read-only. The statement would change the database ({what}).").replace("{what}", &what),
                    Refusal::Unparsed(e) => tr("Atlas could not tell whether this only reads, so it was not sent: {e}").replace("{e}", &e),
                };
                self.set_status(&tab, &text);
                return;
            }
        };
        match kind {
            Kind::Read => self.send_query(tab, sql),
            Kind::Write | Kind::Schema => self.send_change(tab, sql, matches!(kind, Kind::Schema)),
            Kind::Dangerous(what) => {
                let c = Rc::downgrade(self);
                let shown = sql.clone();
                super::review::review(
                    &self.widget,
                    &tr("Run this statement?"),
                    &tr("It cannot be undone: {what}.").replace("{what}", &what),
                    &shown,
                    &tr("Run"),
                    true,
                    move || {
                        if let Some(c) = c.upgrade() {
                            c.send_change(tab, sql, true);
                        }
                    },
                );
            }
        }
    }

    fn send_query(self: &Rc<Self>, tab: Rc<ConsoleTab>, sql: String) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        self.remember(&sql);
        self.set_status(&tab, &tr("Running…"));
        let generation = self.next_generation();
        let c = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let started = Instant::now();
            let result = runtime::run(async move { session.query(sql, CONSOLE_ROWS).await }).await;
            let ms = started.elapsed().as_millis();
            let Some(c) = c.upgrade() else { return };
            if c.generation.get() != generation {
                return;
            }
            match result {
                Ok(set) => {
                    let n = set.rows.len();
                    let mut status = ntr("{} row", "{} rows", n as u32).replace("{}", &super::data::group(n as u64));
                    if set.truncated {
                        status = tr("first {} rows").replace("{}", &super::data::group(n as u64));
                    }
                    c.set_status(&tab, &format!("{status} · {ms} ms"));
                    tab.grid.set(&set.columns, set.rows, &[], false);
                }
                Err(e) => c.set_status(&tab, &e),
            }
        });
    }

    fn send_change(self: &Rc<Self>, tab: Rc<ConsoleTab>, sql: String, schema_change: bool) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        self.remember(&sql);
        self.set_status(&tab, &tr("Running…"));
        let generation = self.next_generation();
        let c = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let started = Instant::now();
            let result = runtime::run(async move { session.execute(sql).await }).await;
            let ms = started.elapsed().as_millis();
            let Some(c) = c.upgrade() else { return };
            if c.generation.get() != generation {
                return;
            }
            match result {
                Ok(n) => {
                    let text = ntr("{} row changed", "{} rows changed", n.min(u32::MAX as u64) as u32)
                        .replace("{}", &super::data::group(n));
                    c.set_status(&tab, &format!("{text} · {ms} ms"));
                    tab.grid.set(&[], Vec::new(), &[], false);
                    if schema_change {
                        c.ctx.schema_changed();
                    } else {
                        c.ctx.rows_changed();
                    }
                }
                Err(e) => c.set_status(&tab, &e),
            }
        });
    }

    fn next_generation(&self) -> u64 {
        let g = self.generation.get() + 1;
        self.generation.set(g);
        g
    }

    /// The plan of the statement in the editor, as an indented list. The
    /// statement itself is never run (no ANALYZE).
    pub fn explain(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let Some(tab) = self.current() else { return };
        let sql = text_of(&tab.buffer);
        if sql.trim().is_empty() {
            return;
        }
        let dialect = session.dialect;
        let explain = plan::explain_sql(dialect, &sql);
        self.set_status(&tab, &tr("Planning…"));
        let generation = self.next_generation();
        let c = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = runtime::run(async move { session.query(explain, 10_000).await }).await;
            let Some(c) = c.upgrade() else { return };
            if c.generation.get() != generation {
                return;
            }
            let tree = result.and_then(|set| match dialect {
                Dialect::Sqlite => {
                    let rows: Vec<(i64, i64, String)> = set
                        .rows
                        .iter()
                        .filter_map(|r| {
                            Some((
                                r.first()?.as_deref()?.parse().ok()?,
                                r.get(1)?.as_deref()?.parse().ok()?,
                                r.last()?.clone().unwrap_or_default(),
                            ))
                        })
                        .collect();
                    Ok(plan::sqlite(&rows))
                }
                Dialect::Postgres => {
                    let json: String = set.rows.iter().filter_map(|r| r.first().cloned().flatten()).collect::<Vec<_>>().join("\n");
                    plan::postgres(&json)
                }
                Dialect::MySql => {
                    let json = set.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
                    plan::mysql(&json)
                }
            });
            match tree {
                Ok(tree) => {
                    let rows: Vec<Vec<Option<String>>> = plan::lines(&tree)
                        .into_iter()
                        .map(|(depth, label, detail)| vec![Some(format!("{}{label}", "    ".repeat(depth))), Some(detail)])
                        .collect();
                    c.set_status(&tab, &tr("Query plan"));
                    tab.grid.set(&[tr("Step"), tr("Details")], rows, &[], false);
                }
                Err(e) => c.set_status(&tab, &e),
            }
        });
    }

    fn fill_history(self: &Rc<Self>) {
        clear(&self.history_list);
        let queries = self.history.borrow().queries.clone();
        if queries.is_empty() {
            self.history_list.append(&hint(&tr("Nothing run yet")));
        }
        for q in queries {
            let c = Rc::downgrade(self);
            let button = sql_button(&q, None);
            button.connect_clicked(move |b| {
                if let Some(c) = c.upgrade() {
                    c.set_text(&q);
                }
                popdown(b);
            });
            self.history_list.append(&button);
        }
    }

    fn fill_saved(self: &Rc<Self>) {
        clear(&self.saved_list);
        let save = gtk::Button::builder().label(tr("Save this query…")).css_classes(["flat"]).build();
        let c = Rc::downgrade(self);
        save.connect_clicked(move |b| {
            popdown(b);
            if let Some(c) = c.upgrade() {
                c.save_query();
            }
        });
        self.saved_list.append(&save);
        let queries = self.saved.borrow().queries.clone();
        for q in queries {
            let row = gtk::Box::builder().spacing(4).build();
            let button = sql_button(&q.sql, Some(&q.name));
            button.set_hexpand(true);
            let remove = gtk::Button::builder().icon_name("user-trash-symbolic").tooltip_text(tr("Forget")).css_classes(["flat", "circular"]).build();
            row.append(&button);
            row.append(&remove);
            let (c, sql) = (Rc::downgrade(self), q.sql.clone());
            button.connect_clicked(move |b| {
                if let Some(c) = c.upgrade() {
                    c.set_text(&sql);
                }
                popdown(b);
            });
            let (c, name) = (Rc::downgrade(self), q.name.clone());
            remove.connect_clicked(move |b| {
                let Some(c) = c.upgrade() else { return };
                c.saved.borrow_mut().remove(&name);
                c.store_saved();
                popdown(b);
            });
            self.saved_list.append(&row);
        }
    }

    fn store_saved(&self) {
        if let Some(k) = self.ctx.key.borrow().as_ref() {
            let _ = store::save(&file("saved", k), &*self.saved.borrow());
        }
    }

    fn save_query(self: &Rc<Self>) {
        let Some(tab) = self.current() else { return };
        let sql = text_of(&tab.buffer);
        if sql.trim().is_empty() {
            return;
        }
        let dialog = adw::AlertDialog::new(Some(&tr("Save Query")), None);
        let entry = gtk::Entry::builder().placeholder_text(tr("Name")).activates_default(true).text(tab_title(&sql)).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("save", &tr("Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        let c = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            if r != "save" {
                return;
            }
            if let Some(c) = c.upgrade() {
                c.saved.borrow_mut().put(&entry.text(), &sql);
                c.store_saved();
            }
        });
        dialog.present(Some(&self.widget));
    }
}

fn apply_scheme(buffer: &sourceview5::Buffer) {
    let manager = sourceview5::StyleSchemeManager::default();
    manager.prepend_search_path(&crate::data_dir().join("sourceview/styles").to_string_lossy());
    let id = if adw::StyleManager::default().is_dark() { "atlas-dark" } else { "atlas-light" };
    if let Some(scheme) = manager.scheme(id) {
        buffer.set_style_scheme(Some(&scheme));
    }
}

fn text_of(buffer: &impl IsA<gtk::TextBuffer>) -> String {
    let (s, e) = buffer.bounds();
    buffer.text(&s, &e, false).to_string()
}

fn tab_title(sql: &str) -> String {
    let one: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.is_empty() {
        return tr("Query");
    }
    let short: String = one.chars().take(24).collect();
    if one.chars().count() > 24 { format!("{short}…") } else { short }
}

fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn hint(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).css_classes(["hint"]).margin_top(12).margin_bottom(12).build()
}

fn sql_button(sql: &str, name: Option<&str>) -> gtk::Button {
    let one_line: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    if let Some(n) = name {
        content.append(&gtk::Label::builder().label(n).xalign(0.0).css_classes(["strong"]).build());
    }
    content.append(
        &gtk::Label::builder()
            .label(&one_line)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(60)
            .css_classes(["mono"])
            .build(),
    );
    gtk::Button::builder().child(&content).css_classes(["flat"]).build()
}

fn popdown(w: &impl IsA<gtk::Widget>) {
    if let Some(pop) = w.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
        pop.popdown();
    }
}
