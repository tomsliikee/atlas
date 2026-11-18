//! Every change Atlas makes to a database is shown as SQL first. This is
//! that dialog, plus the helpers that run a reviewed batch and report back.

use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;

use crate::db::{Progress, Session};
use crate::i18n::tr;
use crate::runtime;

/// A read-only SQL view, highlighted like the console.
pub fn sql_view(text: &str) -> (gtk::ScrolledWindow, sourceview5::Buffer) {
    let buffer = sourceview5::Buffer::new(None);
    if let Some(l) = sourceview5::LanguageManager::default().language("atlas-sql") {
        buffer.set_language(Some(&l));
    }
    let manager = sourceview5::StyleSchemeManager::default();
    manager.prepend_search_path(&crate::data_dir().join("sourceview/styles").to_string_lossy());
    let id = if adw::StyleManager::default().is_dark() { "atlas-dark" } else { "atlas-light" };
    if let Some(scheme) = manager.scheme(id) {
        buffer.set_style_scheme(Some(&scheme));
    }
    buffer.set_text(text);
    let view = sourceview5::View::builder()
        .buffer(&buffer)
        .monospace(true)
        .editable(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .css_classes(["ddl"])
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(160)
        .max_content_height(420)
        .propagate_natural_height(true)
        .css_classes(["ddl-frame"])
        .build();
    (scroll, buffer)
}

/// Shows `sql` under `heading` and calls `confirm` when the user agrees.
/// `destructive` makes the button say so in the theme's destructive style.
pub fn review(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    sql: &str,
    confirm_label: &str,
    destructive: bool,
    confirm: impl FnOnce() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    let (view, _) = sql_view(sql);
    view.set_size_request(520, -1);
    dialog.set_extra_child(Some(&view));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("ok", confirm_label);
    dialog.set_response_appearance(
        "ok",
        if destructive { adw::ResponseAppearance::Destructive } else { adw::ResponseAppearance::Suggested },
    );
    dialog.set_default_response(Some(if destructive { "cancel" } else { "ok" }));
    dialog.set_close_response("cancel");
    let confirm = std::cell::Cell::new(Some(confirm));
    dialog.connect_response(None, move |_, r| {
        if r == "ok"
            && let Some(f) = confirm.take()
        {
            f();
        }
    });
    dialog.present(Some(parent));
}

/// A plain question with two answers.
pub fn ask(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str, confirm_label: &str, confirm: impl FnOnce() + 'static) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("ok", confirm_label);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let confirm = std::cell::Cell::new(Some(confirm));
    dialog.connect_response(None, move |_, r| {
        if r == "ok"
            && let Some(f) = confirm.take()
        {
            f();
        }
    });
    dialog.present(Some(parent));
}

/// Shows an error the server returned, under a plain sentence.
pub fn failed(parent: &impl IsA<gtk::Widget>, heading: &str, error: &str) {
    let dialog = adw::AlertDialog::new(Some(heading), None);
    let detail = gtk::Label::builder()
        .label(error)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .css_classes(["error-detail"])
        .build();
    dialog.set_extra_child(Some(&detail));
    dialog.add_response("close", &tr("Close"));
    dialog.present(Some(parent));
}

/// Runs a reviewed batch in one transaction and calls `done` on the main
/// thread with the result.
pub fn run_batch(
    session: Session,
    statements: Vec<String>,
    foreign_keys_off: bool,
    done: impl FnOnce(Result<u64, String>) + 'static,
) {
    glib::spawn_future_local(async move {
        let result =
            runtime::run(async move { session.execute_all(statements, foreign_keys_off, Progress::default()).await }).await;
        done(result);
    });
}

/// Statements as one text for the review dialog.
pub fn script(statements: &[String]) -> String {
    let mut out = String::new();
    for s in statements {
        let s = s.trim().trim_end_matches(';');
        out.push_str(s);
        out.push_str(";\n");
    }
    out
}

/// A window-modal progress dialog with a stop button, for imports and
/// backups. Returns the dialog and the label to update.
pub fn progress_dialog(parent: &impl IsA<gtk::Widget>, heading: &str, progress: &Progress) -> (adw::AlertDialog, gtk::Label) {
    let dialog = adw::AlertDialog::new(Some(heading), None);
    let label = gtk::Label::builder().label(tr("Starting…")).css_classes(["hint"]).build();
    let bar = gtk::ProgressBar::builder().pulse_step(0.1).build();
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    content.append(&bar);
    content.append(&label);
    dialog.set_extra_child(Some(&content));
    dialog.add_response("stop", &tr("Stop"));
    dialog.set_close_response("stop");
    dialog.set_can_close(false);
    let p = progress.clone();
    dialog.connect_response(None, move |_, _| p.cancel());
    let weak = bar.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(120), move || match weak.upgrade() {
        Some(bar) => {
            bar.pulse();
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    dialog.present(Some(parent));
    (dialog, label)
}

