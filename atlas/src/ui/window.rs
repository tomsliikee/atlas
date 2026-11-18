//! The main window: start page, or the diagram with its sidebar, search,
//! minimap, zoom pill and the data panel floating on the right.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use atlas_core::schema::Schema;
use atlas_core::store::{self, Connection, Layout, Recent};
use gtk::{gdk, gio, glib, graphene};

use super::data::DataPanel;
use super::export::{self, Format};
use super::search::Search;
use super::sidebar::Sidebar;
use super::start::{Pick, Start};
use super::{Ctx, config_dir, connect};
use crate::canvas::Canvas;
use crate::db::{self, Access, Target};
use crate::i18n::tr;
use crate::{APP_ID, motion, runtime, theme};

type WinFn = Box<dyn Fn(&Rc<Win>)>;
type WinStrFn = Box<dyn Fn(&Rc<Win>, String)>;

/// What is open, so it can be reloaded.
#[derive(Clone)]
enum Source {
    Server(Box<Connection>, String),
    File(PathBuf, Access),
}

pub struct Win {
    pub window: adw::ApplicationWindow,
    ctx: Rc<Ctx>,
    settings: gio::Settings,
    stack: gtk::Stack,
    title: adw::WindowTitle,
    start: Start,
    loading: gtk::Label,
    canvas: Canvas,
    sidebar: Rc<Sidebar>,
    split: adw::OverlaySplitView,
    search: Rc<Search>,
    panel: Rc<DataPanel>,
    stage: gtk::Overlay,
    ghost: gtk::Box,
    /// 0: panel closed, 1: open. Drives the ghost and the panel's opacity.
    panel_t: Cell<f64>,
    panel_open: Cell<bool>,
    ghost_from: Cell<Option<graphene::Rect>>,
    zoom_label: gtk::Button,
    diagram_only: Vec<gtk::Widget>,
    minimap: gtk::DrawingArea,
    editing: gtk::Label,
    source: RefCell<Option<Source>>,
    animation: RefCell<Option<adw::Animation>>,
}

pub fn build(app: &adw::Application) -> Rc<Win> {
    let settings = gio::Settings::new(APP_ID);
    let ctx = Rc::new(Ctx::default());

    // Header.
    let title = adw::WindowTitle::new("Atlas", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let sidebar_button = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(tr("Tables (F9)"))
        .build();
    let close_db = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text(tr("Close Database"))
        .action_name("win.close-db")
        .build();
    header.pack_start(&close_db);
    header.pack_start(&sidebar_button);
    // The session may write: said once, plainly, beside the title.
    let editing = gtk::Label::builder()
        .label(tr("Editing"))
        .tooltip_text(tr("This connection was opened with changes allowed"))
        .css_classes(["pill", "editing"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    header.pack_start(&editing);
    let theme_button = gtk::Button::builder()
        .icon_name(theme_icon())
        .tooltip_text(tr("Light or dark (Ctrl+Shift+L)"))
        .action_name("win.toggle-theme")
        .build();
    let search_button = gtk::Button::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text(tr("Go to (Ctrl+K)"))
        .action_name("win.search")
        .build();
    let sql_button = gtk::Button::builder()
        .icon_name("utilities-terminal-symbolic")
        .tooltip_text(tr("SQL (Ctrl+E)"))
        .action_name("win.console")
        .build();
    let export_menu = gio::Menu::new();
    export_menu.append(Some(&tr("PNG Image")), Some("win.export::png"));
    export_menu.append(Some(&tr("SVG Image")), Some("win.export::svg"));
    export_menu.append(Some(&tr("PDF Document")), Some("win.export::pdf"));
    export_menu.append(Some(&tr("Mermaid File")), Some("win.export::mmd"));
    export_menu.append(Some(&tr("Copy as Mermaid")), Some("win.copy-mermaid"));
    let export_button = gtk::MenuButton::builder()
        .icon_name("document-save-symbolic")
        .tooltip_text(tr("Export"))
        .menu_model(&export_menu)
        .build();
    header.pack_end(&main_menu());
    header.pack_end(&theme_button);
    header.pack_end(&export_button);
    header.pack_end(&sql_button);
    header.pack_end(&search_button);

    // Diagram page.
    let canvas = Canvas::default();
    let sidebar = Sidebar::new();
    let search = Search::new();
    let panel = DataPanel::new(ctx.clone());
    let ghost = gtk::Box::builder().css_classes(["ghost"]).can_target(false).visible(false).build();
    panel.widget.set_visible(false);
    let minimap = super::minimap::new(&canvas);
    let zoom_label = gtk::Button::builder().label("100 %").tooltip_text(tr("Actual size (Ctrl+1)")).action_name("win.zoom-reset").css_classes(["flat", "zoom-label"]).build();
    let pill = gtk::Box::builder()
        .css_classes(["pill"])
        .halign(gtk::Align::Start)
        .valign(gtk::Align::End)
        .margin_start(12)
        .margin_bottom(12)
        .build();
    for (icon, tip, action) in [
        ("list-remove-symbolic", tr("Zoom out (Ctrl+−)"), "win.zoom-out"),
        ("", String::new(), ""),
        ("list-add-symbolic", tr("Zoom in (Ctrl++)"), "win.zoom-in"),
        ("zoom-fit-best-symbolic", tr("Show all (Ctrl+0)"), "win.fit"),
    ] {
        if icon.is_empty() {
            pill.append(&zoom_label);
            continue;
        }
        pill.append(&gtk::Button::builder().icon_name(icon).tooltip_text(tip).action_name(action).css_classes(["flat", "circular"]).build());
    }

    let stage = gtk::Overlay::builder().child(&canvas).build();
    stage.add_overlay(&pill);
    stage.add_overlay(&minimap);
    stage.add_overlay(&ghost);
    stage.add_overlay(&panel.widget);
    stage.add_overlay(&search.widget);
    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar.widget)
        .content(&stage)
        .sidebar_width_fraction(0.2)
        .min_sidebar_width(220.0)
        .max_sidebar_width(300.0)
        .show_sidebar(settings.boolean("sidebar"))
        .build();
    split.bind_property("show-sidebar", &sidebar_button, "active").bidirectional().sync_create().build();

    // Loading page.
    let loading = gtk::Label::builder().css_classes(["statement"]).wrap(true).justify(gtk::Justification::Center).build();
    let loading_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).halign(gtk::Align::Center).build();
    loading_page.append(&adw::Spinner::builder().width_request(32).height_request(32).build());
    loading_page.append(&loading);

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).transition_duration(motion::LAYOUT_MS).build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));
    *ctx.toasts.borrow_mut() = Some(toasts.clone());
    // The cross-fade overlay for theme changes sits above everything.
    let fade = gtk::Overlay::builder().child(&toasts).build();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Atlas")
        .default_width(settings.int("window-width"))
        .default_height(settings.int("window-height"))
        .maximized(settings.boolean("window-maximized"))
        .content(&fade)
        .build();
    window.add_css_class("atlas");
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(adw::BreakpointConditionLengthType::MaxWidth, 720.0, adw::LengthUnit::Sp));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    let weak_start: Rc<RefCell<Option<std::rc::Weak<Win>>>> = Rc::default();
    let ws = weak_start.clone();
    let start = Start::new(move |pick| {
        if let Some(win) = ws.borrow().as_ref().and_then(|w| w.upgrade()) {
            win.on_pick(pick);
        }
    });
    stack.add_named(&start.widget, Some("start"));
    stack.add_named(&loading_page, Some("loading"));
    stack.add_named(&split, Some("diagram"));

    let win = Rc::new(Win {
        window,
        ctx,
        settings,
        stack,
        title,
        start,
        loading,
        canvas,
        sidebar,
        split,
        search,
        panel,
        stage,
        ghost,
        panel_t: Cell::new(0.0),
        panel_open: Cell::new(false),
        ghost_from: Cell::new(None),
        zoom_label,
        diagram_only: vec![
            close_db.upcast(),
            sidebar_button.upcast(),
            search_button.upcast(),
            sql_button.upcast(),
            export_button.upcast(),
        ],
        minimap,
        editing,
        source: RefCell::default(),
        animation: RefCell::default(),
    });
    *weak_start.borrow_mut() = Some(Rc::downgrade(&win));
    // Everything else holds the window weakly; this keeps it alive until the
    // window is destroyed, when GTK drops its signal handlers.
    let keep = win.clone();
    win.window.connect_destroy(move |_| {
        let _ = &keep;
    });
    win.wire(&fade, &theme_button);
    win.show_start();
    win
}

