//! Rows as a table: one `ColumnView` with a column per result column.
//! Sorting is only reported (the query does it on the server); foreign key
//! cells are links; reaching the bottom asks for the next page.

use std::cell::RefCell;
use std::rc::Rc;

use atlas_core::sql::Sort;
use gtk::prelude::*;
use gtk::{gio, glib};

pub type Row = Rc<Vec<Option<String>>>;

#[derive(Default)]
struct Hooks {
    sort: Option<Box<dyn Fn(Option<Sort>)>>,
    more: Option<Box<dyn Fn()>>,
    link: Option<Box<dyn Fn(usize, String)>>,
    selected: Option<Box<dyn Fn(Option<Row>)>>,
}

#[derive(Clone)]
pub struct Grid {
    pub widget: gtk::ScrolledWindow,
    view: gtk::ColumnView,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    columns: Rc<RefCell<Vec<String>>>,
    hooks: Rc<RefCell<Hooks>>,
    /// While columns are rebuilt, sorter changes are ours, not the user's.
    quiet: Rc<std::cell::Cell<bool>>,
}

impl Grid {
    pub fn new() -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let view = gtk::ColumnView::builder()
            .model(&selection)
            .reorderable(false)
            .show_column_separators(false)
            .show_row_separators(false)
            .css_classes(["grid"])
            .build();
        let widget = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hexpand(true).build();
        let grid = Grid {
            widget,
            view,
            store,
            selection,
            columns: Rc::default(),
            hooks: Rc::default(),
            quiet: Rc::default(),
        };

        let hooks = grid.hooks.clone();
        grid.widget.connect_edge_reached(move |_, pos| {
            if pos == gtk::PositionType::Bottom
                && let Some(f) = &hooks.borrow().more
            {
                f();
            }
        });

        let (hooks, sel) = (grid.hooks.clone(), grid.selection.clone());
        grid.selection.connect_selection_changed(move |_, _, _| {
            let bits = sel.selection();
            let row = (!bits.is_empty())
                .then(|| sel.item(bits.minimum()))
                .flatten()
                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                .map(|b| b.borrow::<Row>().clone());
            if let Some(f) = &hooks.borrow().selected {
                f(row);
            }
        });

