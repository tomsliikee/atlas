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

