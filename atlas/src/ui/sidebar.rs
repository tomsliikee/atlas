//! The list of tables beside the canvas, with row counts and a filter.

use std::cell::RefCell;
use std::rc::Rc;

use atlas_core::schema::{Schema, TableKind};
use gtk::prelude::*;

use crate::i18n::tr;

type NameFn = Box<dyn Fn(&str)>;

pub struct Sidebar {
    pub widget: gtk::Box,
    list: gtk::ListBox,
    filter: gtk::SearchEntry,
    names: RefCell<Vec<String>>,
    activate: RefCell<Option<NameFn>>,
    /// Set while the selection follows the canvas, so it is not echoed back.
    syncing: std::cell::Cell<bool>,
}

impl Sidebar {
    pub fn new() -> Rc<Self> {
        let filter = gtk::SearchEntry::builder().placeholder_text(tr("Filter tables")).css_classes(["sidebar-filter"]).build();
        let list = gtk::ListBox::builder().css_classes(["navigation-sidebar", "tables"]).build();
        let scroll = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["sidebar"]).build();
        widget.append(&filter);
        widget.append(&scroll);
        let sidebar = Rc::new(Sidebar {
            widget,
            list,
            filter,
            names: RefCell::default(),
            activate: RefCell::default(),
            syncing: Default::default(),
        });
        let s = Rc::downgrade(&sidebar);
        sidebar.list.connect_row_selected(move |_, row| {
            let Some(s) = s.upgrade() else { return };
            if s.syncing.get() {
                return;
            }
            let name = row.and_then(|r| s.names.borrow().get(r.index() as usize).cloned());
            if let (Some(name), Some(f)) = (name, s.activate.borrow().as_ref()) {
                f(&name);
            }
        });
        let s = Rc::downgrade(&sidebar);
        sidebar.filter.connect_search_changed(move |e| {
            let Some(s) = s.upgrade() else { return };
            let needle = e.text().to_lowercase();
            let names = s.names.borrow().clone();
            s.list.set_filter_func(move |row| needle.is_empty() || names.get(row.index() as usize).is_some_and(|n| n.to_lowercase().contains(&needle)));
        });
        sidebar
    }

    pub fn on_activate(&self, f: impl Fn(&str) + 'static) {
        *self.activate.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_schema(&self, schema: Option<&Schema>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.names.borrow_mut().clear();
        self.filter.set_text("");
        let Some(schema) = schema else { return };
        let mut tables: Vec<_> = schema.tables.iter().collect();
        tables.sort_by_key(|t| t.name.to_lowercase());
        for t in tables {
            let name = gtk::Label::builder().label(&t.name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
            let mut right = crate::canvas::count_label(t);
            if t.kind == TableKind::View {
                right = if right.is_empty() { tr("view") } else { format!("{} · {right}", tr("view")) };
            }
            let count = gtk::Label::builder().label(&right).css_classes(["count"]).build();
            let row = gtk::Box::builder().spacing(8).build();
            row.append(&name);
            row.append(&count);
            self.list.append(&row);
            self.names.borrow_mut().push(t.name.clone());
        }
    }

    /// Selects `name` without activating it.
    pub fn select(&self, name: Option<&str>) {
        self.syncing.set(true);
        let index = name.and_then(|n| self.names.borrow().iter().position(|x| x == n));
        match index.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.syncing.set(false);
    }
}
