//! Ctrl+K: type a table or column name, Enter flies there.

use std::cell::RefCell;
use std::rc::Rc;

use atlas_core::schema::Schema;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::i18n::tr;

const MAX: usize = 12;

type NameFn = Box<dyn Fn(&str)>;

pub struct Search {
    pub widget: gtk::Revealer,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    /// (table, column) per result row.
    results: RefCell<Vec<(String, Option<String>)>>,
    schema: RefCell<Option<Schema>>,
    chosen: RefCell<Option<NameFn>>,
}

impl Search {
    pub fn new() -> Rc<Self> {
        let entry = gtk::SearchEntry::builder().placeholder_text(tr("Go to table or column")).hexpand(true).build();
        let list = gtk::ListBox::builder().css_classes(["results"]).selection_mode(gtk::SelectionMode::Browse).build();
        let inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["search"]).width_request(460).build();
        inner.append(&entry);
        inner.append(&list);
        let widget = gtk::Revealer::builder()
            .child(&inner)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(crate::motion::STATE_MS)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(18)
            .build();
        let search = Rc::new(Search {
            widget,
            entry,
            list,
            results: RefCell::default(),
            schema: RefCell::default(),
            chosen: RefCell::default(),
        });
        let s = Rc::downgrade(&search);
        search.entry.connect_search_changed(move |_| {
            if let Some(s) = s.upgrade() {
                s.refresh();
            }
        });
        let s = Rc::downgrade(&search);
        search.entry.connect_activate(move |_| {
            if let Some(s) = s.upgrade() {
                let i = s.list.selected_row().map_or(0, |r| r.index());
                s.choose(i as usize);
            }
        });
        let s = Rc::downgrade(&search);
        search.entry.connect_stop_search(move |_| {
            if let Some(s) = s.upgrade() {
                s.hide();
            }
        });
        let s = Rc::downgrade(&search);
        search.list.connect_row_activated(move |_, row| {
            if let Some(s) = s.upgrade() {
                s.choose(row.index() as usize);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let s = Rc::downgrade(&search);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(s) = s.upgrade() else { return glib::Propagation::Proceed };
            let step = match key {
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            let n = s.results.borrow().len() as i32;
            if n == 0 {
                return glib::Propagation::Stop;
            }
            let i = s.list.selected_row().map_or(0, |r| r.index());
            if let Some(row) = s.list.row_at_index((i + step).rem_euclid(n)) {
                s.list.select_row(Some(&row));
            }
            glib::Propagation::Stop
        });
        search.entry.add_controller(keys);
        search
    }

    pub fn on_chosen(&self, f: impl Fn(&str) + 'static) {
        *self.chosen.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_schema(&self, schema: Option<Schema>) {
        *self.schema.borrow_mut() = schema;
    }

    pub fn show(&self) {
        self.entry.set_text("");
        self.refresh();
        self.widget.set_reveal_child(true);
        self.entry.grab_focus();
    }

    pub fn hide(&self) {
        self.widget.set_reveal_child(false);
    }

    pub fn is_shown(&self) -> bool {
        self.widget.reveals_child()
    }

    fn refresh(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let needle = self.entry.text().to_lowercase();
        let mut found: Vec<(u8, String, Option<String>)> = Vec::new();
        if let Some(schema) = self.schema.borrow().as_ref() {
            for t in &schema.tables {
                let name = t.name.to_lowercase();
                if needle.is_empty() || name.contains(&needle) {
                    let rank = if name == needle { 0 } else if name.starts_with(&needle) { 1 } else { 2 };
                    found.push((rank, t.name.clone(), None));
                }
                if needle.len() >= 2 {
                    for c in &t.columns {
                        let cn = c.name.to_lowercase();
                        if cn.contains(&needle) {
                            let rank = if cn == needle { 3 } else { 4 };
                            found.push((rank, t.name.clone(), Some(c.name.clone())));
                        }
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1)));
        found.truncate(MAX);
        let mut results = self.results.borrow_mut();
        results.clear();
        for (_, table, column) in found {
            let row = gtk::Box::builder().spacing(8).build();
            match &column {
                Some(c) => {
                    row.append(&gtk::Label::builder().label(c).xalign(0.0).build());
                    row.append(&gtk::Label::builder().label(&table).hexpand(true).xalign(1.0).css_classes(["count"]).build());
                }
                None => {
                    row.append(&gtk::Label::builder().label(&table).xalign(0.0).hexpand(true).css_classes(["strong"]).build());
                    row.append(&gtk::Label::builder().label(tr("table")).css_classes(["count"]).build());
                }
            }
            self.list.append(&row);
            results.push((table, column));
        }
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
        self.list.set_visible(!results.is_empty());
    }

    fn choose(&self, index: usize) {
        let table = self.results.borrow().get(index).map(|(t, _)| t.clone());
        self.hide();
        if let (Some(t), Some(f)) = (table, self.chosen.borrow().as_ref()) {
            f(&t);
        }
    }
}
