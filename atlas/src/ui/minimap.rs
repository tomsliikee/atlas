//! A small map of the whole diagram in the corner. The frame marks what
//! the window shows; a click or a drag moves the view there.

use gtk::prelude::*;
use gtk::{cairo, glib};

use crate::canvas::Canvas;
use crate::palette;

const W: i32 = 184;
const H: i32 = 124;
const PAD: f64 = 10.0;

pub fn new(canvas: &Canvas) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(W)
        .content_height(H)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(12)
        .margin_bottom(12)
        .css_classes(["minimap"])
        .build();
    area.set_draw_func(glib::clone!(
        #[weak]
        canvas,
        move |_, cr, w, h| draw(&canvas, cr, w as f64, h as f64)
    ));
    let drag = gtk::GestureDrag::new();
    let jump = glib::clone!(
        #[weak]
        canvas,
        #[weak]
        area,
        move |x: f64, y: f64, animated: bool| {
            if let Some((scale, ox, oy)) = mapping(&canvas, area.width() as f64, area.height() as f64) {
                canvas.center_on((x - ox) / scale, (y - oy) / scale, animated);
            }
        }
    );
    let j = jump.clone();
    drag.connect_drag_begin(move |_, x, y| j(x, y, true));
    drag.connect_drag_update(move |g, ox, oy| {
        if let Some((sx, sy)) = g.start_point() {
            jump(sx + ox, sy + oy, false);
        }
    });
    area.add_controller(drag);
    area.set_cursor_from_name(Some("pointer"));
    canvas.connect_view(glib::clone!(
        #[weak]
        area,
        move || area.queue_draw()
    ));
    area
}

/// World → minimap: (scale, offset x, offset y), fitting bounds and view.
fn mapping(canvas: &Canvas, w: f64, h: f64) -> Option<(f64, f64, f64)> {
    let (_, bounds, view) = canvas.overview()?;
    let x0 = bounds.x().min(view.x()) as f64;
    let y0 = bounds.y().min(view.y()) as f64;
    let x1 = (bounds.x() + bounds.width()).max(view.x() + view.width()) as f64;
    let y1 = (bounds.y() + bounds.height()).max(view.y() + view.height()) as f64;
    let scale = ((w - 2.0 * PAD) / (x1 - x0)).min((h - 2.0 * PAD) / (y1 - y0));
    let ox = PAD + ((w - 2.0 * PAD) - (x1 - x0) * scale) / 2.0 - x0 * scale;
    let oy = PAD + ((h - 2.0 * PAD) - (y1 - y0) * scale) / 2.0 - y0 * scale;
    Some((scale, ox, oy))
}

fn rgba(cr: &cairo::Context, c: &gtk::gdk::RGBA, alpha: f64) {
    cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64 * alpha);
}

fn draw(canvas: &Canvas, cr: &cairo::Context, w: f64, h: f64) {
    let p = palette::current();
    let Some((cards, _, view)) = canvas.overview() else { return };
    let Some((scale, ox, oy)) = mapping(canvas, w, h) else { return };
    for (r, lit) in cards {
        rgba(cr, if lit { &p.ink2 } else { &p.ink3 }, if lit { 0.9 } else { 0.55 });
        cr.rectangle(ox + r.x() as f64 * scale, oy + r.y() as f64 * scale, (r.width() as f64 * scale).max(1.5), (r.height() as f64 * scale).max(1.5));
        let _ = cr.fill();
    }
    rgba(cr, &p.ink, 0.9);
    cr.set_line_width(1.25);
    let (x, y) = (ox + view.x() as f64 * scale, oy + view.y() as f64 * scale);
    cr.rectangle(x.round() + 0.5, y.round() + 0.5, (view.width() as f64 * scale).round(), (view.height() as f64 * scale).round());
    let _ = cr.stroke();
}
