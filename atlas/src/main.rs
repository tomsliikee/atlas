mod app;
mod canvas;
mod db;
mod i18n;
mod motion;
mod palette;
mod runtime;
mod shots;
mod theme;
mod ui;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

pub const APP_ID: &str = "io.github.tomsliikee.Atlas";

/// Installed data (SQL highlighting, colour schemes): `/app/share/atlas` in
/// the Flatpak, `ATLAS_DATADIR` for a build run from the source tree.
pub fn data_dir() -> PathBuf {
    std::env::var_os("ATLAS_DATADIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/app/share/atlas"))
}

fn main() -> glib::ExitCode {
    i18n::init();
    gio::resources_register_include!("atlas.gresource").expect("bundled resources");
    app::build().run()
}
