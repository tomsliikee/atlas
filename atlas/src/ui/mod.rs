//! Everything the window shows around the canvas.

pub mod admin;
pub mod compare;
pub mod complete;
pub mod connect;
pub mod console;
pub mod data;
pub mod editor;
pub mod export;
pub mod grid;
pub mod import;
pub mod minimap;
pub mod review;
pub mod search;
pub mod sidebar;
pub mod start;
pub mod structure;
pub mod window;

use std::cell::RefCell;
use std::path::PathBuf;

use atlas_core::schema::Schema;
use gtk::glib;

use crate::db::Session;

/// What the window has open, shared by its parts.
#[derive(Default)]
pub struct Ctx {
    pub session: RefCell<Option<Session>>,
    pub schema: RefCell<Option<Schema>>,
    /// Key of the layout and history files of what is open.
    pub key: RefCell<Option<String>>,
    pub toasts: RefCell<Option<adw::ToastOverlay>>,
    /// Called after the structure changed (DDL ran): reload the schema.
    pub on_schema_changed: RefCell<Option<std::rc::Rc<dyn Fn()>>>,
    /// Called after rows changed: reload what shows them.
    pub on_rows_changed: RefCell<Option<std::rc::Rc<dyn Fn()>>>,
}

impl Ctx {
    pub fn schema_changed(&self) {
        let f = self.on_schema_changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    pub fn rows_changed(&self) {
        let f = self.on_rows_changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// True when the open session may write.
    pub fn writable(&self) -> bool {
        self.session.borrow().as_ref().is_some_and(|s| s.access == crate::db::Access::ReadWrite)
    }

    pub fn toast(&self, text: &str) {
        if let Some(t) = self.toasts.borrow().as_ref() {
            let toast = adw::Toast::new(&glib::markup_escape_text(text));
            toast.set_timeout(5);
            t.add_toast(toast);
        }
    }
}

/// `~/.var/app/io.github.tomsliikee.Atlas/config/atlas` in the Flatpak.
/// `ATLAS_CONFIG_DIR` points tests and screenshot runs elsewhere (Flatpak
/// sets `XDG_CONFIG_HOME` itself, so that cannot be used).
pub fn config_dir() -> PathBuf {
    std::env::var_os("ATLAS_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| glib::user_config_dir().join("atlas"))
}
