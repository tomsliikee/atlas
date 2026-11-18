//! Development screenshots without compositor rights. With
//! `ATLAS_SCREENSHOT=<dir>` the window renders itself through its own GSK
//! renderer into PNGs at twice the logical size, walking through the app:
//! start page, then (with `ATLAS_OPEN=<file>`) the diagram, a focused and
//! unfolded table (`ATLAS_FOCUS=<table>`), the panel growing out of it, the
//! rows, search, the SQL console, and the same in dark. Then it quits and
//! the stored theme choice is restored. Without the variable nothing here
//! runs.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};

use crate::ui::window::Win;

const SCALE: f32 = 2.0;

pub fn requested() -> Option<PathBuf> {
    std::env::var_os("ATLAS_SCREENSHOT").map(PathBuf::from)
}

/// Saves what `widget` shows now as `<dir>/<name>.png`.
fn save(widget: &impl IsA<gtk::Widget>, dir: &Path, name: &str) {
    let widget = widget.as_ref();
    let (w, h) = (widget.width() as f32, widget.height() as f32);
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(SCALE, SCALE);
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let Some(node) = snapshot.to_node() else {
        eprintln!("atlas: {name}: nothing to render");
        return;
    };
    let Some(renderer) = widget.native().and_then(|n| n.renderer()) else {
        eprintln!("atlas: {name}: no renderer");
        return;
    };
    let bounds = graphene::Rect::new(0.0, 0.0, w * SCALE, h * SCALE);
    let texture: gdk::Texture = renderer.render_texture(&node, Some(&bounds));
    let path = dir.join(format!("{name}.png"));
    match texture.save_to_png(&path) {
        Ok(()) => println!("atlas: wrote {}", path.display()),
        Err(e) => eprintln!("atlas: {}: {e}", path.display()),
    }
}

enum Step {
    Shot(&'static str),
    Act(&'static str, Option<String>),
    Wait(u64),
    Theme(adw::ColorScheme),
}

pub fn run(win: Rc<Win>, dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    let open = std::env::var("ATLAS_OPEN").ok();
    let focus = std::env::var("ATLAS_FOCUS").ok();
    let search = std::env::var("ATLAS_SEARCH").unwrap_or_else(|_| "id".into());
    let sql = std::env::var("ATLAS_SQL").ok();
    let export = std::env::var("ATLAS_EXPORT_DIR").ok();

    let mut steps = vec![Step::Wait(700), Step::Shot("start-light")];
    if let Some(path) = open {
        steps.extend([Step::Act("open-path", Some(path)), Step::Wait(2200), Step::Shot("diagram-light")]);
        if let Some(t) = focus {
            steps.extend([
                Step::Act("focus", Some(t.clone())),
                Step::Wait(1100),
                Step::Shot("focus-light"),
                Step::Act("unfold", Some(t.clone())),
                Step::Wait(900),
                Step::Shot("unfold-light"),
                Step::Act("data", Some(t.clone())),
                Step::Wait(120),
                Step::Shot("panel-grow"),
                Step::Wait(1300),
                Step::Shot("data-light"),
                Step::Act("close-panel", None),
                Step::Wait(600),
                Step::Act("data-filter", Some("orders|customer_id|25".into())),
                Step::Wait(1300),
                Step::Shot("filter-light"),
                Step::Act("close-panel", None),
                Step::Wait(600),
                Step::Act("search-text", Some(search)),
                Step::Wait(500),
                Step::Shot("search-light"),
                Step::Act("search", None),
                Step::Wait(400),
            ]);
            if let Some(q) = sql {
                steps.extend([Step::Act("console-sql", Some(q)), Step::Wait(1500), Step::Shot("console-light"), Step::Act("close-panel", None), Step::Wait(600)]);
            }
            if let Some(dir) = export {
                for ext in ["png", "svg", "pdf", "mmd"] {
                    steps.push(Step::Act("export-to", Some(format!("{dir}/diagram.{ext}"))));
                }
            }
            steps.extend([
                Step::Theme(adw::ColorScheme::ForceDark),
                Step::Wait(700),
                Step::Shot("focus-dark"),
                Step::Act("data", Some(t)),
                Step::Wait(1400),
                Step::Shot("data-dark"),
                Step::Act("close-panel", None),
                Step::Wait(600),
                Step::Act("fit", None),
                Step::Wait(900),
                Step::Shot("diagram-dark"),
            ]);
        } else {
            steps.extend([Step::Theme(adw::ColorScheme::ForceDark), Step::Wait(700), Step::Shot("diagram-dark")]);
        }
    } else {
        steps.extend([Step::Theme(adw::ColorScheme::ForceDark), Step::Wait(500), Step::Shot("start-dark")]);
    }
    // With ATLAS_FAIL=host:port:user:database a connection that fails, to
    // show the explanation (it needs a test user without password).
    if let Ok(spec) = std::env::var("ATLAS_FAIL") {
        steps.extend([
            Step::Theme(adw::ColorScheme::ForceLight),
            Step::Act("close-db", None),
            Step::Wait(400),
            Step::Act("connect-spec", Some(spec)),
            Step::Wait(2500),
            Step::Shot("failed-light"),
        ]);
        next(win, Rc::new(dir), Rc::new(steps), 0);
        return;
    }
    // Last, because a dialog stays open until the app quits.
    steps.extend([
        Step::Theme(adw::ColorScheme::ForceLight),
        Step::Act("close-db", None),
        Step::Wait(500),
        Step::Act("new-connection", None),
        Step::Wait(1600),
        Step::Shot("connect-light"),
    ]);
    next(win, Rc::new(dir), Rc::new(steps), 0);
}

fn next(win: Rc<Win>, dir: Rc<PathBuf>, steps: Rc<Vec<Step>>, i: usize) {
    let Some(step) = steps.get(i) else {
        gio::Settings::new(crate::APP_ID).reset(crate::theme::KEY);
        if let Some(app) = GtkWindowExt::application(&win.window) {
            app.quit();
        }
        return;
    };
    let delay = match step {
        Step::Shot(name) => {
            save(&win.window, &dir, name);
            0
        }
        Step::Act(name, arg) => {
            let variant = arg.as_ref().map(|a| a.to_variant());
            if let Err(e) = WidgetExt::activate_action(&win.window, &format!("win.{name}"), variant.as_ref()) {
                eprintln!("atlas: {name}: {e}");
            }
            0
        }
        Step::Wait(ms) => *ms,
        Step::Theme(scheme) => {
            adw::StyleManager::default().set_color_scheme(*scheme);
            0
        }
    };
    glib::timeout_add_local_once(Duration::from_millis(delay.max(16)), move || next(win, dir, steps, i + 1));
}
