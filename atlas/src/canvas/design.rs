//! Design Mode on the canvas: adding, editing and removing tables, drawing
//! foreign keys from a column to another table, undo and redo. Every change
//! goes out through `connect_schema_changed`; the window decides whether it
//! is a design of its own or a change to apply to a live database.

use atlas_core::design;
use atlas_core::schema::{Column, Relation, RelationKind, Schema, Table};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use super::{Canvas, Card, Drag, DragMode, State, card, motion, params};
use crate::i18n::tr;
use crate::palette::Palette;
use atlas_core::spring::Spring;

const UNDO_MAX: usize = 50;

impl State {
    /// The column whose row is under world point (wx, wy) of card `i`.
    pub(super) fn column_at(&self, i: usize, wx: f64, wy: f64) -> Option<usize> {
        let c = &self.cards[i];
        let t = &self.schema.as_ref()?.tables[c.table];
        let ly = wy - c.y.value;
        let _ = wx;
        if ly < card::HEADER {
            return None;
        }
        let index = ((ly - card::HEADER) / card::ROW).floor() as usize;
        card::row_center(t, index, c.height.value).map(|_| index)
    }
}

/// The key being drawn, from its column to the pointer.
pub(super) fn draw_link(s: &gtk::Snapshot, st: &State, schema: &Schema, p: &Palette) {
    let Some(Drag { mode: DragMode::Link { from, column, to }, moved: true, .. }) = st.drag.as_ref() else { return };
    let c = &st.cards[*from];
    let t = &schema.tables[c.table];
    let Some(cy) = card::row_center(t, *column, c.height.value) else { return };
    let (x0, y0) = ((c.x.value + c.metrics.w) as f32, (c.y.value + cy) as f32);
    let builder = gsk::PathBuilder::new();
    builder.move_to(x0, y0);
    let mid = (x0 + to.0 as f32) / 2.0;
    builder.cubic_to(mid, y0, mid, to.1 as f32, to.0 as f32, to.1 as f32);
    let stroke = gsk::Stroke::new(1.5);
    stroke.set_dash(&[6.0, 4.0]);
    s.append_stroke(&builder.to_path(), &stroke, &p.ink);
    s.append_color(&p.ink, &graphene::Rect::new(to.0 as f32 - 3.0, to.1 as f32 - 3.0, 6.0, 6.0));
}