fn theme_icon() -> &'static str {
    if adw::StyleManager::default().is_dark() { "weather-clear-symbolic" } else { "weather-clear-night-symbolic" }
}

fn main_menu() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    let open = gio::Menu::new();
    open.append(Some(&tr("New Connection…")), Some("win.new-connection"));
    open.append(Some(&tr("Open File…")), Some("win.open-file"));
    open.append(Some(&tr("New Window")), Some("app.new-window"));
    menu.append_section(None, &open);
    let database = gio::Menu::new();
    database.append(Some(&tr("Allow Changes to This File")), Some("win.allow-changes"));
    database.append(Some(&tr("New Table…")), Some("win.new-table"));
    database.append(Some(&tr("Design Mode")), Some("win.design"));
    database.append(Some(&tr("Apply Design…")), Some("win.apply-design"));
    database.append(Some(&tr("Save Design…")), Some("win.save-design"));
    database.append(Some(&tr("Export as SQL…")), Some("win.export-ddl"));
    database.append(Some(&tr("Import CSV…")), Some("win.import-csv"));
    database.append(Some(&tr("Run SQL File…")), Some("win.import-sql"));
    database.append(Some(&tr("Back Up…")), Some("win.backup"));
    database.append(Some(&tr("Compare With…")), Some("win.compare"));
    database.append(Some(&tr("Server…")), Some("win.server"));
    database.append(Some(&tr("Users…")), Some("win.users"));
    menu.append_section(Some(&tr("Database")), &database);
    let view = gio::Menu::new();
    view.append(Some(&tr("Inferred Relations")), Some("win.show-inferred"));
    view.append(Some(&tr("Minimap")), Some("win.minimap"));
    view.append(Some(&tr("Unfold All Tables")), Some("win.unfold-all"));
    view.append(Some(&tr("Fold All Tables")), Some("win.fold-all"));
    view.append(Some(&tr("Tidy Up Layout")), Some("win.tidy"));
    view.append(Some(&tr("Reload Structure")), Some("win.refresh"));
    menu.append_section(None, &view);
    let app = gio::Menu::new();
    app.append(Some(&tr("Follow System Style")), Some("win.follow-system"));
    app.append(Some(&tr("Keyboard Shortcuts")), Some("win.shortcuts"));
    app.append(Some(&tr("About Atlas")), Some("win.about"));
    app.append(Some(&tr("Quit")), Some("app.quit"));
    menu.append_section(None, &app);
    gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).tooltip_text(tr("Main Menu")).build()
}

