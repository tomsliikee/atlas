//! One table as a card: its measurements and its drawn content.
//!
//! A card is a header (name, row count) and one row per visible column:
//! a PK/FK chip, the name, the type in Geist Mono. Tables with more than
//! `FOLD_OVER` columns show the first `FOLDED_ROWS` and a fold row; the
//! card unfolds on a spring by revealing the rest under a clip, so nothing
//! jumps.

use atlas_core::schema::{Schema, Table, TableKind};
use gtk::prelude::*;
use gtk::{graphene, gsk, pango};

use crate::palette::Palette;

pub const HEADER: f64 = 42.0;
pub const ROW: f64 = 26.0;
pub const PAD_X: f64 = 14.0;
pub const PAD_BOTTOM: f64 = 10.0;
pub const RADIUS: f32 = 12.0;
pub const MIN_W: f64 = 210.0;
pub const MAX_W: f64 = 360.0;
pub const FOLD_OVER: usize = 9;
pub const FOLDED_ROWS: usize = 7;
const CHIP_W: f64 = 24.0;

pub fn font(family: &str, size: f64, weight: pango::Weight) -> pango::FontDescription {
    let mut f = pango::FontDescription::new();
    f.set_family(family);
    f.set_absolute_size(size * pango::SCALE as f64);
    f.set_weight(weight);
    f
}

pub fn sans(size: f64, weight: pango::Weight) -> pango::FontDescription {
    font("Inter", size, weight)
}

pub fn mono(size: f64) -> pango::FontDescription {
    font("Geist Mono", size, pango::Weight::Normal)
}

/// Inter with tabular figures, so counts line up.
fn tabular() -> pango::AttrList {
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
    attrs
}

pub fn layout(ctx: &pango::Context, text: &str, desc: &pango::FontDescription) -> pango::Layout {
    let l = pango::Layout::new(ctx);
    l.set_font_description(Some(desc));
    l.set_attributes(Some(&tabular()));
    l.set_text(text);
    l
}

fn width_of(ctx: &pango::Context, text: &str, desc: &pango::FontDescription) -> f64 {
    layout(ctx, text, desc).pixel_size().0 as f64
}

/// Row counts as people say them: 940, 12.4k, 3.1M.
pub fn count_label(table: &Table) -> String {
    let Some(n) = table.rows else { return String::new() };
    let short = match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
    };
    if table.rows_estimated { format!("~{short}") } else { short }
}

/// What a card measures and which columns it shows.
#[derive(Clone, Debug)]
pub struct Metrics {
    pub w: f64,
    pub folded_h: f64,
    pub full_h: f64,
    /// True when the table has more columns than a folded card shows.
    pub foldable: bool,
}

impl Metrics {
    pub fn height(&self, unfolded: bool) -> f64 {
        if unfolded { self.full_h } else { self.folded_h }
    }
}

fn rows_shown(table: &Table, unfolded: bool) -> usize {
    if unfolded || table.columns.len() <= FOLD_OVER { table.columns.len() } else { FOLDED_ROWS }
}

pub fn measure(ctx: &pango::Context, table: &Table) -> Metrics {
    let name_w = width_of(ctx, &table.name, &sans(14.0, pango::Weight::Semibold));
    let count_w = width_of(ctx, &count_label(table), &sans(12.0, pango::Weight::Normal));
    let mut w = name_w + count_w + 24.0;
    let col_font = sans(13.0, pango::Weight::Normal);
    let type_font = mono(11.5);
    for c in &table.columns {
        let row_w = CHIP_W + 8.0 + width_of(ctx, &c.name, &col_font) + 18.0 + width_of(ctx, &c.data_type, &type_font);
        w = w.max(row_w);
    }
    let w = (w + 2.0 * PAD_X).clamp(MIN_W, MAX_W).ceil();
    let foldable = table.columns.len() > FOLD_OVER;
    let h = |rows: usize| HEADER + rows as f64 * ROW + if foldable { ROW } else { 0.0 } + PAD_BOTTOM;
    Metrics { w, folded_h: h(rows_shown(table, false)), full_h: h(rows_shown(table, true)), foldable }
}

/// Y of the middle of column `index`'s row, if `unfolded_to` shows it.
pub fn row_center(table: &Table, index: usize, visible_h: f64) -> Option<f64> {
    let y = HEADER + index as f64 * ROW + ROW / 2.0;
    // A row counts as shown once most of it is above the clip.
    (y + ROW * 0.25 <= visible_h - PAD_BOTTOM && index < table.columns.len()).then_some(y)
}

/// The fold row's rectangle in card coordinates, for hit testing.
pub fn fold_row(table: &Table, metrics: &Metrics, unfolded: bool) -> Option<graphene::Rect> {
    metrics.foldable.then(|| {
        let y = HEADER + rows_shown(table, unfolded) as f64 * ROW;
        graphene::Rect::new(0.0, y as f32, metrics.w as f32, ROW as f32)
    })
}

