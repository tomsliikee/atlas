//! The diagram as PNG, SVG, PDF or Mermaid text.
//!
//! All three images come from the same render node the screen uses, at
//! zoom 1: PNG through the window's own GPU renderer at 2x, SVG and PDF
//! through cairo, which keeps text and lines as vectors.

use std::path::Path;

use gtk::prelude::*;
use gtk::{gio, graphene, gsk};

use crate::canvas::Canvas;
use crate::i18n::tr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Svg,
    Pdf,
    Mermaid,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Svg => "svg",
            Format::Pdf => "pdf",
            Format::Mermaid => "mmd",
        }
    }
}

/// Asks where to save, then writes. Errors and success come back as text.
pub fn save(window: &impl IsA<gtk::Window>, canvas: &Canvas, format: Format, name: &str, done: impl Fn(Result<String, String>) + 'static) {
    let dialog = gtk::FileDialog::builder()
        .title(tr("Export Diagram"))
        .initial_name(format!("{name}.{}", format.extension()))
        .modal(true)
        .build();
    let canvas = canvas.clone();
    let parent = window.clone().upcast::<gtk::Window>();
    dialog.save(Some(window), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else {
            done(Err(tr("This location cannot be written to.")));
            return;
        };
        done(write(&parent, &canvas, format, &path).map(|()| path.display().to_string()));
    });
}

pub fn mermaid(canvas: &Canvas) -> Option<String> {
    let schema = canvas.schema()?;
    Some(atlas_core::mermaid::er_diagram(&schema, canvas.show_inferred()))
}

pub fn write(window: &gtk::Window, canvas: &Canvas, format: Format, path: &Path) -> Result<(), String> {
    if format == Format::Mermaid {
        let text = mermaid(canvas).ok_or_else(|| tr("Nothing to export."))?;
        return std::fs::write(path, text).map_err(|e| e.to_string());
    }
    let dark = adw::StyleManager::default().is_dark();
    let (node, bounds) = canvas.export_node(dark).ok_or_else(|| tr("Nothing to export."))?;
    let (w, h) = (bounds.width() as f64, bounds.height() as f64);
    match format {
        Format::Png => {
            // 2x, but no side beyond 16k pixels, the common texture limit.
            let scale = (2.0_f32).min(16_000.0 / bounds.width().max(bounds.height()));
            let scaled = gsk::TransformNode::new(&node, Some(&gsk::Transform::new().scale(scale, scale)));
            let out = graphene::Rect::new(bounds.x() * scale, bounds.y() * scale, bounds.width() * scale, bounds.height() * scale);
            let renderer = window.native().and_then(|n| n.renderer()).ok_or_else(|| tr("No renderer."))?;
            let texture = renderer.render_texture(&scaled, Some(&out));
            texture.save_to_png(path).map_err(|e| e.to_string())
        }
        Format::Svg => {
            let surface = gtk::cairo::SvgSurface::new(w, h, Some(path)).map_err(|e| e.to_string())?;
            draw_to(&surface, &node, &bounds)?;
            surface.finish();
            Ok(())
        }
        Format::Pdf => {
            let surface = gtk::cairo::PdfSurface::new(w, h, path).map_err(|e| e.to_string())?;
            draw_to(&surface, &node, &bounds)?;
            surface.finish();
            Ok(())
        }
        Format::Mermaid => unreachable!("handled above"),
    }
}

fn draw_to(surface: &gtk::cairo::Surface, node: &gsk::RenderNode, bounds: &graphene::Rect) -> Result<(), String> {
    let cr = gtk::cairo::Context::new(surface).map_err(|e| e.to_string())?;
    cr.translate(-bounds.x() as f64, -bounds.y() as f64);
    node.draw(&cr);
    Ok(())
}


/// Row formats for "Save as…".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rows {
    Csv,
    Json,
    Sql,
}