impl Win {
    fn wire(self: &Rc<Self>, fade: &gtk::Overlay, theme_button: &gtk::Button) {
        let w = Rc::downgrade(self);
        let action = |name: &str, f: WinFn| {
            let a = gio::SimpleAction::new(name, None);
            let w = w.clone();
            a.connect_activate(move |_, _| {
                if let Some(win) = w.upgrade() {
                    f(&win);
                }
            });
            self.window.add_action(&a);
        };
        let fade_c = fade.clone();
        action(
            "toggle-theme",
            Box::new(move |win| {
                let s = win.settings.clone();
                cross_fade(&fade_c, move || {
                    let _ = s.set_string(theme::KEY, theme::flipped());
                    theme::apply(&s);
                });
            }),
        );
        let fade_c = fade.clone();
        action(
            "follow-system",
            Box::new(move |win| {
                let s = win.settings.clone();
                cross_fade(&fade_c, move || {
                    s.reset(theme::KEY);
                    theme::apply(&s);
                });
            }),
        );
        action("new-connection", Box::new(|win| win.on_pick(Pick::New)));
        action("open-file", Box::new(|win| win.on_pick(Pick::File)));
        action("close-db", Box::new(|win| win.close_db()));
        action("refresh", Box::new(|win| win.reload()));
        action("search", Box::new(|win| win.toggle_search()));
        action("console", Box::new(|win| win.open_console()));
        action("toggle-sidebar", Box::new(|win| win.split.set_show_sidebar(!win.split.shows_sidebar())));
        action("zoom-in", Box::new(|win| win.canvas.zoom_by(1.25)));
        action("zoom-out", Box::new(|win| win.canvas.zoom_by(0.8)));
        action("zoom-reset", Box::new(|win| win.canvas.zoom_reset()));
        action("fit", Box::new(|win| win.canvas.fit()));
        action("tidy", Box::new(|win| win.canvas.tidy()));
        action("unfold-all", Box::new(|win| win.canvas.fold_all(true)));
        action("fold-all", Box::new(|win| win.canvas.fold_all(false)));
        action("copy-mermaid", Box::new(|win| {
            if let Some(text) = export::mermaid(&win.canvas) {
                win.window.clipboard().set_text(&text);
                win.ctx.toast(&tr("Mermaid diagram copied"));
            }
        }));
        action("new-table", Box::new(|win| {
            if win.ctx.writable() {
                super::editor::open(&win.window, win.ctx.clone(), None);
            } else if win.canvas.design() {
                win.canvas.add_table_at_center();
            } else {
                win.ctx.toast(&tr("Open the connection with changes allowed, or use Design Mode."));
            }
        }));
        action("import-csv", Box::new(|win| {
            if win.require_writable() {
                let table = (win.panel.mode().as_deref() == Some("data")).then(|| win.panel.current_table());
                super::import::csv_import(&win.window, win.ctx.clone(), table);
            }
        }));
        action("import-sql", Box::new(|win| {
            if win.require_writable() {
                super::import::sql_import(&win.window, win.ctx.clone());
            }
        }));
        action("backup", Box::new(|win| {
            if win.ctx.session.borrow().is_some() {
                super::import::backup(&win.window, win.ctx.clone());
            } else {
                win.ctx.toast(&tr("A dump has no rows to back up."));
            }
        }));
        action("compare", Box::new(|win| super::compare::open(&win.window, win.ctx.clone())));
        action("server", Box::new(|win| {
            if win.ctx.session.borrow().is_some() {
                super::admin::server_info(&win.window, win.ctx.clone());
            }
        }));
        action("users", Box::new(|win| {
            let sqlite = win.ctx.session.borrow().as_ref().is_none_or(|s| s.dialect == atlas_core::sql::Dialect::Sqlite);
            if sqlite {
                win.ctx.toast(&tr("SQLite files have no users."));
            } else if win.require_writable() {
                super::admin::users(&win.window, win.ctx.clone());
            }
        }));
        action("allow-changes", Box::new(|win| {
            let source = win.source.borrow().clone();
            match source {
                Some(Source::File(path, Access::ReadOnly)) if win.ctx.session.borrow().is_some() => {
                    let p = path.clone();
                    super::review::ask(
                        &win.window,
                        &tr("Allow Changes?"),
                        &tr("The file is opened again with writing allowed. Every change is shown as SQL before it runs."),
                        &tr("Allow Changes"),
                        glib::clone!(
                            #[weak(rename_to = w)]
                            win.window,
                            move || {
                                let _ = gtk::prelude::WidgetExt::activate_action(&w, "win.open-writable", Some(&p.to_string_lossy().to_variant()));
                            }
                        ),
                    );
                }
                Some(Source::Server(..)) => win.ctx.toast(&tr("For a server, turn on “Allow changes” in the connection.")),
                _ => {}
            }
        }));
        let design = gio::SimpleAction::new_stateful("design", None, &false.to_variant());
        let ww = w.clone();
        design.connect_activate(move |a, _| {
            let Some(win) = ww.upgrade() else { return };
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            win.canvas.set_design(on);
            win.ctx.toast(&if on {
                tr("Design Mode: double-click empty space for a table, drag from a column to another table for a key.")
            } else {
                tr("Design Mode off")
            });
        });
        self.window.add_action(&design);
        // Sets the toggle without the toast, for design files.
        let d = design.clone();
        let set_state = gio::SimpleAction::new("design-state", Some(glib::VariantTy::BOOLEAN));
        set_state.connect_activate(move |_, v| {
            if let Some(on) = v.and_then(|v| v.get::<bool>()) {
                d.set_state(&on.to_variant());
            }
        });
        self.window.add_action(&set_state);
        action("apply-design", Box::new(|win| win.apply_design()));
        action("save-design", Box::new(|win| win.save_text_file(&tr("Save Design"), "atlas.json", |win| {
            let schema = win.canvas.schema()?;
            Some(atlas_core::design::save(&schema, &win.canvas.layout()))
        })));
        action("export-ddl", Box::new(|win| win.save_text_file(&tr("Export as SQL"), "sql", |win| {
            let schema = win.canvas.schema()?;
            let dialect = win.ctx.session.borrow().as_ref().map_or(atlas_core::sql::Dialect::MySql, |s| s.dialect);
            Some(atlas_core::ddl::schema_script(dialect, &schema).joined())
        })));
        action("about", Box::new(|win| win.about()));
        action("shortcuts", Box::new(|win| win.shortcuts()));

        let export = gio::SimpleAction::new("export", Some(glib::VariantTy::STRING));
        let ww = w.clone();
        export.connect_activate(move |_, param| {
            let Some(win) = ww.upgrade() else { return };
            let format = match param.and_then(|p| p.str()) {
                Some("svg") => Format::Svg,
                Some("pdf") => Format::Pdf,
                Some("mmd") => Format::Mermaid,
                _ => Format::Png,
            };
            let name = win.ctx.schema.borrow().as_ref().map(|s| s.name.clone()).unwrap_or_else(|| "diagram".into());
            let ctx = win.ctx.clone();
            export::save(&win.window, &win.canvas, format, &name, move |r| match r {
                Ok(path) => ctx.toast(&tr("Saved to {path}").replace("{path}", &path)),
                Err(e) => ctx.toast(&e),
            });
        });
        self.window.add_action(&export);

        // Stateful toggles kept in GSettings.
        for key in ["show-inferred", "minimap"] {
            self.window.add_action(&self.settings.create_action(key));
        }
        let ww = w.clone();
        self.settings.connect_changed(Some("show-inferred"), move |s, k| {
            if let Some(win) = ww.upgrade() {
                win.canvas.set_show_inferred(s.boolean(k));
            }
        });
        self.canvas.set_show_inferred(self.settings.boolean("show-inferred"));
        self.settings.bind("minimap", &self.minimap, "visible").get_only().build();
        self.settings.bind("sidebar", &self.split, "show-sidebar").build();

        // Hidden actions for scripted screenshots and tests.
        let string_action = |name: &str, f: WinStrFn| {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let w = w.clone();
            a.connect_activate(move |_, p| {
                if let (Some(win), Some(s)) = (w.upgrade(), p.and_then(|p| p.str()).map(str::to_owned)) {
                    f(&win, s);
                }
            });
            self.window.add_action(&a);
        };
        string_action("open-path", Box::new(|win, p| win.open_file(PathBuf::from(p))));
        string_action("open-writable", Box::new(|win, p| win.open_file_with(PathBuf::from(p), Access::ReadWrite)));
        string_action("structure", Box::new(|win, t| {
            win.open_data(&t);
            win.panel.show_structure_now();
        }));
        string_action("focus", Box::new(|win, t| win.canvas.focus_table(Some(&t), true)));
        string_action("data", Box::new(|win, t| win.open_data(&t)));
        string_action("console-sql", Box::new(|win, sql| {
            win.open_console();
            win.panel.set_console_text(&sql);
            win.panel.run_console();
        }));
        action("close-panel", Box::new(|win| win.close_panel()));
        // "host:port:user:database", MariaDB, empty password: for scripted
        // runs against a test server only; it never takes a password.
        string_action("connect-spec", Box::new(|win, spec| {
            let p: Vec<&str> = spec.split(':').collect();
            if let [host, port, user, database] = p[..] {
                let conn = Connection {
                    id: format!("spec-{spec}"),
                    name: format!("{database} @ {host}"),
                    dialect: atlas_core::sql::Dialect::MySql,
                    host: host.into(),
                    port: port.parse().unwrap_or(3306),
                    user: user.into(),
                    database: database.into(),
                    socket: None,
                    ssh: None,
                    writable: false,
                    keyring: false,
                };
                win.open_server(conn, String::new());
            }
        }));
        // "table|column|value": the rows of a table filtered to one value,
        // as a click on a foreign key does.
        string_action("data-filter", Box::new(|win, spec| {
            let parts: Vec<&str> = spec.splitn(3, '|').collect();
            if let [table, column, value] = parts[..] {
                let from = win.canvas.card_rect(table);
                let filter = atlas_core::sql::Filter { column: column.into(), op: atlas_core::sql::Op::Equals, value: value.into() };
                win.panel.open_table(table, vec![filter]);
                win.show_panel(from);
            }
        }));
        string_action("export-to", Box::new(|win, path| {
            let path = PathBuf::from(path);
            let format = match path.extension().and_then(|e| e.to_str()) {
                Some("svg") => Format::Svg,
                Some("pdf") => Format::Pdf,
                Some("mmd") => Format::Mermaid,
                _ => Format::Png,
            };
            match export::write(win.window.upcast_ref(), &win.canvas, format, &path) {
                Ok(()) => println!("atlas: exported {}", path.display()),
                Err(e) => eprintln!("atlas: export {}: {e}", path.display()),
            }
        }));
        string_action("unfold", Box::new(|win, t| win.canvas.set_unfolded(&t, true)));
        string_action("search-text", Box::new(|win, t| {
            win.search.show();
            win.search_set(&t);
        }));

        // Canvas.
        let ww = w.clone();
        self.canvas.connect_open(move |name| {
            if let Some(win) = ww.upgrade() {
                win.open_data(name);
            }
        });
        let ww = w.clone();
        self.canvas.connect_focus(move |name| {
            if let Some(win) = ww.upgrade() {
                win.sidebar.select(name);
            }
        });
        let ww = w.clone();
        self.canvas.connect_view(move || {
            if let Some(win) = ww.upgrade() {
                win.zoom_label.set_label(&format!("{} %", (win.canvas.zoom() * 100.0).round()));
                if win.panel_t.get() > 0.0 {
                    win.stage.queue_allocate();
                }
            }
        });
        let ww = w.clone();
        self.canvas.connect_layout(move || {
            if let Some(win) = ww.upgrade() {
                win.save_layout();
            }
        });
        let ww = w.clone();
        self.sidebar.on_activate(move |name| {
            if let Some(win) = ww.upgrade() {
                win.canvas.focus_table(Some(name), true);
                if win.panel_open.get() && win.panel.mode().as_deref() == Some("data") {
                    win.panel.open_table(name, Vec::new());
                }
            }
        });
        let ww = w.clone();
        self.search.on_chosen(move |name| {
            if let Some(win) = ww.upgrade() {
                win.canvas.focus_table(Some(name), true);
                win.canvas.grab_focus();
            }
        });
        let ww = w.clone();
        self.panel.on_navigate(move |name| {
            if let Some(win) = ww.upgrade() {
                win.canvas.focus_table(Some(name), true);
            }
        });
        let ww = w.clone();
        self.panel.on_close(move || {
            if let Some(win) = ww.upgrade() {
                win.close_panel();
            }
        });
        let ww = w.clone();
        self.panel.on_edit_table(move |t| {
            if let Some(win) = ww.upgrade() {
                super::editor::open(&win.window, win.ctx.clone(), Some(t));
            }
        });
        let ww = w.clone();
        self.panel.on_drop_table(move |t| {
            if let Some(win) = ww.upgrade() {
                super::editor::drop_table(&win.window, win.ctx.clone(), t);
            }
        });
        let ww = w.clone();
        *self.ctx.on_schema_changed.borrow_mut() = Some(Rc::new(move || {
            if let Some(win) = ww.upgrade() {
                win.reload_schema();
            }
        }));
        let ww = w.clone();
        *self.ctx.on_rows_changed.borrow_mut() = Some(Rc::new(move || {
            if let Some(win) = ww.upgrade() {
                win.panel.reload_rows();
            }
        }));
        let ww = w.clone();
        self.canvas.connect_schema_changed(move |schema| {
            if let Some(win) = ww.upgrade() {
                win.design_changed(schema);
            }
        });

        // Panel and ghost geometry.
        let ww = w.clone();
        self.stage.connect_get_child_position(move |stage, child| {
            let win = ww.upgrade()?;
            let (sw, sh) = (stage.width() as f32, stage.height() as f32);
            let target = panel_rect(sw, sh);
            if child == win.panel.widget.upcast_ref::<gtk::Widget>() {
                return Some(to_gdk(&target));
            }
            if child == win.ghost.upcast_ref::<gtk::Widget>() {
                let from = win.ghost_from.get().unwrap_or_else(|| graphene::Rect::new(target.x() + 48.0, target.y(), target.width(), target.height()));
                return Some(to_gdk(&lerp(&from, &target, win.panel_t.get() as f32)));
            }
            None
        });

        // Escape: search, then panel.
        let keys = gtk::EventControllerKey::new();
        let ww = w.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(win) = ww.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Escape {
                if win.search.is_shown() {
                    win.search.hide();
                    return glib::Propagation::Stop;
                }
                if win.panel_open.get() {
                    win.close_panel();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);

        adw::StyleManager::default().connect_dark_notify(glib::clone!(
            #[weak]
            theme_button,
            move |_| theme_button.set_icon_name(theme_icon())
        ));

        let ww = w.clone();
        self.window.connect_close_request(move |window| {
            if let Some(win) = ww.upgrade() {
                win.save_layout();
                let (width, height) = window.default_size();
                let _ = win.settings.set_int("window-width", width);
                let _ = win.settings.set_int("window-height", height);
                let _ = win.settings.set_boolean("window-maximized", window.is_maximized());
            }
            glib::Propagation::Proceed
        });
    }

    fn search_set(&self, text: &str) {
        // The entry is the revealer's first descendant with text.
        if let Some(entry) = self
            .search
            .widget
            .child()
            .and_then(|b| b.first_child())
            .and_then(|e| e.downcast::<gtk::SearchEntry>().ok())
        {
            entry.set_text(text);
        }
    }

    // ---- Opening and closing ----------------------------------------------

    fn show_start(&self) {
        let connections: Vec<Connection> = store::load(&config_dir().join("connections.json"));
        let recent: Recent = store::load(&config_dir().join("recent.json"));
        self.start.set_recent(&connections, &recent.files);
        self.stack.set_visible_child_name("start");
        self.title.set_title("Atlas");
        self.title.set_subtitle("");
        for w in &self.diagram_only {
            w.set_visible(false);
        }
    }

    fn on_pick(self: &Rc<Self>, pick: Pick) {
        match pick {
            Pick::New => {
                let w = Rc::downgrade(self);
                connect::edit(&self.window, None, move |conn, password| {
                    if let Some(win) = w.upgrade() {
                        win.remember(&conn);
                        win.open_server(conn, password);
                    }
                });
            }
            Pick::Edit(c) => {
                let w = Rc::downgrade(self);
                connect::edit(&self.window, Some(&c), move |conn, password| {
                    if let Some(win) = w.upgrade() {
                        win.remember(&conn);
                        win.open_server(conn, password);
                    }
                });
            }
            Pick::Server(c) => {
                let w = Rc::downgrade(self);
                let conn = c.clone();
                connect::password(&self.window, &c, move |password| {
                    if let Some(win) = w.upgrade() {
                        win.open_server(conn.clone(), password);
                    }
                });
            }
            Pick::Forget(c) => {
                let path = config_dir().join("connections.json");
                let mut all: Vec<Connection> = store::load(&path);
                all.retain(|x| x.id != c.id);
                let _ = store::save(&path, &all);
                self.show_start();
            }
            Pick::File => {
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&tr("Databases and SQL dumps")));
                for p in ["*.sql", "*.db", "*.sqlite", "*.sqlite3", "*.db3", "*.dump", "*.txt"] {
                    filter.add_pattern(p);
                }
                let all = gtk::FileFilter::new();
                all.set_name(Some(&tr("All Files")));
                all.add_pattern("*");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                filters.append(&all);
                let dialog = gtk::FileDialog::builder().title(tr("Open Database or Dump")).filters(&filters).modal(true).build();
                let w = Rc::downgrade(self);
                dialog.open(Some(&self.window), gio::Cancellable::NONE, move |r| {
                    if let (Ok(file), Some(win)) = (r, w.upgrade())
                        && let Some(path) = file.path()
                    {
                        win.open_file(path);
                    }
                });
            }
            Pick::Recent(path) => self.open_file(path),
            Pick::ForgetFile(path) => {
                let file = config_dir().join("recent.json");
                let mut recent: Recent = store::load(&file);
                recent.files.retain(|p| *p != path);
                let _ = store::save(&file, &recent);
                self.show_start();
            }
        }
    }

    /// Saves a connection (never its password), newest first.
    fn remember(&self, conn: &Connection) {
        let path = config_dir().join("connections.json");
        let mut all: Vec<Connection> = store::load(&path);
        all.retain(|c| c.id != conn.id);
        all.insert(0, conn.clone());
        let _ = store::save(&path, &all);
    }

    fn remember_file(&self, path: &Path) {
        let file = config_dir().join("recent.json");
        let mut recent: Recent = store::load(&file);
        recent.files.retain(|p| p != path);
        recent.files.insert(0, path.to_owned());
        recent.files.truncate(8);
        let _ = store::save(&file, &recent);
    }

    fn loading(&self, text: &str) {
        self.close_session();
        self.loading.set_text(text);
        self.stack.set_visible_child_name("loading");
        for w in &self.diagram_only {
            w.set_visible(false);
        }
    }

    fn open_server(self: &Rc<Self>, conn: Connection, password: String) {
        self.loading(&tr("Connecting to {name}").replace("{name}", &conn.name));
        *self.source.borrow_mut() = Some(Source::Server(Box::new(conn.clone()), password.clone()));
        let w = Rc::downgrade(self);
        let c = conn.clone();
        let typed = password.clone();
        glib::spawn_future_local(async move {
            let access = if c.writable { Access::ReadWrite } else { Access::ReadOnly };
            let result = runtime::run(async move {
                let session = db::connect_with(Target::Server(c), password, access).await?;
                let schema = session.schema().await?;
                Ok::<_, String>((session, schema))
            })
            .await;
            let Some(win) = w.upgrade() else { return };
            match result {
                Ok((session, schema)) => {
                    let subtitle = conn.subtitle();
                    *win.ctx.session.borrow_mut() = Some(session);
                    win.show_schema(schema, &conn.id, &subtitle);
                }
                Err(e) => {
                    win.show_start();
                    win.connect_failed(conn, typed, &e);
                }
            }
        });
    }

    /// Says why, offers the databases this user may open when the name was
    /// the problem, or the form again.
    fn connect_failed(self: &Rc<Self>, conn: Connection, password: String, error: &str) {
        let (w1, w2) = (Rc::downgrade(self), Rc::downgrade(self));
        let again = conn.clone();
        connect::failed(
            &self.window,
            &conn,
            &password,
            error,
            move |changed, password| {
                if let Some(win) = w1.upgrade() {
                    win.remember(&changed);
                    win.open_server(changed, password);
                }
            },
            move || {
                if let Some(win) = w2.upgrade() {
                    win.on_pick(Pick::Edit(again.clone()));
                }
            },
        );
    }

    fn open_file(self: &Rc<Self>, path: PathBuf) {
        self.open_file_with(path, Access::ReadOnly);
    }

    fn open_file_with(self: &Rc<Self>, path: PathBuf, access: Access) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.loading(&tr("Reading {name}").replace("{name}", &name));
        *self.source.borrow_mut() = Some(Source::File(path.clone(), access));
        let w = Rc::downgrade(self);
        let p = path.clone();
        glib::spawn_future_local(async move {
            let result = runtime::run(async move {
                let mut head = [0u8; 16];
                let is_sqlite = std::fs::File::open(&p)
                    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
                    .is_ok()
                    && &head == b"SQLite format 3\0";
                if is_sqlite {
                    let session = db::connect_with(Target::File(p), String::new(), access).await?;
                    let schema = session.schema().await?;
                    Ok((Some(session), schema, 0))
                } else {
                    let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(d) = atlas_core::design::load(&text) {
                        return Ok((None, d.schema, usize::MAX));
                    }
                    let stem = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let (mut schema, report) = atlas_core::dump::parse(&stem, &text);
                    if schema.tables.is_empty() {
                        return Err(tr("No CREATE TABLE statements found in this file."));
                    }
                    atlas_core::infer::infer(&mut schema);
                    Ok((None, schema, report.skipped.len()))
                }
            })
            .await;
            let Some(win) = w.upgrade() else { return };
            match result {
                Ok((None, schema, usize::MAX)) => {
                    // A design file: structure only, edited on the canvas.
                    win.remember_file(&path);
                    win.show_schema(schema, &path.to_string_lossy(), &tr("Design"));
                    win.canvas.set_design(true);
                    let _ = gtk::prelude::WidgetExt::activate_action(&win.window, "win.design-state", Some(&true.to_variant()));
                }
                Ok((session, schema, skipped)) => {
                    win.remember_file(&path);
                    let is_dump = session.is_none();
                    *win.ctx.session.borrow_mut() = session;
                    let kind = match (is_dump, access) {
                        (true, _) => tr("SQL dump · structure only"),
                        (false, Access::ReadOnly) => tr("SQLite · read-only"),
                        (false, Access::ReadWrite) => tr("SQLite · changes allowed"),
                    };
                    win.show_schema(schema, &path.to_string_lossy(), &kind);
                    if skipped > 0 {
                        win.ctx.toast(&crate::i18n::ntr("{} statement could not be read", "{} statements could not be read", skipped as u32).replace("{}", &skipped.to_string()));
                    }
                }
                Err(e) => {
                    win.show_start();
                    win.ctx.toast(&e);
                }
            }
        });
    }

    fn show_schema(&self, schema: Schema, key: &str, subtitle: &str) {
        let layout: Layout = store::load(&config_dir().join("layouts").join(store::key_file(key)));
        *self.ctx.key.borrow_mut() = Some(key.to_owned());
        *self.ctx.schema.borrow_mut() = Some(schema.clone());
        self.title.set_title(&schema.name);
        self.title.set_subtitle(subtitle);
        self.sidebar.set_schema(Some(&schema));
        self.search.set_schema(Some(schema.clone()));
        self.canvas.set_schema(Some(schema), &layout);
        self.stack.set_visible_child_name("diagram");
        for w in &self.diagram_only {
            w.set_visible(true);
        }
        self.editing.set_visible(self.ctx.writable());
        self.canvas.grab_focus();
    }

    /// Asks for a file and writes what `text` gives.
    fn save_text_file(self: &Rc<Self>, title: &str, extension: &str, text: impl Fn(&Win) -> Option<String> + 'static) {
        let Some(name) = self.canvas.schema().map(|s| s.name) else { return };
        let dialog = gtk::FileDialog::builder().title(title).initial_name(format!("{name}.{extension}")).modal(true).build();
        let w = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |r| {
            let (Ok(file), Some(win)) = (r, w.upgrade()) else { return };
            let (Some(path), Some(content)) = (file.path(), text(&win)) else { return };
            match std::fs::write(&path, content) {
                Ok(()) => win.ctx.toast(&tr("Saved to {path}").replace("{path}", &path.display().to_string())),
                Err(e) => win.ctx.toast(&e.to_string()),
            }
        });
    }

    /// The canvas design against the live structure: the SQL, reviewed,
    /// then run.
    fn apply_design(self: &Rc<Self>) {
        if !self.require_writable() {
            return;
        }
        let (Some(session), Some(live), Some(design)) = (self.ctx.session.borrow().clone(), self.ctx.schema.borrow().clone(), self.canvas.schema()) else { return };
        let m = atlas_core::diff::migration(session.dialect, &live, &design);
        if m.is_empty() {
            self.ctx.toast(&tr("The design matches the database"));
            return;
        }
        let ctx = self.ctx.clone();
        let (statements, fk_off) = (m.statements.clone(), m.foreign_keys_off);
        super::review::review(
            &self.window,
            &tr("Apply the Design?"),
            &tr("Tables removed in the design are dropped with their rows."),
            &m.text(),
            &tr("Apply"),
            true,
            move || {
                super::review::run_batch(session, statements, fk_off, move |r| match r {
                    Ok(_) => {
                        ctx.toast(&tr("Design applied"));
                        ctx.schema_changed();
                    }
                    Err(e) => ctx.toast(&e),
                });
            },
        );
    }

    fn require_writable(&self) -> bool {
        if self.ctx.writable() {
            return true;
        }
        self.ctx.toast(&tr("This connection is read-only. Turn on “Allow changes” for it first."));
        false
    }

    /// The structure again from the open session, keeping the session, the
    /// layout and what the panel shows.
    fn reload_schema(self: &Rc<Self>) {
        let Some(session) = self.ctx.session.borrow().clone() else { return };
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = runtime::run(async move { session.schema().await }).await;
            let Some(win) = w.upgrade() else { return };
            match result {
                Ok(schema) => {
                    win.save_layout();
                    let key = win.ctx.key.borrow().clone().unwrap_or_default();
                    let layout: Layout = store::load(&config_dir().join("layouts").join(store::key_file(&key)));
                    *win.ctx.schema.borrow_mut() = Some(schema.clone());
                    win.sidebar.set_schema(Some(&schema));
                    win.search.set_schema(Some(schema.clone()));
                    win.canvas.set_schema(Some(schema), &layout);
                    win.panel.refresh_structure();
                    win.panel.reload_rows();
                }
                Err(e) => win.ctx.toast(&e),
            }
        });
    }

    /// A change made in Design Mode. Without a writable session the design
    /// is the schema; with one, the user applies it with "Apply Design".
    fn design_changed(self: &Rc<Self>, schema: Schema) {
        self.sidebar.set_schema(Some(&schema));
        self.search.set_schema(Some(schema.clone()));
        if self.ctx.session.borrow().is_none() || !self.ctx.writable() {
            *self.ctx.schema.borrow_mut() = Some(schema);
        }
        self.save_layout();
    }

    fn save_layout(&self) {
        let Some(key) = self.ctx.key.borrow().clone() else { return };
        if self.canvas.schema().is_none() {
            return;
        }
        let _ = store::save(&config_dir().join("layouts").join(store::key_file(&key)), &self.canvas.layout());
    }

    fn close_session(&self) {
        if let Some(session) = self.ctx.session.borrow_mut().take() {
            glib::spawn_future_local(async move {
                runtime::run(async move { session.close().await }).await;
            });
        }
    }

    fn close_db(self: &Rc<Self>) {
        self.save_layout();
        self.hide_panel_now();
        self.search.hide();
        self.close_session();
        *self.ctx.schema.borrow_mut() = None;
        *self.ctx.key.borrow_mut() = None;
        *self.source.borrow_mut() = None;
        self.canvas.set_schema(None, &Layout::default());
        self.sidebar.set_schema(None);
        self.search.set_schema(None);
        self.show_start();
    }

    fn reload(self: &Rc<Self>) {
        let source = self.source.borrow().clone();
        self.save_layout();
        self.hide_panel_now();
        match source {
            Some(Source::Server(c, p)) => self.open_server(*c, p),
            Some(Source::File(path, access)) => self.open_file_with(path, access),
            None => {}
        }
    }

    // ---- Search, panel ------------------------------------------------------

    fn toggle_search(&self) {
        if self.stack.visible_child_name().as_deref() != Some("diagram") {
            return;
        }
        if self.search.is_shown() {
            self.search.hide();
        } else {
            self.search.show();
        }
    }

    fn open_data(self: &Rc<Self>, table: &str) {
        let from = self.canvas.card_rect(table);
        self.panel.open_table(table, Vec::new());
        self.show_panel(from);
    }

    fn open_console(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() != Some("diagram") {
            return;
        }
        self.panel.open_console();
        self.show_panel(None);
        self.panel.focus_editor();
    }

    /// Grows the panel out of `from` (a card) on a spring.
    fn show_panel(self: &Rc<Self>, from: Option<graphene::Rect>) {
        if self.panel_open.get() {
            return;
        }
        self.panel_open.set(true);
        self.ghost_from.set(from);
        self.ghost.set_visible(true);
        self.ghost.set_opacity(1.0);
        self.panel.widget.set_visible(true);
        self.panel.widget.set_opacity(0.0);
        let w = Rc::downgrade(self);
        let target = adw::CallbackAnimationTarget::new(move |t| {
            if let Some(win) = w.upgrade() {
                win.panel_t.set(t);
                // The content arrives once the shape has nearly arrived.
                win.panel.widget.set_opacity(((t - 0.82) / 0.18).clamp(0.0, 1.0));
                win.stage.queue_allocate();
            }
        });
        let start = self.panel_t.get();
        let spring = motion::PANEL;
        let animation = adw::SpringAnimation::builder()
            .widget(&self.stage)
            .value_from(start)
            .value_to(1.0)
            .spring_params(&spring.params())
            .epsilon(0.002)
            .clamp(true)
            .target(&target)
            .build();
        let w = Rc::downgrade(self);
        animation.connect_done(move |_| {
            if let Some(win) = w.upgrade() {
                win.ghost.set_visible(false);
                win.panel.widget.set_opacity(1.0);
            }
        });
        animation.play();
        *self.animation.borrow_mut() = Some(animation.upcast());
    }

    /// Shrinks the panel back into the card it came from, or fades it out.
    fn close_panel(self: &Rc<Self>) {
        if !self.panel_open.get() {
            return;
        }
        self.panel_open.set(false);
        let table = self.panel.current_table();
        let back = if self.panel.mode().as_deref() == Some("data") { self.canvas.card_rect(&table) } else { None };
        self.ghost_from.set(back);
        self.ghost.set_visible(true);
        let w = Rc::downgrade(self);
        let target = adw::CallbackAnimationTarget::new(move |t| {
            if let Some(win) = w.upgrade() {
                win.panel_t.set(t);
                win.panel.widget.set_opacity(((t - 0.7) / 0.3).clamp(0.0, 1.0));
                win.ghost.set_opacity((t * 2.5).clamp(0.0, 1.0));
                win.stage.queue_allocate();
            }
        });
        let animation = adw::TimedAnimation::builder()
            .widget(&self.stage)
            .value_from(self.panel_t.get())
            .value_to(0.0)
            .duration(motion::LAYOUT_MS)
            .easing(motion::EASE)
            .target(&target)
            .build();
        let w = Rc::downgrade(self);
        animation.connect_done(move |_| {
            if let Some(win) = w.upgrade() {
                win.ghost.set_visible(false);
                win.panel.widget.set_visible(false);
                win.canvas.grab_focus();
            }
        });
        animation.play();
        *self.animation.borrow_mut() = Some(animation.upcast());
    }

    fn hide_panel_now(&self) {
        if let Some(a) = self.animation.borrow_mut().take() {
            a.skip();
        }
        self.panel_open.set(false);
        self.panel_t.set(0.0);
        self.ghost.set_visible(false);
        self.panel.widget.set_visible(false);
    }

    fn about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Atlas")
            .application_icon(APP_ID)
            .developer_name("Thomas Haiden")
            .version(env!("CARGO_PKG_VERSION"))
            .comments(tr("A light ER diagram and database viewer. It only reads."))
            .license_type(gtk::License::MitX11)
            .build();
        about.add_legal_section("Inter, Geist Serif, Geist Mono", None, gtk::License::Custom, Some("SIL Open Font License 1.1"));
        about.present(Some(&self.window));
    }

    fn shortcuts(&self) {
        let rows: Vec<(String, String)> = vec![
            ("Ctrl+K".into(), tr("Go to a table or column")),
            ("Ctrl+E".into(), tr("SQL console")),
            ("Ctrl+Enter".into(), tr("Run the SQL")),
            ("F9".into(), tr("Show or hide the table list")),
            (tr("Two fingers, wheel"), tr("Move the view")),
            (tr("Pinch, Ctrl+wheel"), tr("Zoom")),
            ("Ctrl+0  /  Ctrl+1".into(), tr("Show all  /  actual size")),
            (tr("Arrow keys"), tr("Next connected table")),
            (tr("Space"), tr("Unfold or fold the table")),
            (tr("Enter, double click"), tr("Show its rows")),
            ("Esc".into(), tr("Close search, panel, focus")),
            ("F5".into(), tr("Reload structure")),
            ("Ctrl+Shift+L".into(), tr("Light or dark")),
        ];
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
        for (keys, what) in rows {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(&what)).build();
            row.add_suffix(&gtk::Label::builder().label(&keys).css_classes(["mono", "dim-label"]).build());
            list.append(&row);
        }
        let page = gtk::ScrolledWindow::builder()
            .child(&adw::Clamp::builder().child(&list).margin_top(12).margin_bottom(18).margin_start(18).margin_end(18).build())
            .propagate_natural_height(true)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder().title(tr("Keyboard Shortcuts")).content_width(480).child(&toolbar).build();
        dialog.present(Some(&self.window));
    }
}