/// Text and chips of a card, without its background (drawn per frame so
/// the height can spring). Built once per state and cached.
#[allow(clippy::too_many_arguments)]
pub fn content(
    ctx: &pango::Context,
    schema: &Schema,
    table: &Table,
    metrics: &Metrics,
    unfolded: bool,
    selected: bool,
    with_inferred: bool,
    p: &Palette,
) -> Option<gsk::RenderNode> {
    let s = gtk::Snapshot::new();
    let w = metrics.w as f32;

    // Header: name left, count right.
    let weight = if selected { pango::Weight::Bold } else { pango::Weight::Semibold };
    let name = layout(ctx, &table.name, &sans(14.0, weight));
    name.set_width(((metrics.w - 2.0 * PAD_X - 56.0) * pango::SCALE as f64) as i32);
    name.set_ellipsize(pango::EllipsizeMode::End);
    let mut right = count_label(table);
    if table.kind == TableKind::View {
        right = if right.is_empty() { "view".into() } else { format!("view · {right}") };
    }
    let count = layout(ctx, &right, &sans(12.0, pango::Weight::Normal));
    let (_, nh) = name.pixel_size();
    let (cw, ch) = count.pixel_size();
    s.save();
    s.translate(&graphene::Point::new(PAD_X as f32, ((HEADER - nh as f64) / 2.0) as f32));
    s.append_layout(&name, &p.ink);
    s.restore();
    s.save();
    s.translate(&graphene::Point::new(w - PAD_X as f32 - cw as f32, ((HEADER - ch as f64) / 2.0) as f32));
    s.append_layout(&count, &p.ink3);
    s.restore();

    let col_font = sans(13.0, pango::Weight::Normal);
    let key_font = sans(13.0, pango::Weight::Semibold);
    let type_font = mono(11.5);
    let chip_font = mono(9.0);
    let shown = rows_shown(table, unfolded);
    for (i, c) in table.columns.iter().take(shown).enumerate() {
        let y = HEADER + i as f64 * ROW;
        let foreign = schema.is_foreign(&table.name, &c.name, with_inferred);
        // Chip: PK, FK, or both as PF.
        let chip = match (c.primary, foreign) {
            (true, true) => Some("PF"),
            (true, false) => Some("PK"),
            (false, true) => Some("FK"),
            _ => None,
        };
        if let Some(label) = chip {
            let rect = graphene::Rect::new(PAD_X as f32, (y + 6.0) as f32, CHIP_W as f32, (ROW - 12.0) as f32);
            fill_rounded(&s, gsk::RoundedRect::from_rect(rect, 4.0), &p.fill);
            let l = layout(ctx, label, &chip_font);
            let (lw, lh) = l.pixel_size();
            s.save();
            s.translate(&graphene::Point::new(
                (PAD_X + (CHIP_W - lw as f64) / 2.0) as f32,
                (y + (ROW - lh as f64) / 2.0) as f32,
            ));
            s.append_layout(&l, &p.ink2);
            s.restore();
        }
        let ty = layout(ctx, &c.data_type, &type_font);
        let (tw, th) = ty.pixel_size();
        let type_x = metrics.w - PAD_X - tw as f64;
        let name_x = PAD_X + CHIP_W + 8.0;
        let n = layout(ctx, &c.name, if c.primary { &key_font } else { &col_font });
        n.set_width(((type_x - name_x - 12.0).max(20.0) * pango::SCALE as f64) as i32);
        n.set_ellipsize(pango::EllipsizeMode::End);
        let (_, nh) = n.pixel_size();
        s.save();
        s.translate(&graphene::Point::new(name_x as f32, (y + (ROW - nh as f64) / 2.0) as f32));
        s.append_layout(&n, &p.ink);
        s.restore();
        s.save();
        s.translate(&graphene::Point::new(type_x as f32, (y + (ROW - th as f64) / 2.0) as f32));
        s.append_layout(&ty, &p.ink3);
        s.restore();
    }
    if metrics.foldable {
        let hidden = table.columns.len().saturating_sub(FOLDED_ROWS);
        let text = if unfolded {
            crate::i18n::tr("Show fewer")
        } else {
            crate::i18n::ntr("{} more column", "{} more columns", hidden as u32).replace("{}", &hidden.to_string())
        };
        let y = HEADER + shown as f64 * ROW;
        let l = layout(ctx, &text, &sans(12.5, pango::Weight::Medium));
        let (_, lh) = l.pixel_size();
        s.save();
        s.translate(&graphene::Point::new((PAD_X + CHIP_W + 8.0) as f32, (y + (ROW - lh as f64) / 2.0) as f32));
        s.append_layout(&l, &p.ink3);
        s.restore();
    }
    s.to_node()
}

/// A filled rounded rectangle: GSK has no such node, so it is a colour
/// under a rounded clip, which the GL and Vulkan renderers draw in one go.
pub fn fill_rounded(s: &gtk::Snapshot, rect: gsk::RoundedRect, color: &gtk::gdk::RGBA) {
    s.push_rounded_clip(&rect);
    s.append_color(color, rect.bounds());
    s.pop();
}