impl Rows {
    fn extension(self) -> &'static str {
        match self {
            Rows::Csv => "csv",
            Rows::Json => "json",
            Rows::Sql => "sql",
        }
    }
}

/// Where the rows come from: what the grid holds, or a whole table read
/// page by page.
pub enum Source {
    Loaded(Vec<String>, Vec<Vec<Option<String>>>),
    Table {
        session: crate::db::Session,
        table: String,
        filters: Vec<atlas_core::sql::Filter>,
        sort: Option<atlas_core::sql::Sort>,
        types: Vec<String>,
    },
}

const EXPORT_PAGE: u64 = 2000;

/// Asks for a file, then writes the rows there.
pub fn save_rows(
    parent: &impl IsA<gtk::Widget>,
    ctx: &std::rc::Rc<super::Ctx>,
    name: &str,
    format: Rows,
    dialect: atlas_core::sql::Dialect,
    source: Source,
) {
    let Some(window) = parent.root().and_downcast::<gtk::Window>() else { return };
    let dialog = gtk::FileDialog::builder()
        .title(tr("Save Rows"))
        .initial_name(format!("{name}.{}", format.extension()))
        .modal(true)
        .build();
    let ctx = ctx.clone();
    let name = name.to_owned();
    dialog.save(Some(&window), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else {
            ctx.toast(&tr("This location cannot be written to."));
            return;
        };
        gtk::glib::spawn_future_local(async move {
            let written = crate::runtime::run(async move { write_rows(&path, &name, format, dialect, source).await }).await;
            match written {
                Ok(n) => ctx.toast(&crate::i18n::ntr("Saved {} row", "Saved {} rows", n as u32).replace("{}", &n.to_string())),
                Err(e) => ctx.toast(&e),
            }
        });
    });
}

async fn write_rows(
    path: &Path,
    name: &str,
    format: Rows,
    dialect: atlas_core::sql::Dialect,
    source: Source,
) -> Result<usize, String> {
    use atlas_core::sql;
    use std::io::Write as _;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut total = 0;
    let mut first = true;
    let mut emit = |columns: &[String], types: &[String], rows: &[Vec<Option<String>>], last: bool| -> Result<(), String> {
        let text = match format {
            Rows::Csv => {
                let t = sql::csv(columns, rows);
                // The header only once.
                if first { t } else { t.split_once("\r\n").map(|(_, r)| r.to_owned()).unwrap_or_default() }
            }
            Rows::Sql if rows.is_empty() => String::new(),
            Rows::Sql => sql::insert_batch(dialect, name, columns, types, rows),
            Rows::Json => {
                let body = sql::json(columns, rows);
                let inner = body.trim().trim_start_matches('[').trim_end_matches(']').trim().to_owned();
                let mut t = String::new();
                if first {
                    t.push_str("[\n");
                }
                if !inner.is_empty() {
                    if !first && total > 0 {
                        t.push_str(",\n");
                    }
                    t.push_str("  ");
                    t.push_str(&inner);
                }
                if last {
                    t.push_str("\n]\n");
                }
                t
            }
        };
        out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        total += rows.len();
        first = false;
        Ok(())
    };
    match source {
        Source::Loaded(columns, rows) => emit(&columns, &[], &rows, true)?,
        Source::Table { session, table, filters, sort, types } => {
            let mut offset = 0;
            loop {
                let q = sql::page(session.dialect, &table, &filters, sort.as_ref(), EXPORT_PAGE, offset);
                let set = session.query(q, EXPORT_PAGE as usize).await?;
                let last = (set.rows.len() as u64) < EXPORT_PAGE;
                let columns = if set.columns.is_empty() { Vec::new() } else { set.columns.clone() };
                if set.rows.is_empty() && offset > 0 {
                    emit(&columns, &types, &[], true)?;
                    break;
                }
                emit(&columns, &types, &set.rows, last)?;
                offset += set.rows.len() as u64;
                if last {
                    break;
                }
            }
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    Ok(total)
}
