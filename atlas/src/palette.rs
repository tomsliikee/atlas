//! The Insights Mono colours for drawing code (canvas, minimap, export),
//! read from the same tokens.json the stylesheet is built from.

use std::sync::OnceLock;

use atlas_core::tokens::{Theme, Tokens};
use gtk::gdk::RGBA;

#[derive(Clone, Debug)]
pub struct Palette {
    pub bg: RGBA,
    pub card: RGBA,
    pub fill: RGBA,
    pub line: RGBA,
    pub ink: RGBA,
    pub ink2: RGBA,
    pub ink3: RGBA,
    pub shadow: RGBA,
}

fn parse(colors: &std::collections::BTreeMap<String, String>, name: &str) -> RGBA {
    colors
        .get(name)
        .and_then(|v| RGBA::parse(v.as_str()).ok())
        .unwrap_or_else(|| panic!("tokens.json has no usable colour {name}"))
}

fn build(theme: Theme) -> Palette {
    let tokens = Tokens::parse(include_str!("../../insights-mono/tokens.json")).expect("bundled tokens");
    let c = tokens.colors(theme).expect("bundled tokens resolve");
    Palette {
        bg: parse(&c, "bg"),
        card: parse(&c, "card"),
        fill: parse(&c, "fill"),
        line: parse(&c, "line"),
        ink: parse(&c, "ink"),
        ink2: parse(&c, "ink2"),
        ink3: parse(&c, "ink3"),
        // shadow-bar's main layer, as one colour.
        shadow: if theme == Theme::Dark { RGBA::new(0.0, 0.0, 0.0, 0.5) } else { RGBA::new(0.0, 0.0, 0.0, 0.14) },
    }
}

pub fn get(dark: bool) -> &'static Palette {
    static LIGHT: OnceLock<Palette> = OnceLock::new();
    static DARK: OnceLock<Palette> = OnceLock::new();
    if dark { DARK.get_or_init(|| build(Theme::Dark)) } else { LIGHT.get_or_init(|| build(Theme::Light)) }
}

pub fn current() -> &'static Palette {
    get(adw::StyleManager::default().is_dark())
}

/// `color` with its alpha multiplied by `alpha`.
pub fn fade(color: &RGBA, alpha: f32) -> RGBA {
    RGBA::new(color.red(), color.green(), color.blue(), color.alpha() * alpha)
}
