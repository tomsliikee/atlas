//! The structure of one table: columns, indexes, keys, checks, triggers and
//! its CREATE statement. With a writable session it also offers the table
//! editor and dropping the table.

use atlas_core::ddl;
use atlas_core::schema::{RelationKind, Schema, TableKind};
use atlas_core::sql::Dialect;
use gtk::prelude::*;

use crate::i18n::tr;

fn section(parent: &gtk::Box, title: &str) -> gtk::ListBox {
    parent.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["section-title"]).build());
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["structure-list"]).build();
    parent.append(&list);
    list
}

fn line(list: &gtk::ListBox, name: &str, detail: &str) {
    let row = gtk::Box::builder().spacing(12).build();
    row.append(&gtk::Label::builder().label(name).xalign(0.0).selectable(true).css_classes(["strong"]).build());
    row.append(
        &gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .css_classes(["mono", "hint"])
            .build(),
    );
    list.append(&row);
}

pub struct Actions {
    pub edit: Box<dyn Fn()>,
    pub drop: Box<dyn Fn()>,
}

/// The page for `table` of `schema`, written as `dialect` DDL.
pub fn build(schema: &Schema, table: &str, dialect: Dialect, actions: Option<Actions>) -> gtk::Widget {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["structure"]).build();
    let Some(t) = schema.table(table) else {
        return page.upcast();
    };

    if let Some(actions) = actions {
        let bar = gtk::Box::builder().spacing(8).margin_bottom(4).build();
        let edit = gtk::Button::builder().label(tr("Edit Table…")).css_classes(["solid", "small"]).build();
        let drop = gtk::Button::builder().label(tr("Drop…")).css_classes(["quiet", "small"]).build();
        edit.set_visible(t.kind == TableKind::Table);
        let Actions { edit: e, drop: d } = actions;
        edit.connect_clicked(move |_| e());
        drop.connect_clicked(move |_| d());
        bar.append(&edit);
        bar.append(&drop);
        page.append(&bar);
    }

    let cols = section(&page, &tr("Columns"));
    for c in &t.columns {
        let mut facts = vec![c.data_type.clone()];
        if c.primary {
            facts.push("PRIMARY KEY".into());
        }
        if !c.nullable && !c.primary {
            facts.push("NOT NULL".into());
        }
        if c.auto_increment {
            facts.push("AUTO".into());
        }
        if let Some(d) = &c.default {
            facts.push(format!("DEFAULT {d}"));
        }
        if let Some(g) = &c.generated {
            facts.push(format!("AS ({g})"));
        }
        if let Some(v) = &c.enum_values
            && !c.data_type.to_ascii_lowercase().starts_with("enum(")
        {
            facts.push(format!("{{{}}}", v.join(", ")));
        }
        if let Some(cm) = &c.comment {
            facts.push(format!("— {cm}"));
        }
        line(&cols, &c.name, &facts.join(" · "));
    }

    let uniques: Vec<_> = t.unique.iter().collect();
    if !t.indexes.is_empty() || !uniques.is_empty() {
        let list = section(&page, &tr("Indexes"));
        for u in uniques {
            line(&list, &tr("unique"), &u.join(", "));
        }
        for i in &t.indexes {
            let mut d = i.columns.join(", ");
            if i.unique {
                d = format!("UNIQUE · {d}");
            }
            if !i.kind.is_empty() {
                d = format!("{d} · {}", i.kind);
            }
            line(&list, &i.name, &d);
        }
    }

    let outgoing: Vec<_> = schema.relations.iter().filter(|r| r.from_table == table).collect();
    let incoming: Vec<_> = schema.relations.iter().filter(|r| r.to_table == table && r.kind == RelationKind::Declared).collect();
    if !outgoing.is_empty() {
        let list = section(&page, &tr("References"));
        for r in outgoing {
            let mut d = format!("{} → {}({})", r.from_columns.join(", "), r.to_table, r.to_columns.join(", "));
            if let Some(a) = r.on_delete {
                d.push_str(&format!(" · ON DELETE {}", a.sql()));
            }
            if let Some(a) = r.on_update {
                d.push_str(&format!(" · ON UPDATE {}", a.sql()));
            }
            if r.kind == RelationKind::Inferred {
                d.push_str(&format!(" · {}", tr("inferred")));
            }
            line(&list, r.name.as_deref().unwrap_or("—"), &d);
        }
    }
    if !incoming.is_empty() {
        let list = section(&page, &tr("Referenced by"));
        for r in incoming {
            line(&list, &r.from_table, &format!("{} → {}", r.from_columns.join(", "), r.to_columns.join(", ")));
        }
    }
    if !t.checks.is_empty() {
        let list = section(&page, &tr("Checks"));
        for c in &t.checks {
            line(&list, c.name.as_deref().unwrap_or("—"), &c.expr);
        }
    }
    if !t.triggers.is_empty() {
        let list = section(&page, &tr("Triggers"));
        for tr_ in &t.triggers {
            line(&list, &tr_.name, &format!("{} {}", tr_.timing, tr_.event));
        }
    }

    page.append(&gtk::Label::builder().label(tr("Definition")).xalign(0.0).css_classes(["section-title"]).build());
    let mut text = if t.kind == TableKind::View {
        ddl::create_view(dialect, t)
    } else {
        ddl::create_table(dialect, t, &schema.relations, true)
    };
    for i in &t.indexes {
        text.push_str(&ddl::create_index(dialect, &t.name, i));
    }
    for tr_ in &t.triggers {
        text.push('\n');
        text.push_str(tr_.body.trim());
        text.push('\n');
    }
    let (view, _) = super::review::sql_view(&text);
    view.set_max_content_height(-1);
    view.set_propagate_natural_height(true);
    page.append(&view);
    let copy = gtk::Button::builder().label(tr("Copy DDL")).halign(gtk::Align::Start).margin_top(8).css_classes(["quiet", "small"]).build();
    copy.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
    });
    page.append(&copy);

    gtk::ScrolledWindow::builder().child(&page).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build().upcast()
}

/// A routine or view definition, read-only.
pub fn definition(text: &str) -> gtk::Widget {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["structure"]).build();
    let (view, _) = super::review::sql_view(text);
    view.set_max_content_height(-1);
    view.set_vexpand(true);
    page.append(&view);
    page.upcast()
}
