//! Translations through gettext. Strings are written in English in the code
//! and looked up in `po/<lang>.po`, compiled into the Flatpak.

pub const DOMAIN: &str = "atlas";

/// GTK sets the locale itself when it starts; this only binds the domain.
pub fn init() {
    let dir = std::env::var("ATLAS_LOCALEDIR").unwrap_or_else(|_| "/app/share/locale".into());
    let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

pub fn tr(text: &str) -> String {
    gettextrs::gettext(text)
}

pub fn ntr(one: &str, many: &str, n: u32) -> String {
    gettextrs::ngettext(one, many, n)
}
