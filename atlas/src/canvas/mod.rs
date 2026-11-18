//! The diagram: a widget that draws every table as a card and every
//! relation as a line, under a camera that pans and zooms on springs.
//!
//! Frames stay cheap: each card's text is one cached render node, rebuilt
//! only when the card's state changes; a camera move only changes the
//! transform around them. A frame clock tick runs while something moves and
//! is removed when everything rests, so the canvas costs nothing when idle.

mod card;
mod edges;

use std::cell::RefCell;
use std::collections::VecDeque;

use atlas_core::layout::{self, Point, Size};
use atlas_core::schema::{RelationKind, Schema};
use atlas_core::spring::{Momentum, Params, Spring};
use atlas_core::store::{Camera, Layout};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::motion;
use crate::palette::{self, Palette};

pub use card::count_label;

pub const MIN_ZOOM: f64 = 0.08;
pub const MAX_ZOOM: f64 = 4.0;
const GRID: f64 = 24.0;
const DRAG_SLOP: f64 = 4.0;

fn params(s: motion::Spring) -> Params {
    Params { damping: s.damping, stiffness: s.stiffness }
}

struct Card {
    table: usize,
    metrics: card::Metrics,
    x: Spring,
    y: Spring,
    height: Spring,
    dim: Spring,
    unfolded: bool,
    /// Content node and the state it was built for.
    cache: Option<(gsk::RenderNode, (bool, bool, bool, bool))>,
}

impl Card {
    fn rect(&self) -> edges::Rect {
        edges::Rect { x: self.x.value as f32, y: self.y.value as f32, w: self.metrics.w as f32 }
    }

    fn contains(&self, wx: f64, wy: f64) -> bool {
        wx >= self.x.value && wx <= self.x.value + self.metrics.w && wy >= self.y.value && wy <= self.y.value + self.height.value
    }

    fn center(&self) -> (f64, f64) {
        (self.x.value + self.metrics.w / 2.0, self.y.value + self.height.value / 2.0)
    }

    fn moving(&self) -> bool {
        !(self.x.is_resting() && self.y.is_resting() && self.height.is_resting() && self.dim.is_resting())
    }
}

enum DragMode {
    Pan { pan_x: f64, pan_y: f64 },
    Card { index: usize, x: f64, y: f64 },
}

struct Drag {
    mode: DragMode,
    start: (f64, f64),
    moved: bool,
    samples: VecDeque<(i64, f64, f64)>,
}

/// What a camera should do once the widget has a size.
#[derive(Clone, Copy)]
enum Pending {
    Fit,
    Restore(Camera),
}

pub struct State {
    schema: Option<Schema>,
    cards: Vec<Card>,
    show_inferred: bool,
    zoom: Spring,
    pan_x: Spring,
    pan_y: Spring,
    momentum: Momentum,
    /// Zoom around a fixed point: (screen x, screen y, world x, world y).
    anchor: Option<(f64, f64, f64, f64)>,
    focus: Option<usize>,
    hover: Option<usize>,
    /// Draw order: the last one is on top.
    order: Vec<usize>,
    drag: Option<Drag>,
    pointer: (f64, f64),
    tick: Option<gtk::TickCallbackId>,
    last_frame: Option<i64>,
    pending: Option<Pending>,
    pinch_start: Option<f64>,
    layout_dirty: bool,
}

impl Default for State {
    fn default() -> Self {
        let cam = params(motion::CAMERA);
        State {
            schema: None,
            cards: Vec::new(),
            show_inferred: true,
            zoom: Spring::new(1.0, cam).with_epsilon(0.0004),
            pan_x: Spring::new(0.0, cam).with_epsilon(0.2),
            pan_y: Spring::new(0.0, cam).with_epsilon(0.2),
            momentum: Momentum::default(),
            anchor: None,
            focus: None,
            hover: None,
            order: Vec::new(),
            drag: None,
            pointer: (0.0, 0.0),
            tick: None,
            last_frame: None,
            pending: None,
            pinch_start: None,
            layout_dirty: false,
        }
    }
}

impl State {
    fn to_world(&self, sx: f64, sy: f64) -> (f64, f64) {
        ((sx - self.pan_x.value) / self.zoom.value, (sy - self.pan_y.value) / self.zoom.value)
    }

    fn hit(&self, wx: f64, wy: f64) -> Option<usize> {
        self.order.iter().rev().copied().find(|&i| self.cards[i].contains(wx, wy))
    }

    fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut it = self.cards.iter();
        let first = it.next()?;
        let mut b = (first.x.target, first.y.target, first.x.target + first.metrics.w, first.y.target + first.height.target);
        for c in it {
            b.0 = b.0.min(c.x.target);
            b.1 = b.1.min(c.y.target);
            b.2 = b.2.max(c.x.target + c.metrics.w);
            b.3 = b.3.max(c.y.target + c.height.target);
        }
        Some(b)
    }

    fn animating(&self) -> bool {
        !(self.zoom.is_resting() && self.pan_x.is_resting() && self.pan_y.is_resting() && self.momentum.is_resting())
            || self.cards.iter().any(Card::moving)
    }

    /// Updates the dim targets after the focus changed.
    fn refocus(&mut self) {
        let schema = self.schema.as_ref();
        let near: Vec<usize> = match (self.focus, schema) {
            (Some(f), Some(s)) => {
                let name = &s.tables[self.cards[f].table].name;
                s.neighbours(name, self.show_inferred).iter().filter_map(|n| s.table_index(n)).collect()
            }
            _ => Vec::new(),
        };
        for (i, c) in self.cards.iter_mut().enumerate() {
            let lit = self.focus.is_none() || self.focus == Some(i) || near.contains(&c.table);
            c.dim.retarget(if lit { 1.0 } else { 0.22 });
            c.cache = None;
        }
    }

    fn raise(&mut self, index: usize) {
        self.order.retain(|&i| i != index);
        self.order.push(index);
    }
}

type NameFn = Box<dyn Fn(&str)>;
type FocusFn = Box<dyn Fn(Option<&str>)>;

/// Cards (world rectangle, lit), world bounds, visible world rectangle.
pub type Overview = (Vec<(graphene::Rect, bool)>, graphene::Rect, graphene::Rect);

