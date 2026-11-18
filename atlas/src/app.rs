//! The application object: one window, the shortcuts.

use adw::prelude::*;
use gtk::gio;

use crate::{APP_ID, shots, theme, ui};

pub fn build() -> adw::Application {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        // libadwaita loads style.css from here by itself.
        .resource_base_path("/io/github/tomsliikee/Atlas")
        .build();

    app.connect_startup(|app| {
        gtk::Window::set_default_icon_name(APP_ID);
        theme::apply(&gio::Settings::new(APP_ID));
        for (action, keys) in [
            ("win.toggle-theme", &["<Ctrl><Shift>l"][..]),
            ("win.search", &["<Ctrl>k", "<Ctrl>f"]),
            ("win.console", &["<Ctrl>e"]),
            ("win.toggle-sidebar", &["F9"]),
            ("win.open-file", &["<Ctrl>o"]),
            ("win.new-connection", &["<Ctrl>n"]),
            ("win.close-db", &["<Ctrl>w"]),
            ("win.refresh", &["F5", "<Ctrl>r"]),
            ("win.fit", &["<Ctrl>0"]),
            ("win.zoom-reset", &["<Ctrl>1"]),
            ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
            ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
            ("win.tidy", &["<Ctrl>l"]),
            ("win.show-inferred", &["<Ctrl>i"]),
            ("win.minimap", &["<Ctrl>m"]),
            ("app.quit", &["<Ctrl>q"]),
        ] {
            app.set_accels_for_action(action, keys);
        }
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &adw::Application, _, _| app.quit())
            .build();
        app.add_action_entries([quit]);
    });

    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let win = ui::window::build(app);
        win.window.present();
        if let Some(dir) = shots::requested() {
            shots::run(win, dir);
        }
    });

    app
}
