//! Light, dark or the system's choice, kept in GSettings.

use adw::prelude::*;
use gtk::gio;

pub const KEY: &str = "color-scheme";

/// Puts the stored choice into effect.
pub fn apply(settings: &gio::Settings) {
    let scheme = match settings.string(KEY).as_str() {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// The choice that flips what is on screen now: from a light screen to dark
/// and back, whether the light came from the system or from Atlas.
pub fn flipped() -> &'static str {
    if adw::StyleManager::default().is_dark() { "light" } else { "dark" }
}