#[derive(Default)]
struct Callbacks {
    open: Vec<NameFn>,
    focus: Vec<FocusFn>,
    view: Vec<Box<dyn Fn()>>,
    layout: Vec<Box<dyn Fn()>>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub state: RefCell<State>,
        pub(super) callbacks: RefCell<Callbacks>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "AtlasCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let pending = self.state.borrow_mut().pending.take();
            if let Some(p) = pending {
                self.obj().apply_pending(p);
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Canvas {
    fn st(&self) -> std::cell::RefMut<'_, State> {
        self.imp().state.borrow_mut()
    }

    fn setup(&self) {
        self.set_focusable(true);
        self.set_overflow(gtk::Overflow::Hidden);
        self.set_hexpand(true);
        self.set_vexpand(true);
        self.add_css_class("canvas");

        // Wheel and touchpad.
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC,
        );
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |ctl, dx, dy| {
                canvas.on_scroll(ctl, dx, dy);
                glib::Propagation::Stop
            }
        ));
        scroll.connect_decelerate(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |ctl, vx, vy| {
                // Kinetic only for touchpad panning, not for Ctrl-zoom.
                if ctl.unit() == gdk::ScrollUnit::Surface && !ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    canvas.st().momentum = Momentum { vx: -vx, vy: -vy };
                    canvas.animate();
                }
            }
        ));
        self.add_controller(scroll);

        // Pinch.
        let zoom = gtk::GestureZoom::new();
        zoom.connect_begin(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| {
                let mut st = canvas.st();
                st.pinch_start = Some(st.zoom.value);
                st.momentum = Momentum::default();
            }
        ));
        zoom.connect_scale_changed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |g, scale| {
                let Some(start) = canvas.st().pinch_start else { return };
                let (cx, cy) = g.bounding_box_center().unwrap_or(canvas.st().pointer);
                canvas.zoom_at(start * scale, cx, cy, false);
            }
        ));
        zoom.connect_end(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| canvas.st().pinch_start = None
        ));
        self.add_controller(zoom);

        // Drag: cards with the primary button, the view with any button on
        // empty space or the middle button anywhere.
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |g, x, y| canvas.drag_begin(g, x, y)
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |g, ox, oy| canvas.drag_update(g, ox, oy)
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |g, ox, oy| canvas.drag_end(g, ox, oy)
        ));
        self.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, n, x, y| {
                if n == 2 {
                    let (wx, wy) = canvas.st().to_world(x, y);
                    let hit = canvas.st().hit(wx, wy);
                    if let Some(name) = hit.and_then(|i| canvas.table_name(i)) {
                        canvas.emit_open(&name);
                    }
                }
            }
        ));
        self.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, x, y| canvas.on_motion(x, y)
        ));
        self.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| canvas.on_key(key, mods)
        ));
        self.add_controller(keys);

        adw::StyleManager::default().connect_dark_notify(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_| canvas.invalidate()
        ));
    }

    // ---- Public API ------------------------------------------------------

    pub fn connect_open<F: Fn(&str) + 'static>(&self, f: F) {
        self.imp().callbacks.borrow_mut().open.push(Box::new(f));
    }

    pub fn connect_focus<F: Fn(Option<&str>) + 'static>(&self, f: F) {
        self.imp().callbacks.borrow_mut().focus.push(Box::new(f));
    }

    /// Called whenever the camera or the cards move (zoom label, minimap).
    pub fn connect_view<F: Fn() + 'static>(&self, f: F) {
        self.imp().callbacks.borrow_mut().view.push(Box::new(f));
    }

    /// Called when positions or folds changed and should be saved.
    pub fn connect_layout<F: Fn() + 'static>(&self, f: F) {
        self.imp().callbacks.borrow_mut().layout.push(Box::new(f));
    }

    pub fn set_schema(&self, schema: Option<Schema>, saved: &Layout) {
        let ctx = self.pango_context();
        {
            let mut st = self.st();
            st.cards.clear();
            st.focus = None;
            st.hover = None;
            st.drag = None;
            st.momentum = Momentum::default();
            st.schema = schema;
            let Some(schema) = st.schema.clone() else {
                st.order.clear();
                drop(st);
                self.queue_draw();
                return;
            };
            let grow = params(motion::GROW);
            let cam = params(motion::CAMERA);
            for (i, t) in schema.tables.iter().enumerate() {
                let metrics = card::measure(&ctx, t);
                let unfolded = saved.unfolded.contains(&t.name);
                let h = metrics.height(unfolded);
                st.cards.push(Card {
                    table: i,
                    metrics,
                    x: Spring::new(0.0, cam).with_epsilon(0.1),
                    y: Spring::new(0.0, cam).with_epsilon(0.1),
                    height: Spring::new(h, grow).with_epsilon(0.1),
                    dim: Spring::new(1.0, params(motion::HOVER)).with_epsilon(0.002),
                    unfolded,
                    cache: None,
                });
            }
            st.order = (0..st.cards.len()).collect();
            let auto = auto_layout(&st);
            for (i, c) in st.cards.iter_mut().enumerate() {
                let name = &schema.tables[c.table].name;
                let p = saved.positions.get(name).copied().unwrap_or(auto[i]);
                c.x.set(p.x);
                c.y.set(p.y);
            }
            st.pending = Some(match saved.camera {
                Some(cam) if cam.zoom > 0.0 => Pending::Restore(cam),
                _ => Pending::Fit,
            });
        }
        if self.width() > 0
            && let Some(p) = self.st().pending.take()
        {
            self.apply_pending(p);
        }
        self.queue_draw();
        self.emit_view();
    }

    pub fn schema(&self) -> Option<Schema> {
        self.imp().state.borrow().schema.clone()
    }

    /// Positions, folds and camera, for saving.
    pub fn layout(&self) -> Layout {
        let st = self.imp().state.borrow();
        let mut out = Layout::default();
        let Some(schema) = &st.schema else { return out };
        for c in &st.cards {
            let name = schema.tables[c.table].name.clone();
            out.positions.insert(name.clone(), Point { x: c.x.target, y: c.y.target });
            if c.unfolded {
                out.unfolded.insert(name);
            }
        }
        out.camera = Some(Camera { x: st.pan_x.target, y: st.pan_y.target, zoom: st.zoom.target });
        out
    }

    pub fn set_show_inferred(&self, show: bool) {
        let mut st = self.st();
        st.show_inferred = show;
        for c in &mut st.cards {
            c.cache = None;
        }
        st.refocus();
        drop(st);
        self.animate();
    }

    pub fn show_inferred(&self) -> bool {
        self.imp().state.borrow().show_inferred
    }

    /// Lays everything out again, animated.
    pub fn tidy(&self) {
        {
            let mut st = self.st();
            let auto = auto_layout(&st);
            for (c, p) in st.cards.iter_mut().zip(auto) {
                c.x.retarget(p.x);
                c.y.retarget(p.y);
            }
            st.layout_dirty = true;
        }
        self.animate();
        self.fit();
    }

    pub fn fold_all(&self, unfolded: bool) {
        {
            let mut st = self.st();
            for c in &mut st.cards {
                if c.metrics.foldable {
                    c.unfolded = unfolded;
                    let h = c.metrics.height(unfolded);
                    c.height.params = params(if unfolded { motion::GROW } else { motion::SHRINK });
                    c.height.retarget(h);
                    c.cache = None;
                }
            }
            st.layout_dirty = true;
        }
        self.animate();
    }

    /// Unfolds or folds `name` on its spring.
    pub fn set_unfolded(&self, name: &str, unfolded: bool) {
        let index = {
            let st = self.imp().state.borrow();
            st.schema.as_ref().and_then(|s| s.table_index(name)).and_then(|t| st.cards.iter().position(|c| c.table == t))
        };
        if let Some(i) = index
            && self.imp().state.borrow().cards[i].unfolded != unfolded
        {
            self.toggle_fold(i);
        }
    }

    pub fn focused(&self) -> Option<String> {
        let st = self.imp().state.borrow();
        st.focus.and_then(|i| st.schema.as_ref().map(|s| s.tables[st.cards[i].table].name.clone()))
    }

    /// Focuses `name` and, with `fly`, moves the camera to it.
    pub fn focus_table(&self, name: Option<&str>, fly: bool) {
        let index = {
            let st = self.imp().state.borrow();
            name.and_then(|n| st.schema.as_ref()?.table_index(n)).and_then(|t| st.cards.iter().position(|c| c.table == t))
        };
        self.set_focus(index);
        if fly && let Some(i) = index {
            self.fly_to(i);
        }
    }

    pub fn zoom(&self) -> f64 {
        self.imp().state.borrow().zoom.target
    }

    pub fn zoom_by(&self, factor: f64) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let target = self.imp().state.borrow().zoom.target * factor;
        self.zoom_at(target, w / 2.0, h / 2.0, true);
    }

    pub fn zoom_reset(&self) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        self.zoom_at(1.0, w / 2.0, h / 2.0, true);
    }

    /// Shows every table.
    pub fn fit(&self) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        if w <= 0.0 {
            self.st().pending = Some(Pending::Fit);
            return;
        }
        let mut st = self.st();
        let Some((x0, y0, x1, y1)) = st.bounds() else { return };
        let margin = 64.0;
        let z = ((w - 2.0 * margin) / (x1 - x0)).min((h - 2.0 * margin) / (y1 - y0)).clamp(MIN_ZOOM, 1.0);
        st.anchor = None;
        st.zoom.retarget(z);
        st.pan_x.retarget(w / 2.0 - (x0 + x1) / 2.0 * z);
        st.pan_y.retarget(h / 2.0 - (y0 + y1) / 2.0 * z);
        drop(st);
        self.animate();
    }

    /// Moves the camera so that world point (x, y) is in the middle.
    pub fn center_on(&self, wx: f64, wy: f64, animated: bool) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let mut st = self.st();
        let z = st.zoom.target;
        st.anchor = None;
        st.momentum = Momentum::default();
        let (tx, ty) = (w / 2.0 - wx * z, h / 2.0 - wy * z);
        if animated {
            st.pan_x.retarget(tx);
            st.pan_y.retarget(ty);
        } else {
            st.pan_x.set(tx);
            st.pan_y.set(ty);
        }
        drop(st);
        self.animate();
    }

    /// The card of `name` in widget coordinates.
    pub fn card_rect(&self, name: &str) -> Option<graphene::Rect> {
        let st = self.imp().state.borrow();
        let t = st.schema.as_ref()?.table_index(name)?;
        let c = st.cards.iter().find(|c| c.table == t)?;
        let z = st.zoom.value;
        Some(graphene::Rect::new(
            (c.x.value * z + st.pan_x.value) as f32,
            (c.y.value * z + st.pan_y.value) as f32,
            (c.metrics.w * z) as f32,
            (c.height.value * z) as f32,
        ))
    }

    /// Cards as world rectangles (focused flag), the world bounds and the
    /// visible world rectangle, for the minimap.
    pub fn overview(&self) -> Option<Overview> {
        let st = self.imp().state.borrow();
        let (x0, y0, x1, y1) = st.bounds()?;
        let cards = st
            .cards
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let lit = c.dim.target > 0.5 && st.focus.is_some();
                (
                    graphene::Rect::new(c.x.value as f32, c.y.value as f32, c.metrics.w as f32, c.height.value as f32),
                    lit || st.focus == Some(i),
                )
            })
            .collect();
        let (vx0, vy0) = st.to_world(0.0, 0.0);
        let (vx1, vy1) = st.to_world(self.width() as f64, self.height() as f64);
        Some((
            cards,
            graphene::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32),
            graphene::Rect::new(vx0 as f32, vy0 as f32, (vx1 - vx0) as f32, (vy1 - vy0) as f32),
        ))
    }

    /// The whole diagram at zoom 1, for export: (node, bounds).
    pub fn export_node(&self, dark: bool) -> Option<(gsk::RenderNode, graphene::Rect)> {
        let p = palette::get(dark);
        let ctx = self.pango_context();
        let st = self.imp().state.borrow();
        let schema = st.schema.as_ref()?;
        let (x0, y0, x1, y1) = st.bounds()?;
        let m = 48.0;
        let bounds = graphene::Rect::new((x0 - m) as f32, (y0 - m) as f32, (x1 - x0 + 2.0 * m) as f32, (y1 - y0 + 2.0 * m) as f32);
        let s = gtk::Snapshot::new();
        s.append_color(&p.bg, &bounds);
        draw_edges(&s, &st, schema, p, true);
        for c in st.order.iter().map(|&i| &st.cards[i]) {
            let t = &schema.tables[c.table];
            let h = c.height.target;
            let rect = graphene::Rect::new(c.x.target as f32, c.y.target as f32, c.metrics.w as f32, h as f32);
            let rounded = gsk::RoundedRect::from_rect(rect, card::RADIUS);
            card::fill_rounded(&s, rounded, &p.card);
            s.push_rounded_clip(&rounded);
            if let Some(node) = card::content(&ctx, schema, t, &c.metrics, c.unfolded, false, st.show_inferred, p) {
                s.save();
                s.translate(&graphene::Point::new(c.x.target as f32, c.y.target as f32));
                s.append_node(&node);
                s.restore();
            }
            s.pop();
        }
        s.to_node().map(|n| (n, bounds))
    }

    // ---- Internals --------------------------------------------------------

    fn table_name(&self, index: usize) -> Option<String> {
        let st = self.imp().state.borrow();
        st.schema.as_ref().map(|s| s.tables[st.cards[index].table].name.clone())
    }

    fn emit_open(&self, name: &str) {
        for f in &self.imp().callbacks.borrow().open {
            f(name);
        }
    }

    fn emit_view(&self) {
        for f in &self.imp().callbacks.borrow().view {
            f();
        }
    }

    fn emit_layout(&self) {
        for f in &self.imp().callbacks.borrow().layout {
            f();
        }
    }

    fn set_focus(&self, index: Option<usize>) {
        {
            let mut st = self.st();
            if st.focus == index {
                return;
            }
            st.focus = index;
            if let Some(i) = index {
                st.raise(i);
            }
            st.refocus();
        }
        let name = index.and_then(|i| self.table_name(i));
        for f in &self.imp().callbacks.borrow().focus {
            f(name.as_deref());
        }
        self.animate();
    }

    fn fly_to(&self, index: usize) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let mut st = self.st();
        let z = if st.zoom.target < 0.6 { 1.0 } else { st.zoom.target };
        let (cx, cy) = st.cards[index].center();
        st.anchor = None;
        st.momentum = Momentum::default();
        st.zoom.retarget(z);
        st.pan_x.retarget(w / 2.0 - cx * z);
        st.pan_y.retarget(h / 2.0 - cy * z);
        drop(st);
        self.animate();
    }

    fn apply_pending(&self, p: Pending) {
        match p {
            Pending::Fit => {
                self.fit();
                // The first view appears in place, not flown in.
                let mut st = self.st();
                let (z, x, y) = (st.zoom.target, st.pan_x.target, st.pan_y.target);
                st.zoom.set(z);
                st.pan_x.set(x);
                st.pan_y.set(y);
            }
            Pending::Restore(cam) => {
                let mut st = self.st();
                st.zoom.set(cam.zoom.clamp(MIN_ZOOM, MAX_ZOOM));
                st.pan_x.set(cam.x);
                st.pan_y.set(cam.y);
            }
        }
        self.queue_draw();
        self.emit_view();
    }

    /// Zooms to `target` keeping screen point (sx, sy) still.
    fn zoom_at(&self, target: f64, sx: f64, sy: f64, animated: bool) {
        let target = target.clamp(MIN_ZOOM, MAX_ZOOM);
        let mut st = self.st();
        let (wx, wy) = st.to_world(sx, sy);
        st.momentum = Momentum::default();
        st.anchor = Some((sx, sy, wx, wy));
        if animated {
            st.zoom.retarget(target);
        } else {
            st.zoom.set(target);
            let (px, py) = (sx - wx * target, sy - wy * target);
            st.pan_x.set(px);
            st.pan_y.set(py);
            st.anchor = None;
        }
        drop(st);
        if animated {
            self.animate();
        } else {
            self.queue_draw();
            self.emit_view();
        }
    }

    fn pan_by(&self, dx: f64, dy: f64, animated: bool) {
        let mut st = self.st();
        st.anchor = None;
        if animated {
            let (tx, ty) = (st.pan_x.target + dx, st.pan_y.target + dy);
            st.pan_x.retarget(tx);
            st.pan_y.retarget(ty);
            drop(st);
            self.animate();
        } else {
            let (x, y) = (st.pan_x.value + dx, st.pan_y.value + dy);
            st.pan_x.set(x);
            st.pan_y.set(y);
            drop(st);
            self.queue_draw();
            self.emit_view();
        }
    }

    fn on_scroll(&self, ctl: &gtk::EventControllerScroll, dx: f64, dy: f64) {
        let ctrl = ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
        let shift = ctl.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
        let wheel = ctl.unit() == gdk::ScrollUnit::Wheel;
        let (px, py) = self.st().pointer;
        self.st().momentum = Momentum::default();
        if ctrl {
            let factor = if wheel { 1.2_f64.powf(-dy) } else { (-dy * 0.01).exp() };
            let target = self.imp().state.borrow().zoom.target * factor;
            self.zoom_at(target, px, py, wheel);
        } else if wheel {
            let (dx, dy) = if shift { (dy, dx) } else { (dx, dy) };
            self.pan_by(-dx * 80.0, -dy * 80.0, true);
        } else {
            self.pan_by(-dx, -dy, false);
        }
    }

    fn on_motion(&self, x: f64, y: f64) {
        let hover = {
            let mut st = self.st();
            st.pointer = (x, y);
            let (wx, wy) = st.to_world(x, y);
            st.hit(wx, wy)
        };
        let changed = {
            let mut st = self.st();
            let changed = st.hover != hover;
            st.hover = hover;
            changed
        };
        if changed {
            self.set_cursor_from_name(Some(if hover.is_some() { "pointer" } else { "default" }));
        }
    }

    fn drag_begin(&self, g: &gtk::GestureDrag, x: f64, y: f64) {
        self.grab_focus();
        let button = g.current_button();
        let mut st = self.st();
        st.momentum = Momentum::default();
        st.anchor = None;
        let (wx, wy) = st.to_world(x, y);
        let hit = if button == gdk::BUTTON_PRIMARY { st.hit(wx, wy) } else { None };
        let mode = match hit {
            Some(i) => {
                st.raise(i);
                DragMode::Card { index: i, x: st.cards[i].x.value, y: st.cards[i].y.value }
            }
            None => {
                if button == gdk::BUTTON_SECONDARY {
                    return;
                }
                let (pan_x, pan_y) = (st.pan_x.value, st.pan_y.value);
                st.pan_x.set(pan_x);
                st.pan_y.set(pan_y);
                DragMode::Pan { pan_x, pan_y }
            }
        };
        let now = glib::monotonic_time();
        st.drag = Some(Drag { mode, start: (x, y), moved: false, samples: VecDeque::from([(now, x, y)]) });
    }

    fn drag_update(&self, _g: &gtk::GestureDrag, ox: f64, oy: f64) {
        let mut st = self.st();
        let z = st.zoom.value;
        let Some(drag) = st.drag.as_mut() else { return };
        if !drag.moved && ox.hypot(oy) < DRAG_SLOP {
            return;
        }
        drag.moved = true;
        let now = glib::monotonic_time();
        drag.samples.push_back((now, drag.start.0 + ox, drag.start.1 + oy));
        while drag.samples.len() > 2 && now - drag.samples[0].0 > 90_000 {
            drag.samples.pop_front();
        }
        match drag.mode {
            DragMode::Pan { pan_x, pan_y } => {
                st.pan_x.set(pan_x + ox);
                st.pan_y.set(pan_y + oy);
            }
            DragMode::Card { index, x, y } => {
                let c = &mut st.cards[index];
                c.x.set(x + ox / z);
                c.y.set(y + oy / z);
                st.layout_dirty = true;
            }
        }
        drop(st);
        self.queue_draw();
        self.emit_view();
    }

    fn drag_end(&self, _g: &gtk::GestureDrag, ox: f64, oy: f64) {
        let Some(drag) = self.st().drag.take() else { return };
        if !drag.moved {
            // A click.
            let (x, y) = (drag.start.0 + ox, drag.start.1 + oy);
            self.click(x, y);
            return;
        }
        match drag.mode {
            DragMode::Pan { .. } => {
                // Fling: speed over the last 90 ms.
                if let (Some(a), Some(b)) = (drag.samples.front(), drag.samples.back()) {
                    let dt = (b.0 - a.0) as f64 / 1e6;
                    let idle = (glib::monotonic_time() - b.0) as f64 / 1e6;
                    if dt > 0.01 && idle < 0.05 {
                        self.st().momentum = Momentum { vx: (b.1 - a.1) / dt, vy: (b.2 - a.2) / dt };
                        self.animate();
                    }
                }
            }
            DragMode::Card { .. } => self.save_soon(),
        }
        self.queue_draw();
    }

    fn click(&self, x: f64, y: f64) {
        let (hit, fold) = {
            let st = self.imp().state.borrow();
            let (wx, wy) = st.to_world(x, y);
            let hit = st.hit(wx, wy);
            let fold = hit.and_then(|i| {
                let c = &st.cards[i];
                let t = &st.schema.as_ref()?.tables[c.table];
                let r = card::fold_row(t, &c.metrics, c.unfolded)?;
                let (lx, ly) = ((wx - c.x.value) as f32, (wy - c.y.value) as f32);
                r.contains_point(&graphene::Point::new(lx, ly)).then_some(i)
            });
            (hit, fold)
        };
        if let Some(i) = fold {
            self.toggle_fold(i);
        } else if hit.is_some() && hit == self.imp().state.borrow().focus {
            // A second click on the focused card leaves the focus alone.
        } else {
            self.set_focus(hit);
        }
    }

    fn toggle_fold(&self, index: usize) {
        {
            let mut st = self.st();
            let c = &mut st.cards[index];
            if !c.metrics.foldable {
                return;
            }
            c.unfolded = !c.unfolded;
            let old_h = c.height.target;
            let h = c.metrics.height(c.unfolded);
            c.height.params = params(if c.unfolded { motion::GROW } else { motion::SHRINK });
            c.height.retarget(h);
            c.cache = None;
            // Cards below in the same column move with the bottom edge, so an
            // unfolded card never covers its neighbour.
            let (x0, x1, bottom) = (c.x.target, c.x.target + c.metrics.w, c.y.target + old_h);
            let delta = h - old_h;
            for (i, other) in st.cards.iter_mut().enumerate() {
                let overlaps = other.x.target < x1 && other.x.target + other.metrics.w > x0;
                if i != index && overlaps && other.y.target >= bottom - 1.0 {
                    let y = other.y.target + delta;
                    other.y.retarget(y);
                }
            }
            st.raise(index);
            st.layout_dirty = true;
        }
        self.animate();
    }

    fn on_key(&self, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let focus = self.imp().state.borrow().focus;
        let dir = match key {
            gdk::Key::Left => Some((-1.0, 0.0)),
            gdk::Key::Right => Some((1.0, 0.0)),
            gdk::Key::Up => Some((0.0, -1.0)),
            gdk::Key::Down => Some((0.0, 1.0)),
            _ => None,
        };
        if let Some(d) = dir {
            match focus {
                Some(f) => {
                    if let Some(next) = self.next_in(f, d) {
                        self.set_focus(Some(next));
                        self.fly_to(next);
                    }
                }
                None => self.pan_by(-d.0 * 120.0, -d.1 * 120.0, true),
            }
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => self.zoom_by(1.25),
            gdk::Key::minus | gdk::Key::KP_Subtract => self.zoom_by(0.8),
            gdk::Key::_0 | gdk::Key::KP_0 => self.fit(),
            gdk::Key::_1 | gdk::Key::KP_1 => self.zoom_reset(),
            gdk::Key::Escape if focus.is_some() => self.set_focus(None),
            gdk::Key::space if focus.is_some() => self.toggle_fold(focus.unwrap_or_default()),
            gdk::Key::Return | gdk::Key::KP_Enter if focus.is_some() => {
                if let Some(name) = focus.and_then(|f| self.table_name(f)) {
                    self.emit_open(&name);
                }
            }
            gdk::Key::Tab if focus.is_none() => {
                let first = self.imp().state.borrow().order.first().copied();
                self.set_focus(first);
                if let Some(f) = first {
                    self.fly_to(f);
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// The table to go to from `from` in direction `d`: connected tables
    /// first, any table if none lies that way.
    fn next_in(&self, from: usize, d: (f64, f64)) -> Option<usize> {
        let st = self.imp().state.borrow();
        let schema = st.schema.as_ref()?;
        let (fx, fy) = st.cards[from].center();
        let near: Vec<usize> = schema
            .neighbours(&schema.tables[st.cards[from].table].name, st.show_inferred)
            .iter()
            .filter_map(|n| schema.table_index(n))
            .filter_map(|t| st.cards.iter().position(|c| c.table == t))
            .collect();
        let score = |i: usize| -> Option<f64> {
            let (x, y) = st.cards[i].center();
            let (vx, vy) = (x - fx, y - fy);
            let dist = vx.hypot(vy);
            if i == from || dist < 1.0 {
                return None;
            }
            let cos = (vx * d.0 + vy * d.1) / dist;
            (cos > 0.5).then_some(dist * (2.0 - cos))
        };
        let best = |set: &mut dyn Iterator<Item = usize>| {
            set.filter_map(|i| score(i).map(|s| (s, i))).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, i)| i)
        };
        best(&mut near.iter().copied()).or_else(|| best(&mut (0..st.cards.len())))
    }

    fn invalidate(&self) {
        for c in &mut self.st().cards {
            c.cache = None;
        }
        self.queue_draw();
    }

    fn save_soon(&self) {
        let dirty = std::mem::take(&mut self.st().layout_dirty);
        if dirty {
            self.emit_layout();
        }
    }

    /// Makes sure a frame tick runs until everything rests.
    fn animate(&self) {
        self.queue_draw();
        if self.imp().state.borrow().tick.is_some() {
            return;
        }
        let id = self.add_tick_callback(|canvas, clock| {
            let now = clock.frame_time();
            let still = {
                let mut st = canvas.st();
                let dt = st.last_frame.map_or(1.0 / 60.0, |t| (now - t) as f64 / 1e6);
                st.last_frame = Some(now);
                st.zoom.step(dt);
                if let Some((sx, sy, wx, wy)) = st.anchor {
                    let z = st.zoom.value;
                    let (px, py) = (sx - wx * z, sy - wy * z);
                    st.pan_x.set(px);
                    st.pan_y.set(py);
                    if st.zoom.is_resting() {
                        st.anchor = None;
                    }
                } else {
                    st.pan_x.step(dt);
                    st.pan_y.step(dt);
                }
                if !st.momentum.is_resting() {
                    let (mx, my) = st.momentum.step(dt);
                    let (x, y) = (st.pan_x.value + mx, st.pan_y.value + my);
                    st.pan_x.set(x);
                    st.pan_y.set(y);
                }
                for c in &mut st.cards {
                    c.x.step(dt);
                    c.y.step(dt);
                    c.height.step(dt);
                    c.dim.step(dt);
                }
                !st.animating()
            };
            canvas.queue_draw();
            canvas.emit_view();
            if still {
                let mut st = canvas.st();
                st.tick = None;
                st.last_frame = None;
                drop(st);
                canvas.save_soon();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        self.st().tick = Some(id);
    }

    fn draw(&self, s: &gtk::Snapshot) {
        let (w, h) = (self.width() as f32, self.height() as f32);
        let p = palette::current();
        s.append_color(&p.bg, &graphene::Rect::new(0.0, 0.0, w, h));
        let ctx = self.pango_context();
        let mut st = self.st();
        let z = st.zoom.value;
        draw_grid(s, &st, w, h, p);
        let Some(schema) = st.schema.clone() else { return };

        s.save();
        s.translate(&graphene::Point::new(st.pan_x.value as f32, st.pan_y.value as f32));
        s.scale(z as f32, z as f32);
        draw_edges(s, &st, &schema, p, false);
        let dragging = match st.drag.as_ref() {
            Some(Drag { mode: DragMode::Card { index, .. }, moved: true, .. }) => Some(*index),
            _ => None,
        };
        let order = st.order.clone();
        let focus = st.focus;
        let show_inferred = st.show_inferred;
        for i in order {
            let c = &mut st.cards[i];
            let t = &schema.tables[c.table];
            let selected = focus == Some(i);
            let unfolded_content = c.unfolded || c.height.value > c.metrics.folded_h + 0.5;
            let key = (unfolded_content, selected, adw::StyleManager::default().is_dark(), show_inferred);
            if c.cache.as_ref().is_none_or(|(_, k)| *k != key) {
                c.cache = card::content(&ctx, &schema, t, &c.metrics, unfolded_content, selected, show_inferred, p).map(|n| (n, key));
            }
            let rect = graphene::Rect::new(c.x.value as f32, c.y.value as f32, c.metrics.w as f32, c.height.value as f32);
            let rounded = gsk::RoundedRect::from_rect(rect, card::RADIUS);
            let dim = c.dim.value.clamp(0.0, 1.0) as f32;
            if dim < 0.999 {
                s.push_opacity(dim as f64);
            }
            if dragging == Some(i) {
                s.append_outset_shadow(&rounded, &p.shadow, 0.0, 14.0, 0.0, 24.0);
            }
            card::fill_rounded(s, rounded, if selected { &p.fill } else { &p.card });
            if let Some((node, _)) = &c.cache {
                s.push_rounded_clip(&rounded);
                s.save();
                s.translate(&graphene::Point::new(c.x.value as f32, c.y.value as f32));
                s.append_node(node);
                s.restore();
                s.pop();
            }
            if dim < 0.999 {
                s.pop();
            }
        }
        s.restore();
    }
}

fn auto_layout(st: &State) -> Vec<Point> {
    let Some(schema) = &st.schema else { return Vec::new() };
    let sizes: Vec<Size> = st.cards.iter().map(|c| Size { w: c.metrics.w, h: c.metrics.height(c.unfolded) }).collect();
    let index = |name: &str| schema.table_index(name).and_then(|t| st.cards.iter().position(|c| c.table == t));
    let edges: Vec<(usize, usize)> = schema
        .relations
        .iter()
        .filter(|r| st.show_inferred || r.kind == RelationKind::Declared)
        .filter_map(|r| Some((index(&r.from_table)?, index(&r.to_table)?)))
        .collect();
    layout::layered(&sizes, &edges)
}

/// Faint dots that make a pan visible; gone when zoomed far out.
fn draw_grid(s: &gtk::Snapshot, st: &State, w: f32, h: f32, p: &Palette) {
    let z = st.zoom.value;
    let alpha = ((z - 0.35) / 0.25).clamp(0.0, 1.0) as f32;
    if alpha <= 0.0 || st.schema.is_none() {
        return;
    }
    let step = (GRID * z) as f32;
    let ox = st.pan_x.value.rem_euclid(GRID * z) as f32;
    let oy = st.pan_y.value.rem_euclid(GRID * z) as f32;
    let tile = graphene::Rect::new(ox, oy, step, step);
    s.push_repeat(&graphene::Rect::new(0.0, 0.0, w, h), Some(&tile));
    let r = (1.1 * z.sqrt()) as f32;
    let dot = gsk::RoundedRect::from_rect(graphene::Rect::new(ox - r, oy - r, 2.0 * r, 2.0 * r), r);
    card::fill_rounded(s, dot, &palette::fade(&p.line, alpha));
    s.pop();
}

fn draw_edges(s: &gtk::Snapshot, st: &State, schema: &Schema, p: &Palette, export: bool) {
    let index = |name: &str| schema.table_index(name).and_then(|t| st.cards.iter().position(|c| c.table == t));
    for r in &schema.relations {
        if r.kind == RelationKind::Inferred && !st.show_inferred {
            continue;
        }
        let (Some(a), Some(b)) = (index(&r.from_table), index(&r.to_table)) else { continue };
        let (ca, cb) = (&st.cards[a], &st.cards[b]);
        let (ta, tb) = (&schema.tables[ca.table], &schema.tables[cb.table]);
        let (ha, hb) = if export { (ca.height.target, cb.height.target) } else { (ca.height.value, cb.height.value) };
        let ya = r.from_columns.first().and_then(|c| ta.column_index(c)).and_then(|i| card::row_center(ta, i, ha)).unwrap_or(card::HEADER / 2.0);
        let yb = r.to_columns.first().and_then(|c| tb.column_index(c)).and_then(|i| card::row_center(tb, i, hb)).unwrap_or(card::HEADER / 2.0);
        let (ra, rb) = if export {
            (
                edges::Rect { x: ca.x.target as f32, y: ca.y.target as f32, w: ca.metrics.w as f32 },
                edges::Rect { x: cb.x.target as f32, y: cb.y.target as f32, w: cb.metrics.w as f32 },
            )
        } else {
            (ca.rect(), cb.rect())
        };
        let (child, parent) = edges::anchors(ra, ya as f32, rb, yb as f32, a == b);
        let lit = st.focus.is_some() && (st.focus == Some(a) || st.focus == Some(b));
        let alpha = if export { 1.0 } else { (ca.dim.value.min(cb.dim.value)).clamp(0.08, 1.0) as f32 };
        let color = palette::fade(if lit { &p.ink } else { &p.ink3 }, alpha);
        let stroke = gsk::Stroke::new(if lit { 1.6 } else { 1.25 });
        if r.kind == RelationKind::Inferred {
            stroke.set_dash(&[5.0, 4.0]);
        }
        s.append_stroke(&edges::curve(child, parent), &stroke, &color);
        let solid = gsk::Stroke::new(if lit { 1.6 } else { 1.25 });
        let (end_child, end_parent) = schema.ends(r);
        for (at, end) in [(child, end_child), (parent, end_parent)] {
            let (lines, circle) = edges::marks(at, end);
            if let Some(c) = circle {
                s.append_fill(&c, gsk::FillRule::Winding, &p.bg);
                s.append_stroke(&c, &solid, &color);
            }
            s.append_stroke(&lines, &solid, &color);
        }
    }
}