/// Where the panel floats: the right part of the stage, 12 px from its edges.
fn panel_rect(w: f32, h: f32) -> graphene::Rect {
    let m = 12.0;
    let pw = if w < 760.0 { w - 2.0 * m } else { (w * 0.58).clamp(440.0, 980.0) };
    graphene::Rect::new(w - pw - m, m, pw, (h - 2.0 * m).max(1.0))
}

fn lerp(a: &graphene::Rect, b: &graphene::Rect, t: f32) -> graphene::Rect {
    let l = |x: f32, y: f32| x + (y - x) * t;
    graphene::Rect::new(l(a.x(), b.x()), l(a.y(), b.y()), l(a.width(), b.width()).max(1.0), l(a.height(), b.height()).max(1.0))
}

fn to_gdk(r: &graphene::Rect) -> gdk::Rectangle {
    gdk::Rectangle::new(r.x().round() as i32, r.y().round() as i32, r.width().round().max(1.0) as i32, r.height().round().max(1.0) as i32)
}

/// Runs `change` under a cross-fade from the window as it looked before.
pub fn cross_fade(overlay: &gtk::Overlay, change: impl FnOnce()) {
    let Some(child) = overlay.child() else {
        change();
        return;
    };
    let still = gtk::WidgetPaintable::new(Some(&child)).current_image();
    change();

    let picture = gtk::Picture::for_paintable(&still);
    picture.set_can_target(false);
    picture.set_content_fit(gtk::ContentFit::Fill);
    overlay.add_overlay(&picture);

    let target = adw::CallbackAnimationTarget::new(glib::clone!(
        #[weak]
        picture,
        move |value| picture.set_opacity(value)
    ));
    let animation = adw::TimedAnimation::builder()
        .widget(overlay)
        .value_from(1.0)
        .value_to(0.0)
        .duration(motion::STATE_MS)
        .easing(motion::EASE)
        .target(&target)
        .build();
    animation.connect_done(glib::clone!(
        #[weak]
        overlay,
        #[weak]
        picture,
        move |_| overlay.remove_overlay(&picture)
    ));
    animation.play();
}