impl Canvas {
    pub fn connect_schema_changed<F: Fn(Schema) + 'static>(&self, f: F) {
        self.imp().callbacks.borrow_mut().schema_changed.push(Box::new(f));
    }

    pub fn design(&self) -> bool {
        self.imp().state.borrow().design
    }

    pub fn set_design(&self, on: bool) {
        self.st().design = on;
        if on && self.imp().state.borrow().schema.is_none() {
            self.set_schema(Some(Schema { name: tr("New design"), ..Default::default() }), &Default::default());
        }
        self.queue_draw();
    }

    fn emit_schema(&self) {
        let Some(schema) = self.schema() else { return };
        for f in &self.imp().callbacks.borrow().schema_changed {
            f(schema.clone());
        }
        self.emit_layout();
    }

    /// Keeps the schema as it is now, for undo.
    fn checkpoint(&self) {
        let mut st = self.st();
        if let Some(s) = st.schema.clone() {
            st.undo.push(s);
            if st.undo.len() > UNDO_MAX {
                st.undo.remove(0);
            }
            st.redo.clear();
        }
    }

    pub fn undo(&self) {
        let restored = {
            let mut st = self.st();
            let Some(prev) = st.undo.pop() else { return };
            if let Some(now) = st.schema.replace(prev) {
                st.redo.push(now);
            }
            true
        };
        if restored {
            self.resync();
        }
    }

    pub fn redo(&self) {
        let restored = {
            let mut st = self.st();
            let Some(next) = st.redo.pop() else { return };
            if let Some(now) = st.schema.replace(next) {
                st.undo.push(now);
            }
            true
        };
        if restored {
            self.resync();
        }
    }

    /// Cards for a schema that changed under them: kept where the table is
    /// still there (by name), new ones near the middle, gone ones removed.
    fn resync(&self) {
        let ctx = self.pango_context();
        let (cx, cy) = self.view_center();
        {
            let mut st = self.st();
            let Some(schema) = st.schema.clone() else { return };
            let old: Vec<(String, f64, f64, bool)> = st.cards.iter().map(|c| (c.name.clone(), c.x.value, c.y.value, c.unfolded)).collect();
            st.cards.clear();
            for (i, t) in schema.tables.iter().enumerate() {
                let before = old.iter().find(|o| o.0 == t.name);
                let unfolded = before.is_some_and(|b| b.3);
                let (x, y) = before.map_or((cx + i as f64 * 12.0, cy + i as f64 * 12.0), |b| (b.1, b.2));
                st.cards.push(new_card(&ctx, i, t, x, y, unfolded));
            }
            st.order = (0..st.cards.len()).collect();
            st.focus = st.focus.filter(|f| *f < st.cards.len());
            st.hover = None;
        }
        self.queue_draw();
        self.emit_schema();
    }

    pub fn add_table_at_center(&self) {
        let (x, y) = self.view_center();
        self.add_table_at(x - card::MIN_W / 2.0, y - 60.0);
    }

    /// A new table with an id column at world point (x, y).
    pub fn add_table_at(&self, x: f64, y: f64) {
        if self.imp().state.borrow().schema.is_none() {
            return;
        }
        self.checkpoint();
        let ctx = self.pango_context();
        let index = {
            let mut st = self.st();
            let Some(schema) = st.schema.as_mut() else { return };
            let name = design::free_name(schema, "table");
            schema.tables.push(Table {
                name,
                columns: vec![Column { name: "id".into(), data_type: "integer".into(), primary: true, auto_increment: true, ..Default::default() }],
                ..Default::default()
            });
            let i = schema.tables.len() - 1;
            let t = schema.tables[i].clone();
            st.cards.push(new_card(&ctx, i, &t, x, y, false));
            let n = st.cards.len() - 1;
            st.order.push(n);
            n
        };
        self.queue_draw();
        self.emit_schema();
        self.edit_card(index);
    }

    pub(super) fn remove_card(&self, index: usize) {
        self.checkpoint();
        {
            let mut st = self.st();
            let table = st.cards[index].table;
            let Some(schema) = st.schema.as_mut() else { return };
            let name = schema.tables[table].name.clone();
            schema.tables.remove(table);
            schema.relations.retain(|r| r.from_table != name && r.to_table != name);
            st.cards.remove(index);
            for c in &mut st.cards {
                if c.table > table {
                    c.table -= 1;
                }
                c.cache = None;
            }
            st.order = st.order.iter().filter(|o| **o != index).map(|o| if *o > index { o - 1 } else { *o }).collect();
            st.focus = None;
            st.hover = None;
        }
        self.queue_draw();
        self.emit_schema();
    }

    /// Replaces table `index` of the schema, following a rename into the
    /// relations.
    fn update_table(&self, card_index: usize, table: Table) {
        self.checkpoint();
        let ctx = self.pango_context();
        {
            let mut st = self.st();
            let ti = st.cards[card_index].table;
            let Some(schema) = st.schema.as_mut() else { return };
            let old = schema.tables[ti].name.clone();
            if old != table.name {
                for r in &mut schema.relations {
                    if r.from_table == old {
                        r.from_table = table.name.clone();
                    }
                    if r.to_table == old {
                        r.to_table = table.name.clone();
                    }
                }
            }
            // Keys on columns that are gone go too.
            let names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
            schema.relations.retain(|r| r.from_table != table.name || r.from_columns.iter().all(|c| names.contains(c)));
            schema.tables[ti] = table.clone();
            let c = &mut st.cards[card_index];
            c.metrics = card::measure(&ctx, &table);
            c.height.set(c.metrics.height(c.unfolded));
            c.name = table.name.clone();
            for c in &mut st.cards {
                c.cache = None;
            }
        }
        self.queue_draw();
        self.emit_schema();
    }

    /// A foreign key from `column` of card `from` to the table under `to`.
    pub(super) fn link(&self, from: usize, column: usize, to: (f64, f64)) {
        let relation = {
            let st = self.imp().state.borrow();
            let Some(target) = st.hit(to.0, to.1) else { return };
            let Some(schema) = st.schema.as_ref() else { return };
            let child = &schema.tables[st.cards[from].table];
            let parent = &schema.tables[st.cards[target].table];
            let Some(col) = child.columns.get(column) else { return };
            let pk = parent.primary_key();
            let to_col = pk.first().map(|s| (*s).to_owned()).or_else(|| parent.columns.first().map(|c| c.name.clone()));
            let Some(to_col) = to_col else { return };
            if schema.relations.iter().any(|r| r.from_table == child.name && r.from_columns == [col.name.clone()] && r.kind == RelationKind::Declared) {
                return;
            }
            Relation {
                name: Some(format!("{}_{}_fkey", child.name, col.name)),
                from_table: child.name.clone(),
                from_columns: vec![col.name.clone()],
                to_table: parent.name.clone(),
                to_columns: vec![to_col],
                kind: RelationKind::Declared,
                ..Default::default()
            }
        };
        self.checkpoint();
        {
            let mut st = self.st();
            if let Some(s) = st.schema.as_mut() {
                // An inferred guess for the same column makes way.
                s.relations.retain(|r| !(r.kind == RelationKind::Inferred && r.from_table == relation.from_table && r.from_columns == relation.from_columns));
                s.relations.push(relation);
            }
            for c in &mut st.cards {
                c.cache = None;
            }
        }
        self.queue_draw();
        self.emit_schema();
    }

    /// A popover on the card: its name and its columns as lines of text.
    pub(super) fn edit_card(&self, index: usize) {
        let Some(table) = ({
            let st = self.imp().state.borrow();
            st.schema.as_ref().map(|s| s.tables[st.cards[index].table].clone())
        }) else {
            return;
        };
        let rect = {
            let st = self.imp().state.borrow();
            let c = &st.cards[index];
            let z = st.zoom.value;
            gtk::gdk::Rectangle::new(
                (c.x.value * z + st.pan_x.value) as i32,
                (c.y.value * z + st.pan_y.value) as i32,
                (c.metrics.w * z) as i32,
                (card::HEADER * z) as i32,
            )
        };
        let name = gtk::Entry::builder().text(&table.name).build();
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(&design::columns_to_text(&table.columns));
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .monospace(true)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .css_classes(["console"])
            .build();
        let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(180).min_content_width(340).build();
        let hint = gtk::Label::builder()
            .label(tr("One column per line: name type [pk] [null] [auto] [default …]"))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["hint"])
            .build();
        let error = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["error-detail"]).build();
        let apply = gtk::Button::builder().label(tr("Apply")).css_classes(["solid", "small"]).build();
        let remove = gtk::Button::builder().label(tr("Remove Table")).css_classes(["quiet", "small"]).build();
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
        buttons.append(&remove);
        buttons.append(&apply);
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        content.append(&name);
        content.append(&scroll);
        content.append(&hint);
        content.append(&error);
        content.append(&buttons);
        let pop = gtk::Popover::builder().child(&content).pointing_to(&rect).position(gtk::PositionType::Right).build();
        pop.set_parent(self);
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        let canvas = self.downgrade();
        apply.connect_clicked(glib::clone!(
            #[weak]
            pop,
            move |_| {
                let Some(canvas) = canvas.upgrade() else { return };
                let (s, e) = buffer.bounds();
                let text = buffer.text(&s, &e, false);
                let new_name = name.text().trim().to_owned();
                if new_name.is_empty() {
                    error.set_text(&tr("The table needs a name."));
                    return;
                }
                let taken = canvas.schema().is_some_and(|s| new_name != table.name && s.table(&new_name).is_some());
                if taken {
                    error.set_text(&tr("Another table is called {name}.").replace("{name}", &new_name));
                    return;
                }
                match design::columns_from_text(&text, &table.columns) {
                    Ok(columns) => {
                        let mut t = table.clone();
                        t.name = new_name;
                        t.columns = columns;
                        canvas.update_table(index, t);
                        pop.popdown();
                    }
                    Err(e) => error.set_text(&e),
                }
            }
        ));
        let canvas = self.downgrade();
        remove.connect_clicked(glib::clone!(
            #[weak]
            pop,
            move |_| {
                pop.popdown();
                if let Some(c) = canvas.upgrade() {
                    c.remove_card(index);
                }
            }
        ));
        pop.popup();
    }
}

impl Canvas {
    /// The world point in the middle of the visible area.
    fn view_center(&self) -> (f64, f64) {
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        self.imp().state.borrow().to_world(w / 2.0, h / 2.0)
    }
}

fn new_card(ctx: &gtk::pango::Context, index: usize, t: &Table, x: f64, y: f64, unfolded: bool) -> Card {
    let metrics = card::measure(ctx, t);
    let h = metrics.height(unfolded);
    let cam = params(motion::CAMERA);
    let mut card = Card {
        table: index,
        metrics,
        x: Spring::new(x, cam).with_epsilon(0.1),
        y: Spring::new(y, cam).with_epsilon(0.1),
        height: Spring::new(h, params(motion::GROW)).with_epsilon(0.1),
        dim: Spring::new(1.0, params(motion::HOVER)).with_epsilon(0.002),
        unfolded,
        cache: None,
        name: t.name.clone(),
    };
    card.x.set(x);
    card.y.set(y);
    card
}