        if let Some(sorter) = grid.view.sorter() {
            let (hooks, quiet, columns) = (grid.hooks.clone(), grid.quiet.clone(), grid.columns.clone());
            sorter.connect_changed(move |sorter, _| {
                if quiet.get() {
                    return;
                }
                let Some(sorter) = sorter.downcast_ref::<gtk::ColumnViewSorter>() else { return };
                let sort = sorter.primary_sort_column().and_then(|c| {
                    let index: usize = c.id()?.parse().ok()?;
                    Some(Sort {
                        column: columns.borrow().get(index)?.clone(),
                        descending: sorter.primary_sort_order() == gtk::SortType::Descending,
                    })
                });
                if let Some(f) = &hooks.borrow().sort {
                    f(sort);
                }
            });
        }
        grid
    }

    pub fn on_sort(&self, f: impl Fn(Option<Sort>) + 'static) {
        self.hooks.borrow_mut().sort = Some(Box::new(f));
    }

    pub fn on_more(&self, f: impl Fn() + 'static) {
        self.hooks.borrow_mut().more = Some(Box::new(f));
    }

    pub fn on_link(&self, f: impl Fn(usize, String) + 'static) {
        self.hooks.borrow_mut().link = Some(Box::new(f));
    }

    pub fn on_selected(&self, f: impl Fn(Option<Row>) + 'static) {
        self.hooks.borrow_mut().selected = Some(Box::new(f));
    }

    pub fn columns(&self) -> Vec<String> {
        self.columns.borrow().clone()
    }

    /// Replaces columns and rows. `links` are the column indexes that are
    /// foreign keys; `sortable` lets headers report clicks.
    pub fn set(&self, columns: &[String], rows: Vec<Vec<Option<String>>>, links: &[usize], sortable: bool) {
        self.quiet.set(true);
        while let Some(c) = self.view.columns().item(0).and_then(|o| o.downcast::<gtk::ColumnViewColumn>().ok()) {
            self.view.remove_column(&c);
        }
        *self.columns.borrow_mut() = columns.to_vec();
        for (i, name) in columns.iter().enumerate() {
            let sample = rows.iter().take(50).filter_map(|r| r.get(i)?.as_ref()).map(|v| v.chars().count()).max().unwrap_or(4);
            let chars = sample.max(name.chars().count()).max(4);
            let width = ((chars as f64) * 8.4 + 36.0).clamp(72.0, 380.0) as i32;
            let column = gtk::ColumnViewColumn::builder()
                .title(name)
                .factory(&self.factory(i, links.contains(&i)))
                .resizable(true)
                .fixed_width(width)
                .id(i.to_string())
                .build();
            if sortable {
                column.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
            }
            self.view.append_column(&column);
        }
        self.quiet.set(false);
        self.replace(rows);
    }

    pub fn replace(&self, rows: Vec<Vec<Option<String>>>) {
        let items: Vec<glib::BoxedAnyObject> = rows.into_iter().map(|r| glib::BoxedAnyObject::new(Rc::new(r))).collect();
        self.store.splice(0, self.store.n_items(), &items);
    }

    pub fn append(&self, rows: Vec<Vec<Option<String>>>) {
        let items: Vec<glib::BoxedAnyObject> = rows.into_iter().map(|r| glib::BoxedAnyObject::new(Rc::new(r))).collect();
        self.store.splice(self.store.n_items(), 0, &items);
    }

    pub fn clear_sort(&self) {
        self.quiet.set(true);
        self.view.sort_by_column(None, gtk::SortType::Ascending);
        self.quiet.set(false);
    }

    /// Selected rows, or all rows when nothing is selected.
    pub fn chosen_rows(&self) -> Vec<Vec<Option<String>>> {
        let bits = self.selection.selection();
        let take = |i: u32| {
            self.store
                .item(i)
                .and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok())
                .map(|b| b.borrow::<Row>().as_ref().clone())
        };
        if bits.is_empty() {
            (0..self.store.n_items()).filter_map(take).collect()
        } else {
            (0..self.store.n_items()).filter(|&i| bits.contains(i)).filter_map(take).collect()
        }
    }

    fn factory(&self, index: usize, link: bool) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        let hooks = self.hooks.clone();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .single_line_mode(true)
                .css_classes(["cell"])
                .build();
            if link {
                label.add_css_class("link");
                label.set_cursor_from_name(Some("pointer"));
                let click = gtk::GestureClick::new();
                let (hooks, weak) = (hooks.clone(), item.downgrade());
                click.connect_released(move |g, n, _, _| {
                    if n != 1 {
                        return;
                    }
                    let Some(item) = weak.upgrade() else { return };
                    let Some(row) = item.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok()) else { return };
                    let value = row.borrow::<Row>().get(index).cloned().flatten();
                    if let (Some(v), Some(f)) = (value, &hooks.borrow().link) {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        f(index, v);
                    }
                });
                label.add_controller(click);
            }
            item.set_child(Some(&label));
        });
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
            let Some(row) = item.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok()) else { return };
            let value = row.borrow::<Row>().get(index).cloned().flatten();
            match value {
                Some(v) => {
                    // One line in the grid; the detail pane shows all of it.
                    let line: String = v.chars().take(400).map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
                    label.set_text(&line);
                    label.remove_css_class("null");
                    if link {
                        label.add_css_class("link");
                    }
                }
                None => {
                    // NULL points nowhere, so it is not a link.
                    label.set_text("NULL");
                    label.add_css_class("null");
                    label.remove_css_class("link");
                }
            }
        });
        factory
    }
}
