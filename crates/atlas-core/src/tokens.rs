//! Insights Mono design tokens, read from `insights-mono/tokens.json` and
//! turned into GTK CSS custom properties. The app's build script calls
//! [`css_variables`] for the light and the dark theme, so no colour or size
//! in the stylesheet is typed by hand.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use serde::Deserialize;
use serde_json::Value;

/// The two themes Atlas uses. The tinted themes of the design system belong
/// to Flex and are left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn id(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

#[derive(Debug)]
pub enum TokenError {
    Json(serde_json::Error),
    /// A colour has no value for the theme, refers to a token that does not
    /// exist, or refers to itself through a chain of references.
    Color(String),
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::Json(e) => write!(f, "tokens.json: {e}"),
            TokenError::Color(name) => write!(f, "colour token cannot be resolved: {name}"),
        }
    }
}

impl std::error::Error for TokenError {}

#[derive(Deserialize)]
struct File {
    color: ColorGroup,
    shadow: ColorGroup,
    spacing: ScaleGroup,
    radius: ScaleGroup,
    size: ScaleGroup,
}

#[derive(Deserialize)]
struct ColorGroup {
    tokens: Vec<NamedValue>,
}

#[derive(Deserialize)]
struct ScaleGroup {
    tokens: Vec<NamedValue>,
}

#[derive(Debug, Deserialize)]
struct NamedValue {
    name: String,
    value: Value,
}

/// The tokens Atlas needs, already resolved per theme.
#[derive(Debug)]
pub struct Tokens {
    colors: Vec<NamedValue>,
    /// Shadows differ per theme, like colours.
    shadows: Vec<NamedValue>,
    /// Spacing, radius and size tokens. They are the same in every theme.
    scales: BTreeMap<String, String>,
}

impl Tokens {
    pub fn parse(json: &str) -> Result<Self, TokenError> {
        let file: File = serde_json::from_str(json).map_err(TokenError::Json)?;
        let scales = [file.spacing, file.radius, file.size]
            .into_iter()
            .flat_map(|group| group.tokens)
            .filter_map(|t| t.value.as_str().map(|v| (t.name, v.to_owned())))
            .collect();
        Ok(Tokens { colors: file.color.tokens, shadows: file.shadow.tokens, scales })
    }

    /// Every colour of `theme`, with `{other}` references followed.
    pub fn colors(&self, theme: Theme) -> Result<BTreeMap<String, String>, TokenError> {
        self.colors
            .iter()
            .map(|t| Ok((t.name.clone(), self.resolve(&t.name, theme, 0)?)))
            .collect()
    }

    fn resolve(&self, name: &str, theme: Theme, depth: usize) -> Result<String, TokenError> {
        // A chain longer than the token list can only be a cycle.
        if depth > self.colors.len() {
            return Err(TokenError::Color(name.to_owned()));
        }
        let token = self
            .colors
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| TokenError::Color(name.to_owned()))?;
        let value = match &token.value {
            Value::String(s) => s.as_str(),
            Value::Object(map) => map
                .get(theme.id())
                .and_then(Value::as_str)
                .ok_or_else(|| TokenError::Color(name.to_owned()))?,
            _ => return Err(TokenError::Color(name.to_owned())),
        };
        match value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
            Some(reference) => self.resolve(reference, theme, depth + 1),
            None => Ok(value.to_owned()),
        }
    }
}

/// A `:root { … }` block with every token as `--im-<name>`, followed by the
/// libadwaita colour variables mapped onto them, so stock widgets (header
/// bar, popovers, entries) take the Insights Mono look without extra rules.
pub fn css_variables(tokens: &Tokens, theme: Theme) -> Result<String, TokenError> {
    let colors = tokens.colors(theme)?;
    let mut css = String::from(":root {\n");
    for (name, value) in &colors {
        let _ = writeln!(css, "  --im-{name}: {value};");
    }
    for t in &tokens.shadows {
        if let Some(v) = t.value.get(theme.id()).and_then(Value::as_str) {
            let _ = writeln!(css, "  --im-{}: {v};", t.name);
        }
    }
    for (name, value) in &tokens.scales {
        let _ = writeln!(css, "  --im-{name}: {value};");
    }
    for (adw, im) in ADWAITA_MAP {
        let _ = writeln!(css, "  --{adw}: var(--im-{im});");
    }
    css.push_str("}\n");
    Ok(css)
}

/// libadwaita's named colours and the Insights Mono token each one takes.
/// The accent is ink itself: in a black and white system the thing that acts
/// is the solid one.
const ADWAITA_MAP: &[(&str, &str)] = &[
    ("window-bg-color", "bg"),
    ("window-fg-color", "ink"),
    ("view-bg-color", "bg"),
    ("view-fg-color", "ink"),
    ("headerbar-bg-color", "bg"),
    ("headerbar-fg-color", "ink"),
    ("headerbar-backdrop-color", "bg"),
    ("sidebar-bg-color", "bg"),
    ("sidebar-fg-color", "ink"),
    ("sidebar-backdrop-color", "bg"),
    ("card-bg-color", "card"),
    ("card-fg-color", "ink"),
    ("popover-bg-color", "bar"),
    ("popover-fg-color", "ink"),
    ("dialog-bg-color", "bar"),
    ("dialog-fg-color", "ink"),
    ("accent-bg-color", "solid"),
    ("accent-fg-color", "on-solid"),
    ("accent-color", "ink"),
    ("destructive-bg-color", "down"),
    ("destructive-color", "down"),
];

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
      "color": {"tokens": [
        {"name": "bg", "value": {"light": "#ffffff", "dark": "#000000"}},
        {"name": "ink", "value": {"light": "#0c0c0e", "dark": "#f5f5f7"}},
        {"name": "solid", "value": "{ink}"},
        {"name": "loop-a", "value": "{loop-b}"},
        {"name": "loop-b", "value": "{loop-a}"}
      ]},
      "shadow": {"tokens": [{"name": "shadow-bar", "value": {"light": "0 1px 2px black", "dark": "0 1px 2px white"}}]},
      "spacing": {"tokens": [{"name": "space-card", "value": "18px"}]},
      "radius": {"tokens": [{"name": "radius-card", "value": "22px"}]},
      "size": {"tokens": [{"name": "size-pill", "value": "44px"}]}
    }"##;

    #[test]
    fn follows_references_per_theme() {
        let tokens = Tokens::parse(SAMPLE).unwrap();
        assert_eq!(tokens.resolve("solid", Theme::Light, 0).unwrap(), "#0c0c0e");
        assert_eq!(tokens.resolve("solid", Theme::Dark, 0).unwrap(), "#f5f5f7");
    }

    #[test]
    fn a_cycle_is_an_error_not_a_hang() {
        let tokens = Tokens::parse(SAMPLE).unwrap();
        assert!(tokens.resolve("loop-a", Theme::Light, 0).is_err());
    }

    #[test]
    fn the_real_file_resolves_in_both_themes() {
        let json = include_str!("../../../insights-mono/tokens.json");
        let tokens = Tokens::parse(json).unwrap();
        for theme in [Theme::Light, Theme::Dark] {
            let css = css_variables(&tokens, theme).unwrap();
            assert!(css.contains("--im-bg:"));
            assert!(css.contains("--im-radius-card: 22px;"));
            assert!(css.contains("--im-shadow-bar: 0 14px"));
            assert!(css.contains("--accent-bg-color: var(--im-solid);"));
            // Every variable the libadwaita map points at must exist.
            for (_, im) in ADWAITA_MAP {
                assert!(css.contains(&format!("--im-{im}:")), "missing token {im}");
            }
        }
    }
}
