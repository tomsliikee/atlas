//! The first page: open something, or pick up what was open before.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use atlas_core::store::Connection;
use gtk::glib;

use crate::i18n::tr;

pub enum Pick {
    New,
    File,
    Server(Connection),
    Edit(Connection),
    Forget(Connection),
    Recent(PathBuf),
    ForgetFile(PathBuf),
}

pub struct Start {
    pub widget: gtk::ScrolledWindow,
    recent: gtk::ListBox,
    recent_group: gtk::Box,
    pick: Rc<dyn Fn(Pick)>,
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build()
}

impl Start {
    pub fn new(pick: impl Fn(Pick) + 'static) -> Self {
        let pick: Rc<dyn Fn(Pick)> = Rc::new(pick);
        let statement = gtk::Label::builder().label(tr("Open a database")).css_classes(["statement"]).build();
        let hint = gtk::Label::builder()
            .label(tr("MariaDB, MySQL, PostgreSQL, SQLite or an SQL dump. Atlas only reads."))
            .css_classes(["hint"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let connect = gtk::Button::builder().label(tr("Connect")).css_classes(["solid"]).build();
        let open = gtk::Button::builder().label(tr("Open File")).css_classes(["quiet"]).build();
        let p = pick.clone();
        connect.connect_clicked(move |_| p(Pick::New));
        let p = pick.clone();
        open.connect_clicked(move |_| p(Pick::File));
        let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).margin_top(16).build();
        buttons.append(&connect);
        buttons.append(&open);

        let recent = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list", "recent"]).build();
        let label = gtk::Label::builder().label(tr("Recent")).xalign(0.0).css_classes(["group-label"]).build();
        let recent_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(40).build();
        recent_group.append(&label);
        recent_group.append(&recent);

        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).valign(gtk::Align::Center).build();
        column.append(&statement);
        column.append(&hint);
        column.append(&buttons);
        column.append(&recent_group);
        let clamp = adw::Clamp::builder().maximum_size(560).child(&column).margin_top(48).margin_bottom(48).margin_start(16).margin_end(16).build();
        let widget = gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).build();
        Start { widget, recent, recent_group, pick }
    }

    pub fn set_recent(&self, connections: &[Connection], files: &[PathBuf]) {
        while let Some(child) = self.recent.first_child() {
            self.recent.remove(&child);
        }
        for c in connections {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&c.name))
                .subtitle(glib::markup_escape_text(&c.subtitle()))
                .activatable(true)
                .build();
            let kind = match c.dialect {
                atlas_core::sql::Dialect::MySql => "MariaDB",
                atlas_core::sql::Dialect::Postgres => "PostgreSQL",
                atlas_core::sql::Dialect::Sqlite => "SQLite",
            };
            row.add_prefix(&gtk::Label::builder().label(kind).width_chars(9).xalign(0.0).css_classes(["kind"]).build());
            let edit = icon_button("document-edit-symbolic", &tr("Edit"));
            let forget = icon_button("user-trash-symbolic", &tr("Forget"));
            row.add_suffix(&edit);
            row.add_suffix(&forget);
            let (p, conn) = (self.pick.clone(), c.clone());
            row.connect_activated(move |_| p(Pick::Server(conn.clone())));
            let (p, conn) = (self.pick.clone(), c.clone());
            edit.connect_clicked(move |_| p(Pick::Edit(conn.clone())));
            let (p, conn) = (self.pick.clone(), c.clone());
            forget.connect_clicked(move |_| p(Pick::Forget(conn.clone())));
            self.recent.append(&row);
        }
        for f in files {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name))
                .subtitle(glib::markup_escape_text(&f.parent().map(|p| p.display().to_string()).unwrap_or_default()))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Label::builder().label(tr("File")).width_chars(9).xalign(0.0).css_classes(["kind"]).build());
            let forget = icon_button("user-trash-symbolic", &tr("Forget"));
            row.add_suffix(&forget);
            let (p, path) = (self.pick.clone(), f.clone());
            row.connect_activated(move |_| p(Pick::Recent(path.clone())));
            let (p, path) = (self.pick.clone(), f.clone());
            forget.connect_clicked(move |_| p(Pick::ForgetFile(path.clone())));
            self.recent.append(&row);
        }
        self.recent_group.set_visible(!connections.is_empty() || !files.is_empty());
    }
}
