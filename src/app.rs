//! The application object: every widget handle and all state, on the GTK
//! thread. Driver work is spawned on tokio and reported back through one-shot
//! channels or the `Event` bus.
use crate::config::{self, Config, DocView, Theme};
use crate::events::{self, Event};
use crate::focus::{FocusTracker, Scope};
use crate::keybinds::Keymap;
use crate::mongo::ops::Namespace;
use crate::mongo::{self, Conn, ConnectionId};
use crate::notify::{Notice, Notifier};
use crate::ui::collection::CollectionTab;
use crate::ui::editor_pane::{EditorJob, EditorPane, JobKind};
use crate::ui::palette::Palette;
use crate::ui::sidebar::{Node, Sidebar};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// A background import/export shown in the banner; Cancel aborts it.
pub struct LongOp {
    pub label: String,
    pub conn: ConnectionId,
    /// Server-side cursor to kill on cancel, if any.
    pub ctx: Option<crate::mongo::ops::OpCtx>,
    pub handle: tokio::task::AbortHandle,
    /// A file being written, removed on cancel.
    pub partial_file: Option<PathBuf>,
}

pub struct App {
    pub window: adw::ApplicationWindow,
    pub toast_overlay: adw::ToastOverlay,
    pub banner: adw::Banner,
    pub split: adw::OverlaySplitView,
    pub content_stack: gtk::Stack,
    pub tab_view: adw::TabView,
    pub page_picker: gtk::DropDown,
    pub cmd_revealer: gtk::Revealer,
    pub sidebar: Rc<Sidebar>,
    pub editor_pane: Rc<EditorPane>,
    pub palette: Rc<Palette>,
    pub focus: FocusTracker,
    pub last_pane: Cell<Scope>,
    pub config: RefCell<Config>,
    pub conns: RefCell<Vec<Rc<Conn>>>,
    pub tabs: RefCell<Vec<Rc<CollectionTab>>>,
    pub keymap: RefCell<Keymap>,
    pub events: events::Sender,
    pub notifier: Option<Rc<Notifier>>,
    pub(crate) shortcut_ctl: RefCell<Option<gtk::ShortcutController>>,
    style_provider: gtk::CssProvider,
    style_monitor: RefCell<Option<gio::FileMonitor>>,
    keybindings_monitor: RefCell<Option<gio::FileMonitor>>,
    save_source: RefCell<Option<glib::SourceId>>,
    connecting: RefCell<HashSet<ConnectionId>>,
    /// Which connection the sidebar/tabs consider "current" for `:db`, `:shell`.
    pub current_conn: Cell<Option<ConnectionId>>,
    pub current_db: RefCell<Option<String>>,
    /// `VITI_DEBUG_OPEN=db.coll[,db.coll…]`: open these namespaces once connected.
    pub debug_open: RefCell<Vec<Namespace>>,
    pub long_ops: RefCell<HashMap<uuid::Uuid, LongOp>>,
}

impl App {
    pub fn build(application: &adw::Application) -> Rc<Self> {
        let cfg = config::load();
        let sidebar = Sidebar::new();
        let editor_pane = EditorPane::new();
        let palette = Palette::new();
        let chrome = crate::ui::window::build(
            application,
            sidebar.root.upcast_ref(),
            palette.root.upcast_ref(),
        );
        let (tx, rx) = async_channel::unbounded::<Event>();
        let notifier = Notifier::new();

        let app = Rc::new(Self {
            window: chrome.window,
            toast_overlay: chrome.toast,
            banner: chrome.banner,
            split: chrome.split,
            content_stack: chrome.content_stack,
            tab_view: chrome.tab_view,
            page_picker: chrome.page_picker,
            cmd_revealer: chrome.cmd_revealer,
            sidebar: sidebar.clone(),
            editor_pane: editor_pane.clone(),
            palette: palette.clone(),
            focus: FocusTracker::default(),
            last_pane: Cell::new(Scope::Sidebar),
            config: RefCell::new(cfg),
            conns: RefCell::new(Vec::new()),
            tabs: RefCell::new(Vec::new()),
            keymap: RefCell::new(Keymap::new()),
            events: tx,
            notifier,
            shortcut_ctl: RefCell::new(None),
            style_provider: gtk::CssProvider::new(),
            style_monitor: RefCell::new(None),
            keybindings_monitor: RefCell::new(None),
            save_source: RefCell::new(None),
            connecting: RefCell::new(HashSet::new()),
            current_conn: Cell::new(None),
            current_db: RefCell::new(None),
            long_ops: RefCell::new(HashMap::new()),
            debug_open: RefCell::new(
                std::env::var("VITI_DEBUG_OPEN")
                    .into_iter()
                    .flat_map(|v| {
                        v.split(',')
                            .filter_map(|ns| {
                                let (db, coll) = ns.trim().split_once('.')?;
                                Some(Namespace::new(db, coll))
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect(),
            ),
        });

        // Panes that own vi keys.
        {
            let a = app.clone();
            app.banner
                .connect_button_clicked(move |_| a.cancel_long_ops());
        }
        app.focus.register(&sidebar.sections, Scope::Sidebar);
        app.focus.register(&editor_pane.editor, Scope::Editor);
        app.focus.register(&editor_pane.shell, Scope::Shell);
        app.focus.register(&palette.entry, Scope::Palette);
        sidebar.attach(&app);
        editor_pane.attach(&app.window);
        sidebar.reload_connections();

        app.install_builtin_css();
        app.install_user_css();
        app.watch_user_css();
        app.watch_keybindings();
        app.apply_theme();
        crate::ui::watch_scheme();
        crate::dispatch::install(&app);
        app.install_window_actions();

        {
            let a = app.clone();
            palette.set_on_run(move |line| a.run_command(&line));
        }
        {
            let a = app.clone();
            palette.set_on_close(move || {
                a.cmd_revealer.set_reveal_child(false);
                a.blur_to_pane();
            });
        }
        palette.set_ctx(app.clone());
        {
            let a = app.clone();
            editor_pane.set_on_exit(move |job, status| a.on_editor_exited(job, status));
        }
        {
            let a = app.clone();
            app.tab_view.connect_close_page(move |_, page| {
                a.tabs.borrow_mut().retain(|t| t.page != *page);
                if a.tabs.borrow().is_empty() {
                    a.content_stack.set_visible_child_name("welcome");
                }
                glib::Propagation::Proceed
            });
        }
        {
            let a = app.clone();
            app.tab_view.connect_selected_page_notify(move |_| {
                a.update_title();
                a.sync_page_picker();
            });
        }
        {
            let a = app.clone();
            app.page_picker.connect_selected_notify(move |dd| {
                if let (Some(t), Some(p)) = (
                    a.current_tab(),
                    crate::ui::collection::PAGES.get(dd.selected() as usize),
                ) {
                    t.stack.set_visible_child_name(p.0);
                }
            });
        }
        {
            let a = app.clone();
            app.window.connect_close_request(move |_| {
                a.save_now();
                glib::Propagation::Proceed
            });
        }
        if let Some(n) = &app.notifier {
            let a = app.clone();
            n.set_on_open(move |_, token| {
                if let Some(t) = token {
                    a.window.set_startup_id(&t);
                }
                a.window.present();
            });
        }
        app.pump_events(rx);
        app.update_title();
        app.window.present();
        sidebar.sections.grab_focus();
        app
    }

    // ----- infrastructure ---------------------------------------------------

    fn install_window_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn()>| {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| f());
            self.window.add_action(&a);
        };
        let a = self.clone();
        add(
            "connections",
            Box::new(move || crate::ui::connections::show_manager(&a)),
        );
        let a = self.clone();
        add("help", Box::new(move || crate::ui::help::show(&a)));
        let a = self.clone();
        add("settings", Box::new(move || crate::ui::settings::show(&a)));
        let a = self.clone();
        add(
            "about",
            Box::new(move || {
                let about = adw::AboutDialog::builder()
                    .application_name("Viti")
                    .application_icon(crate::notify::APP_ID)
                    .website("https://github.com/turbinebmw/viti")
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("turbinebmw")
                    .comments("MongoDB Compass, the GTK way, with vi keys.")
                    .license_type(gtk::License::MitX11)
                    .build();
                about.present(Some(&a.window));
            }),
        );
    }

    fn install_builtin_css(&self) {
        if let Some(display) = gtk::gdk::Display::default() {
            crate::accent::install_fallback(&display);
            let css = gtk::CssProvider::new();
            css.load_from_string(crate::style::BUILTIN);
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    fn install_user_css(&self) {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.style_provider,
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );
        }
        self.style_provider
            .connect_parsing_error(|_, section, err| {
                tracing::warn!(
                    "style.css line {}: {err}",
                    section.start_location().lines() + 1
                );
            });
        self.reload_user_css();
    }

    pub fn reload_user_css(&self) {
        self.style_provider.load_from_string(&crate::style::load());
    }

    fn watch_file(
        &self,
        path: PathBuf,
        on_change: impl Fn() + 'static,
    ) -> Option<gio::FileMonitor> {
        let file = gio::File::for_path(&path);
        match file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
            Ok(monitor) => {
                monitor.connect_changed(move |_, _, _, event| {
                    use gio::FileMonitorEvent as E;
                    if matches!(
                        event,
                        E::ChangesDoneHint | E::Renamed | E::MovedIn | E::Created | E::Deleted
                    ) {
                        on_change();
                    }
                });
                Some(monitor)
            }
            Err(e) => {
                tracing::warn!("cannot watch {}: {e}", path.display());
                None
            }
        }
    }

    fn watch_user_css(self: &Rc<Self>) {
        let a = self.clone();
        *self.style_monitor.borrow_mut() =
            self.watch_file(crate::style::path(), move || a.reload_user_css());
    }

    /// keybindings.json is the hand-editable source: reload it into settings
    /// and rebuild the controllers.
    fn watch_keybindings(self: &Rc<Self>) {
        let a = self.clone();
        *self.keybindings_monitor.borrow_mut() =
            self.watch_file(config::keybindings_path(), move || {
                let path = config::keybindings_path();
                if let Ok(text) = std::fs::read_to_string(&path) {
                    match serde_json::from_str::<std::collections::BTreeMap<String, String>>(&text)
                    {
                        Ok(kb) => {
                            if a.config.borrow().settings.keybindings == kb {
                                return;
                            }
                            a.config.borrow_mut().settings.keybindings = kb;
                            a.reinstall_shortcuts();
                            a.toast("Keybindings reloaded");
                        }
                        Err(e) => a.toast(&format!("keybindings.json: {e}")),
                    }
                }
            });
    }

    pub fn apply_theme(&self) {
        let scheme = match self.config.borrow().settings.theme {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    }

    pub fn schedule_save(self: &Rc<Self>) {
        if let Some(src) = self.save_source.borrow_mut().take() {
            src.remove();
        }
        let a = self.clone();
        let id = glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
            a.save_source.borrow_mut().take();
            config::save(&a.config.borrow());
        });
        *self.save_source.borrow_mut() = Some(id);
    }

    pub fn save_now(&self) {
        if let Some(src) = self.save_source.borrow_mut().take() {
            src.remove();
        }
        config::save(&self.config.borrow());
    }

    pub fn toast(&self, msg: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(msg));
    }

    /// Shown to the user and logged at the boundary, naming the resource.
    pub fn toast_error(&self, context: &str, err: &anyhow::Error) {
        tracing::warn!("{context}: {err:#}");
        let toast = adw::Toast::new(&format!("{context}: {err:#}"));
        toast.set_timeout(8);
        self.toast_overlay.add_toast(toast);
    }

    pub fn notify(&self, topic: &str, title: &str, body: &str) {
        let cfg = self.config.borrow();
        if !cfg.settings.desktop_notifications {
            return;
        }
        if let Some(n) = &self.notifier {
            n.send(
                Notice {
                    topic: topic.into(),
                    title: title.into(),
                    body: body.into(),
                },
                &cfg.settings.notification_sound,
            );
        }
    }

    /// Notify only when the user is not looking at the window.
    pub fn notify_if_unfocused(&self, topic: &str, title: &str, body: &str) {
        if !self.window.is_active() {
            self.notify(topic, title, body);
        }
    }

    /// Writes are refused in read-only mode (with a toast).
    pub fn write_guard(&self) -> Result<(), ()> {
        if self.config.borrow().settings.read_only {
            self.toast("Read-only mode is on (Settings › General, or :set readonly off)");
            Err(())
        } else {
            Ok(())
        }
    }

    pub fn max_time_ms(&self) -> u64 {
        self.config.borrow().settings.max_time_ms
    }

    // ----- long operations (import / export) ---------------------------------

    pub fn add_long_op(&self, id: uuid::Uuid, op: LongOp) {
        self.banner.set_title(&op.label);
        self.banner.set_button_label(Some("Cancel"));
        self.banner.set_revealed(true);
        self.long_ops.borrow_mut().insert(id, op);
    }

    pub fn finish_long_op(&self, id: uuid::Uuid) {
        self.long_ops.borrow_mut().remove(&id);
        if self.long_ops.borrow().is_empty() {
            self.banner.set_revealed(false);
            self.banner.set_button_label(None);
        }
    }

    /// The banner's Cancel: abort every running import/export, kill their
    /// cursors and remove half-written files.
    pub fn cancel_long_ops(self: &Rc<Self>) {
        let ops: Vec<LongOp> = self.long_ops.borrow_mut().drain().map(|(_, o)| o).collect();
        if ops.is_empty() {
            self.banner.set_revealed(false);
            return;
        }
        for op in ops {
            op.handle.abort();
            if let Some(ctx) = op.ctx
                && let Some(conn) = self.conn(op.conn)
            {
                let client = conn.client.clone();
                crate::rt::spawn(async move {
                    match crate::mongo::ops::kill_by_comment(&client, &ctx.comment).await {
                        Ok(n) => tracing::info!("killed {n} op(s) for {}", ctx.comment),
                        Err(e) => tracing::warn!("killOp: {e:#}"),
                    }
                });
            }
            if let Some(p) = op.partial_file {
                let _ = std::fs::remove_file(&p);
            }
            tracing::info!("cancelled: {}", op.label);
            self.toast(&format!("Cancelled: {}", op.label));
        }
        self.banner.set_revealed(false);
        self.banner.set_button_label(None);
    }

    pub fn update_title(&self) {
        let ro = if self.config.borrow().settings.read_only {
            " · read-only"
        } else {
            ""
        };
        // Tabs sit in the header, where there is no room for the connection: the
        // tab shows database.collection and the tooltip (and window title) carry
        // connection.database.collection in full.
        let conn_name = |conn| {
            self.config
                .borrow()
                .profile(conn)
                .map(|p| p.short_name())
                .unwrap_or_default()
        };
        for t in self.tabs.borrow().iter() {
            t.page.set_title(&t.ns.to_string());
            t.page
                .set_tooltip(&format!("{}.{}", conn_name(t.conn), t.ns));
        }
        let title = match self.current_tab() {
            Some(t) => format!("{}.{} — Viti{ro}", conn_name(t.conn), t.ns),
            None => format!("Viti{ro}"),
        };
        self.window.set_title(Some(&title));
    }

    // ----- connections ------------------------------------------------------

    pub fn conn(&self, id: ConnectionId) -> Option<Rc<Conn>> {
        self.conns.borrow().iter().find(|c| c.id == id).cloned()
    }

    pub fn connect_profile(self: &Rc<Self>, id: ConnectionId) {
        if self.conn(id).is_some() || !self.connecting.borrow_mut().insert(id) {
            return;
        }
        let Some(profile) = self.config.borrow().profile(id).cloned() else {
            return;
        };
        let store = self.config.borrow().settings.secret_store;
        let name = profile.display_name();
        self.banner.set_title(&format!("Connecting to {name}…"));
        self.banner.set_button_label(None);
        self.banner.set_revealed(true);
        let tx = self.events.clone();
        crate::rt::spawn(async move {
            let secrets =
                tokio::task::spawn_blocking(move || crate::secrets::get_all(store, profile.id))
                    .await
                    .unwrap_or_default();
            let ev = match mongo::connect(&profile, &secrets).await {
                Ok(result) => Event::Connected {
                    conn: profile.id,
                    result,
                },
                Err(e) => Event::ConnectFailed {
                    conn: profile.id,
                    error: format!("{e:#}"),
                },
            };
            let _ = tx.send(ev).await;
        });
    }

    pub fn disconnect(self: &Rc<Self>, id: ConnectionId) {
        let had = {
            let mut conns = self.conns.borrow_mut();
            let before = conns.len();
            conns.retain(|c| c.id != id);
            before != conns.len()
        };
        if !had {
            return;
        }
        // Close its tabs.
        let pages: Vec<adw::TabPage> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| t.conn == id)
            .map(|t| t.page.clone())
            .collect();
        for p in pages {
            self.tab_view.close_page(&p);
        }
        self.sidebar.clear_connection(id);
        if self.current_conn.get() == Some(id) {
            self.current_conn
                .set(self.conns.borrow().first().map(|c| c.id));
        }
        self.toast("Disconnected");
    }

    pub fn load_databases(self: &Rc<Self>, id: ConnectionId) {
        let Some(conn) = self.conn(id) else { return };
        let client = conn.client.clone();
        let a = self.clone();
        glib::spawn_future_local(async move {
            match crate::rt::io(async move { mongo::ops::list_databases(&client).await }).await {
                Ok(dbs) => a.sidebar.set_databases(id, dbs),
                Err(e) => a.toast_error("list databases", &e),
            }
        });
    }

    pub fn load_collections(self: &Rc<Self>, id: ConnectionId, db: &str) {
        let Some(conn) = self.conn(id) else { return };
        let client = conn.client.clone();
        let db = db.to_string();
        let a = self.clone();
        glib::spawn_future_local(async move {
            let db2 = db.clone();
            match crate::rt::io(async move { mongo::ops::list_collections(&client, &db2).await })
                .await
            {
                Ok(colls) => a.sidebar.set_collections(id, &db, colls),
                Err(e) => a.toast_error(&format!("list collections of {db}"), &e),
            }
        });
    }

    fn pump_events(self: &Rc<Self>, rx: events::Receiver) {
        let a = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                a.on_event(ev);
            }
        });
    }

    fn on_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::Connected { conn, result } => {
                self.connecting.borrow_mut().remove(&conn);
                self.banner.set_revealed(false);
                let Some(profile) = self.config.borrow().profile(conn).cloned() else {
                    return;
                };
                let server = result.server.clone();
                self.conns.borrow_mut().push(Rc::new(Conn {
                    id: conn,
                    client: result.client,
                    profile: profile.clone(),
                    server: result.server,
                    tunnel: result.tunnel,
                }));
                if let Some(p) = self.config.borrow_mut().profile_mut(conn) {
                    p.last_used = Some(chrono::Utc::now());
                }
                self.schedule_save();
                self.current_conn.set(Some(conn));
                self.sidebar.refresh_connection(conn);
                self.sidebar.set_active(conn);
                self.toast(&format!(
                    "Connected to {} — MongoDB {} ({})",
                    profile.display_name(),
                    server.version,
                    server.topology
                ));
                let debug_open = std::mem::take(&mut *self.debug_open.borrow_mut());
                if !debug_open.is_empty() {
                    for ns in debug_open {
                        self.open_collection(conn, ns);
                    }
                    // VITI_DEBUG_ACTION=docs.cycle-view,docs.edit-external and
                    // VITI_DEBUG_COMMAND=index;update: run actions / `:` lines
                    // once the page has loaded (dev only).
                    let ids = std::env::var("VITI_DEBUG_ACTION").unwrap_or_default();
                    let cmds = std::env::var("VITI_DEBUG_COMMAND").unwrap_or_default();
                    // VITI_DEBUG_PIPELINE='[{ $match: {} }]' loads (and runs) a
                    // pipeline on the first tab's Aggregations page first.
                    let pipe = std::env::var("VITI_DEBUG_PIPELINE").unwrap_or_default();
                    if !ids.is_empty() || !cmds.is_empty() || !pipe.is_empty() {
                        let a = self.clone();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(1500),
                            move || {
                                if !pipe.is_empty()
                                    && let Some(t) = a.current_tab()
                                {
                                    t.show_page("aggregations");
                                    a.sync_page_picker();
                                    match t.agg.load_text(&pipe) {
                                        Ok(()) => t.agg.run(),
                                        Err(e) => a.toast(&e),
                                    }
                                }
                                for id in ids.split(',').filter(|s| !s.trim().is_empty()) {
                                    a.run_action(id.trim());
                                }
                                for line in cmds.split(';').filter(|s| !s.trim().is_empty()) {
                                    a.run_command(line.trim());
                                }
                            },
                        );
                    }
                }
            }
            Event::ConnectFailed { conn, error } => {
                self.connecting.borrow_mut().remove(&conn);
                self.banner.set_revealed(false);
                let name = self
                    .config
                    .borrow()
                    .profile(conn)
                    .map(|p| p.display_name())
                    .unwrap_or_default();
                tracing::warn!("connect {name}: {error}");
                let toast = adw::Toast::new(&format!("{name}: {error}"));
                toast.set_timeout(10);
                self.toast_overlay.add_toast(toast);
            }
            Event::Disconnected { conn, reason } => {
                self.disconnect(conn);
                if let Some(r) = reason {
                    self.toast(&r);
                }
            }
            Event::Progress {
                op,
                done,
                total,
                label,
            } => {
                // A late event from an operation that was cancelled.
                if !self.long_ops.borrow().contains_key(&op) {
                    return;
                }
                let text = match total {
                    Some(t) => format!(
                        "{label}: {} / {}",
                        crate::ui::thousands(done),
                        crate::ui::thousands(t)
                    ),
                    None => format!("{label}: {}", crate::ui::thousands(done)),
                };
                self.banner.set_title(&text);
                self.banner.set_revealed(true);
            }
            Event::OpDone { op, result } => {
                self.finish_long_op(op);
                self.banner.set_revealed(false);
                match result {
                    Ok(msg) => {
                        self.toast(&msg);
                        self.notify_if_unfocused("op", "Viti", &msg);
                    }
                    Err(e) => self.toast(&e),
                }
            }
        }
    }

    // ----- tabs --------------------------------------------------------------

    pub fn open_collection(self: &Rc<Self>, conn: ConnectionId, ns: Namespace) {
        if let Some(existing) = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.conn == conn && t.ns == ns)
            .cloned()
        {
            self.tab_view.set_selected_page(&existing.page);
            existing.docs.focus_views();
            return;
        }
        let tab = self.new_tab(conn, ns.clone());
        self.content_stack.set_visible_child_name("tabs");
        self.tab_view.set_selected_page(&tab.page);
        self.sync_page_picker();
        self.current_conn.set(Some(conn));
        *self.current_db.borrow_mut() = Some(ns.db.clone());
        self.update_title();
        tab.docs.load();
        tab.docs.focus_views();
    }

    /// Build a tab and register its panes as vi-key owners.
    fn new_tab(self: &Rc<Self>, conn: ConnectionId, ns: Namespace) -> Rc<CollectionTab> {
        let tab = CollectionTab::new(self, conn, ns);
        self.focus.register(&tab.docs.views, Scope::Documents);
        self.focus
            .register(&tab.docs.query_bar.root, Scope::QueryBar);
        self.focus.register(&tab.agg.root, Scope::Aggregation);
        self.focus.register(&tab.explain.root, Scope::Explain);
        self.focus.register(&tab.indexes.root, Scope::Indexes);
        self.focus.register(&tab.schema.root, Scope::Schema);
        self.focus.register(&tab.validation.root, Scope::Validation);
        self.tabs.borrow_mut().push(tab.clone());
        tab
    }

    /// Always a fresh tab, even if the collection is already open.
    pub fn open_collection_new_tab(self: &Rc<Self>, conn: ConnectionId, ns: Namespace) {
        let tab = self.new_tab(conn, ns.clone());
        self.content_stack.set_visible_child_name("tabs");
        self.tab_view.set_selected_page(&tab.page);
        self.sync_page_picker();
        self.current_conn.set(Some(conn));
        *self.current_db.borrow_mut() = Some(ns.db.clone());
        self.update_title();
        tab.docs.load();
        tab.docs.focus_views();
    }

    /// Close every tab of a connection, database or collection.
    pub fn close_tabs(&self, conn: ConnectionId, db: Option<&str>, coll: Option<&str>) {
        let pages: Vec<adw::TabPage> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| t.conn == conn)
            .filter(|t| db.is_none_or(|d| t.ns.db == d))
            .filter(|t| coll.is_none_or(|c| t.ns.coll == c))
            .map(|t| t.page.clone())
            .collect();
        for p in pages {
            self.tab_view.close_page(&p);
        }
    }

    /// After creating/dropping/renaming: refresh the sidebar's database list
    /// and that database's collections.
    pub fn after_namespace_change(self: &Rc<Self>, conn: ConnectionId, db: &str) {
        self.load_databases(conn);
        self.load_collections(conn, db);
    }

    /// Point the header dropdown at the selected tab's current page.
    pub fn sync_page_picker(&self) {
        let tab = self.current_tab();
        self.page_picker.set_visible(tab.is_some());
        if let Some(i) = tab
            .and_then(|t| t.stack.visible_child_name())
            .and_then(|n| crate::ui::collection::page_index(&n))
        {
            self.page_picker.set_selected(i);
        }
    }

    pub fn current_tab(&self) -> Option<Rc<CollectionTab>> {
        let page = self.tab_view.selected_page()?;
        self.tabs.borrow().iter().find(|t| t.page == page).cloned()
    }

    fn close_current_tab(&self) {
        if let Some(p) = self.tab_view.selected_page() {
            self.tab_view.close_page(&p);
        }
    }

    // ----- editor round-trip -------------------------------------------------

    /// Open any file in the embedded editor (style.css, keybindings.json).
    pub fn edit_file_in_pane(self: &Rc<Self>, path: PathBuf, title: &str) {
        let settings = self.config.borrow().settings.clone();
        if self.editor_pane.busy() {
            self.toast("The editor is already open");
            return;
        }
        let mut argv = settings.editor_argv();
        argv.push(path.to_string_lossy().into_owned());
        self.editor_pane.reopen(
            &settings,
            EditorJob {
                id: uuid::Uuid::new_v4(),
                path,
                kind: JobKind::Text {
                    purpose: "file".into(),
                },
                original_text: String::new(),
            },
            title,
        );
    }

    fn on_editor_exited(self: &Rc<Self>, job: EditorJob, status: i32) {
        let cleanup = |job: &EditorJob| {
            if !matches!(job.kind, JobKind::Text { ref purpose } if purpose == "file") {
                let _ = std::fs::remove_file(&job.path);
            }
        };
        let finish = |a: &Rc<Self>| {
            a.editor_pane.hide();
            a.blur_to_pane();
        };
        if let JobKind::Text { purpose } = &job.kind
            && purpose == "file"
        {
            finish(self);
            return;
        }
        if status != 0 {
            self.toast(&format!(
                "Editor exited with status {status}; nothing saved"
            ));
            cleanup(&job);
            finish(self);
            return;
        }
        let text = match std::fs::read_to_string(&job.path) {
            Ok(t) => t,
            Err(e) => {
                self.toast(&format!("could not read the edited file: {e}"));
                finish(self);
                return;
            }
        };
        if text.trim() == job.original_text.trim() {
            self.toast("No changes");
            cleanup(&job);
            finish(self);
            return;
        }
        match job.kind.clone() {
            JobKind::Document {
                conn,
                ns,
                id,
                is_new,
            } => match crate::mongo::ejson::parse_document(&text) {
                Ok(doc) => {
                    cleanup(&job);
                    finish(self);
                    let Some(tab) = self
                        .tabs
                        .borrow()
                        .iter()
                        .find(|t| t.conn == conn && t.ns == ns)
                        .cloned()
                    else {
                        return;
                    };
                    let new_id = doc.get("_id").cloned();
                    if !is_new && id.is_some() && new_id != id {
                        let tab2 = tab.clone();
                        let id2 = id.clone();
                        let doc2 = doc.clone();
                        let dialog = adw::AlertDialog::new(
                            Some("_id changed"),
                            Some(
                                "The document's _id was changed. Replace the original (its _id stays), or insert this as a new document?",
                            ),
                        );
                        dialog.add_responses(&[
                            ("cancel", "Cancel"),
                            ("insert", "Insert as new"),
                            ("replace", "Replace original"),
                        ]);
                        dialog
                            .set_response_appearance("replace", adw::ResponseAppearance::Suggested);
                        dialog.set_close_response("cancel");
                        dialog.connect_response(None, move |_, r| match r {
                            "insert" => tab2.docs.save_document(None, doc2.clone()),
                            "replace" => {
                                let mut d = doc2.clone();
                                if let Some(orig) = &id2 {
                                    d.insert("_id", orig.clone());
                                }
                                tab2.docs.save_document(id2.clone(), d)
                            }
                            _ => {}
                        });
                        dialog.present(Some(&self.window));
                    } else {
                        tab.docs.save_document(if is_new { None } else { id }, doc);
                    }
                }
                Err(e) => self.reedit(job, &e.to_string()),
            },
            JobKind::Documents { conn, ns } => match crate::mongo::ejson::parse_documents(&text) {
                Ok(docs) => {
                    cleanup(&job);
                    finish(self);
                    let Some(c) = self.conn(conn) else { return };
                    let client = c.client.clone();
                    let a = self.clone();
                    glib::spawn_future_local(async move {
                        let ns2 = ns.clone();
                        let r = crate::rt::io(async move {
                            let mut n = 0;
                            for d in docs {
                                match d.get("_id").cloned() {
                                    Some(id) => {
                                        crate::mongo::ops::replace_by_id(&client, &ns2, &id, d)
                                            .await?
                                    }
                                    None => {
                                        crate::mongo::ops::insert_one(&client, &ns2, d).await?;
                                    }
                                }
                                n += 1;
                            }
                            anyhow::Ok(n)
                        })
                        .await;
                        match r {
                            Ok(n) => {
                                a.toast(&format!("Saved {n} documents"));
                                if let Some(tab) = a
                                    .tabs
                                    .borrow()
                                    .iter()
                                    .find(|t| t.conn == conn && t.ns == ns)
                                    .cloned()
                                {
                                    tab.docs.reload_keep_cursor();
                                }
                            }
                            Err(e) => a.toast_error("save documents", &e),
                        }
                    });
                }
                Err(e) => self.reedit(job, &e.to_string()),
            },
            JobKind::Pipeline { conn, ns } => {
                let tab = self
                    .tabs
                    .borrow()
                    .iter()
                    .find(|t| t.conn == conn && t.ns == ns)
                    .cloned();
                let Some(tab) = tab else {
                    cleanup(&job);
                    finish(self);
                    return;
                };
                match tab.agg.load_text(&text) {
                    Ok(()) => {
                        cleanup(&job);
                        finish(self);
                        self.toast("Pipeline updated");
                    }
                    Err(e) => self.reedit(job, &e),
                }
            }
            JobKind::Validation { conn, ns } => {
                match crate::mongo::ejson::parse_document_or_empty(&text) {
                    Ok(_) => {
                        cleanup(&job);
                        finish(self);
                        let tab = self
                            .tabs
                            .borrow()
                            .iter()
                            .find(|t| t.conn == conn && t.ns == ns)
                            .cloned();
                        match tab {
                            Some(t) => {
                                t.validation.set_text(text.trim_end());
                                t.show_page("validation");
                                self.sync_page_picker();
                                self.toast("Rules updated — Save (Ctrl+S) to apply");
                            }
                            None => self.toast("That collection's tab is closed; nothing applied"),
                        }
                    }
                    Err(e) => self.reedit(job, &e.to_string()),
                }
            }
            JobKind::Text { .. } => {
                cleanup(&job);
                finish(self);
            }
        }
    }

    /// The Explain page for the current tab, explaining `source` ("query" or
    /// "pipeline") right away.
    pub fn explain_current(self: &Rc<Self>, source: &str) {
        let Some(t) = self.current_tab() else {
            self.toast("Open a collection first");
            return;
        };
        t.explain.set_source(source);
        t.show_page("explain");
        self.sync_page_picker();
        t.explain.run();
    }

    fn agg(&self) -> Option<Rc<crate::ui::aggregation::AggregationPane>> {
        self.current_tab().map(|t| t.agg.clone())
    }

    /// Parse error after editing: offer to reopen the same file.
    fn reedit(self: &Rc<Self>, job: EditorJob, err: &str) {
        self.editor_pane.hide();
        let toast = adw::Toast::new(&format!("Not saved — {err}"));
        toast.set_timeout(0);
        toast.set_button_label(Some("Re-edit"));
        let a = self.clone();
        let job2 = job.clone();
        toast.connect_button_clicked(move |t| {
            t.dismiss();
            let settings = a.config.borrow().settings.clone();
            a.editor_pane
                .reopen(&settings, job2.clone(), "fix and save again");
        });
        let path = job.path.clone();
        toast.connect_dismissed(move |_| {
            let _ = std::fs::remove_file(&path);
        });
        self.toast_overlay.add_toast(toast);
    }

    // ----- actions and commands ----------------------------------------------

    fn docs(&self) -> Option<Rc<crate::ui::documents::DocumentsPane>> {
        self.current_tab().map(|t| t.docs.clone())
    }

    fn open_palette(&self, initial: &str) {
        self.cmd_revealer.set_reveal_child(true);
        self.palette.open(initial);
    }

    fn toggle_shell(self: &Rc<Self>) {
        let settings = self.config.borrow().settings.clone();
        let conn = self.current_conn.get().and_then(|id| self.conn(id));
        let uri = conn.map(|c| {
            let user = crate::mongo::profile::username(&c.profile.uri);
            let uri = match self.current_db.borrow().as_deref() {
                Some(db)
                    if !c
                        .profile
                        .uri
                        .split("://")
                        .nth(1)
                        .unwrap_or("")
                        .contains('/') =>
                {
                    format!("{}/{db}", c.profile.uri)
                }
                _ => c.profile.uri.clone(),
            };
            (uri, user)
        });
        if uri.is_none() && !self.editor_pane.shell_visible() {
            self.toast("Connect first, then Ctrl+` opens mongosh");
            return;
        }
        self.editor_pane.toggle_shell(&settings, uri);
    }

    pub fn run_action(self: &Rc<Self>, id: &str) -> glib::Propagation {
        let scope = self.focus.current();
        tracing::debug!("action {id} in {scope:?}");
        let stop = glib::Propagation::Stop;
        let proceed = glib::Propagation::Proceed;
        match id {
            // --- global chords ---
            "global.palette" => self.open_palette(""),
            "global.filter" => match scope {
                Scope::Sidebar => {
                    self.sidebar.filter.grab_focus();
                }
                _ => {
                    if let Some(d) = self.docs() {
                        d.query_bar.focus_filter();
                    } else {
                        self.sidebar.filter.grab_focus();
                    }
                }
            },
            "global.help" => crate::ui::help::show(self),
            "global.connections" => crate::ui::connections::show_manager(self),
            "global.focus-next" => {
                self.focus.cycle(1);
            }
            "global.focus-prev" => {
                self.focus.cycle(-1);
            }
            "global.toggle-sidebar" => self.split.set_show_sidebar(!self.split.shows_sidebar()),
            "global.query-options" => match scope {
                Scope::Aggregation => {
                    if let Some(a) = self.agg() {
                        a.toggle_options();
                    }
                }
                _ => {
                    if let Some(d) = self.docs() {
                        d.query_bar.toggle_options();
                    }
                }
            },
            "global.new-tab" => {
                if let Some(t) = self.current_tab() {
                    // Duplicate the current tab's collection in a fresh tab.
                    let (conn, ns) = (t.conn, t.ns.clone());
                    self.open_collection_new_tab(conn, ns);
                }
            }
            "global.my-queries" => crate::ui::my_queries::show(self),
            "global.close-tab" => self.close_current_tab(),
            "global.next-tab" => {
                self.tab_view.select_next_page();
            }
            "global.prev-tab" => {
                self.tab_view.select_previous_page();
            }
            "global.shell" => self.toggle_shell(),
            "global.settings" => crate::ui::settings::show(self),
            "global.refresh" => match scope {
                Scope::Sidebar => self.sidebar.refresh_selected(),
                Scope::Indexes => {
                    if let Some(t) = self.current_tab() {
                        t.indexes.load();
                    }
                }
                Scope::Aggregation => {
                    if let Some(a) = self.agg() {
                        a.run();
                    }
                }
                Scope::Explain => {
                    if let Some(t) = self.current_tab() {
                        t.explain.run();
                    }
                }
                Scope::Schema => {
                    if let Some(t) = self.current_tab() {
                        t.schema.analyze();
                    }
                }
                Scope::Validation => {
                    if let Some(t) = self.current_tab() {
                        t.validation.load();
                    }
                }
                _ => {
                    if let Some(d) = self.docs() {
                        d.load();
                    }
                }
            },
            "global.quit" | "global.quit-key" => {
                if id == "global.quit-key" && scope != Scope::Sidebar && scope != Scope::Global {
                    return proceed;
                }
                self.save_now();
                self.window.close();
            }
            "global.leave-terminal" => {
                self.blur_to_pane();
                if self.focus.current().is_terminal() {
                    self.sidebar.sections.grab_focus();
                }
            }
            // --- normal-mode: navigation ---
            "global.down" | "global.up" => {
                let delta = if id == "global.down" { 1 } else { -1 };
                match scope {
                    Scope::Sidebar => self.sidebar.move_cursor(delta),
                    Scope::Documents => {
                        if let Some(d) = self.docs() {
                            d.move_cursor(delta);
                        }
                    }
                    Scope::Indexes => {
                        if let Some(t) = self.current_tab() {
                            t.indexes.move_cursor(delta);
                        }
                    }
                    Scope::Aggregation => {
                        if let Some(a) = self.agg() {
                            a.move_cursor(delta);
                        }
                    }
                    Scope::Explain => {
                        if let Some(t) = self.current_tab() {
                            t.explain.move_cursor(delta);
                        }
                    }
                    Scope::Schema => {
                        if let Some(t) = self.current_tab() {
                            t.schema.move_cursor(delta);
                        }
                    }
                    _ => return proceed,
                }
            }
            "global.left" | "global.right" => {
                let right = id == "global.right";
                match scope {
                    Scope::Sidebar => {
                        if right {
                            self.sidebar.expand();
                        } else {
                            self.sidebar.collapse();
                        }
                    }
                    Scope::Documents => {
                        if let Some(d) = self.docs() {
                            d.move_column(if right { 1 } else { -1 });
                        }
                    }
                    Scope::Explain => {
                        if let Some(t) = self.current_tab() {
                            t.explain.set_expanded(right);
                        }
                    }
                    Scope::Schema => {
                        if let Some(t) = self.current_tab() {
                            t.schema.move_bar(if right { 1 } else { -1 });
                        }
                    }
                    _ => return proceed,
                }
            }
            "global.top" | "global.bottom" => {
                let top = id == "global.top";
                match scope {
                    Scope::Sidebar => {
                        if top {
                            self.sidebar.move_to(0);
                        } else {
                            self.sidebar.move_to(self.sidebar.last_index());
                        }
                    }
                    Scope::Documents => {
                        if let Some(d) = self.docs() {
                            if top {
                                d.top();
                            } else {
                                d.bottom();
                            }
                        }
                    }
                    Scope::Indexes => {
                        if let Some(t) = self.current_tab() {
                            if top {
                                t.indexes.top();
                            } else {
                                t.indexes.bottom();
                            }
                        }
                    }
                    Scope::Aggregation => {
                        if let Some(a) = self.agg() {
                            if top {
                                a.top();
                            } else {
                                a.bottom();
                            }
                        }
                    }
                    Scope::Explain => {
                        if let Some(t) = self.current_tab() {
                            if top {
                                t.explain.top();
                            } else {
                                t.explain.bottom();
                            }
                        }
                    }
                    Scope::Schema => {
                        if let Some(t) = self.current_tab() {
                            if top {
                                t.schema.top();
                            } else {
                                t.schema.bottom();
                            }
                        }
                    }
                    _ => return proceed,
                }
            }
            "global.escape" => {
                if self.cmd_revealer.reveals_child() {
                    self.palette.close();
                } else if scope == Scope::Aggregation {
                    if !self.agg().is_some_and(|a| a.escape()) {
                        return proceed;
                    }
                } else if scope == Scope::Explain {
                    if let Some(t) = self.current_tab() {
                        t.explain.cancel();
                    }
                } else if scope == Scope::Schema {
                    if let Some(t) = self.current_tab() {
                        t.schema.cancel();
                    }
                } else if scope == Scope::Validation {
                    return proceed;
                } else if let Some(d) = self.docs() {
                    if !d.clear_marks() {
                        d.cancel();
                    }
                } else {
                    return proceed;
                }
            }
            // --- sidebar ---
            "sidebar.filter" => {
                self.sidebar.filter.grab_focus();
            }
            "sidebar.open" => self.sidebar.activate_selected(),
            "sidebar.expand-all" => self.sidebar.expand_all(),
            "sidebar.collapse-all" => self.sidebar.collapse_all(),
            "sidebar.refresh" => self.sidebar.refresh_selected(),
            "sidebar.open-new-tab" => {
                if let Some(Node::Coll { conn, db, name, .. }) = self.sidebar.selected_node() {
                    self.open_collection_new_tab(conn, Namespace::new(&db, &name));
                }
            }
            "sidebar.indexes" => {
                if let Some(Node::Coll { conn, db, name, .. }) = self.sidebar.selected_node() {
                    self.open_collection(conn, Namespace::new(&db, &name));
                    if let Some(t) = self.current_tab() {
                        t.show_page("indexes");
                        self.sync_page_picker();
                    }
                }
            }
            "sidebar.add-collection" => match self.sidebar.selected_node() {
                Some(Node::Conn(conn)) => {
                    if self.conn(conn).is_some() {
                        crate::ui::manage::create_collection(self, conn, None, None);
                    } else {
                        self.toast("Connect first");
                    }
                }
                Some(Node::Db { conn, name }) => {
                    crate::ui::manage::create_collection(self, conn, Some(name), None)
                }
                Some(Node::Coll { conn, db, .. }) => {
                    crate::ui::manage::create_collection(self, conn, Some(db), None)
                }
                None => self.toast("Select a connection or database first"),
            },
            "sidebar.delete" => match self.sidebar.selected_node() {
                Some(Node::Db { conn, name }) => {
                    crate::ui::manage::drop_database(self, conn, &name)
                }
                Some(Node::Coll { conn, db, name, .. }) => {
                    crate::ui::manage::drop_collection(self, conn, Namespace::new(&db, &name))
                }
                Some(Node::Conn(conn)) => crate::ui::connections::remove_profile(self, conn),
                None => {}
            },
            "sidebar.rename" => match self.sidebar.selected_node() {
                Some(Node::Coll { conn, db, name, .. }) => {
                    crate::ui::manage::rename_collection(self, conn, Namespace::new(&db, &name))
                }
                _ => self.toast("Select a collection to rename"),
            },
            // --- documents ---
            _ if id.starts_with("docs.") => {
                let Some(d) = self.docs() else { return proceed };
                match id {
                    "docs.cycle-view" => d.cycle_view(),
                    "docs.peek" | "docs.peek-enter" => d.peek(false),
                    "docs.peek-full" => d.peek(true),
                    "docs.add" => d.add(),
                    "docs.edit" => d.edit(),
                    "docs.edit-external" => d.edit_external(),
                    "docs.duplicate" => d.duplicate(true),
                    "docs.duplicate-now" => d.duplicate(false),
                    "docs.delete" => d.delete(true),
                    "docs.delete-now" => d.delete(false),
                    "docs.select" => d.toggle_mark(),
                    "docs.copy" => d.copy_value(),
                    "docs.copy-doc" => d.copy_document(),
                    "docs.export" => d.export(),
                    "docs.import" => d.import(),
                    "docs.export-language" => d.export_language(),
                    "docs.explain" => self.explain_current("query"),
                    "docs.bulk-update" => d.bulk_update(None),
                    "docs.bulk-delete" => d.bulk_delete(),
                    "docs.refresh" => d.load(),
                    "docs.query" => d.query_bar.focus_filter(),
                    "docs.sort" => d.query_bar.focus_sort(),
                    "docs.sort-column" => d.sort_by_column(),
                    "docs.hide-column" => d.hide_column(),
                    "docs.reset-columns" => d.reset_columns(),
                    "docs.next-doc" => d.move_cursor(1),
                    "docs.prev-doc" => d.move_cursor(-1),
                    "docs.next-page" => d.next_page(),
                    "docs.prev-page" => d.prev_page(),
                    "docs.expand" | "docs.expand-all" => d.toggle_expand_all(),
                    _ => return proceed,
                }
            }
            "query.history" => {
                if let Some(d) = self.docs() {
                    d.open_history();
                }
            }
            "query.clear" => {
                if let Some(d) = self.docs() {
                    d.query_bar.clear_focused();
                }
            }
            "query.favourite" => {
                if let Some(d) = self.docs() {
                    self.save_favourite(&d);
                }
            }
            _ if id.starts_with("idx.") => {
                let Some(t) = self.current_tab() else {
                    return proceed;
                };
                if t.stack.visible_child_name().as_deref() != Some("indexes") {
                    return proceed;
                }
                match id {
                    "idx.add" => t.indexes.add(),
                    "idx.drop" => t.indexes.drop_selected(),
                    "idx.hide" => t.indexes.toggle_hidden(),
                    "idx.peek" | "idx.peek-enter" => t.indexes.peek(),
                    "idx.refresh" => t.indexes.load(),
                    _ => return proceed,
                }
            }
            _ if id.starts_with("agg.") => {
                let Some(t) = self.current_tab() else {
                    return proceed;
                };
                if t.stack.visible_child_name().as_deref() != Some("aggregations") {
                    return proceed;
                }
                let a = t.agg.clone();
                match id {
                    "agg.add-stage" => a.add_stage(),
                    "agg.edit-stage" => a.edit_stage(),
                    "agg.edit-external" => a.edit_external(),
                    "agg.delete-stage" => a.delete_stage(),
                    "agg.run" => a.run(),
                    "agg.clear" => a.clear(),
                    "agg.move-down" => a.move_stage(1),
                    "agg.move-up" => a.move_stage(-1),
                    "agg.toggle-stage" => a.toggle_stage(),
                    "agg.focus-results" => a.toggle_results_focus(),
                    "agg.focus-mode" => a.focus_mode(),
                    "agg.text-mode" => a.toggle_text_mode(),
                    "agg.peek" | "agg.peek-enter" => a.peek(),
                    "agg.next-page" => a.next_page(),
                    "agg.prev-page" => a.prev_page(),
                    "agg.save" => a.save(),
                    "agg.open" => a.open_saved(),
                    "agg.create-view" => a.create_view(),
                    "agg.export-language" => a.export_language(),
                    "agg.export" => crate::ui::export::show(
                        self,
                        a.conn,
                        a.ns.clone(),
                        crate::ui::export::Context::Aggregation(a.clone()),
                        None,
                    ),
                    "agg.explain" => self.explain_current("pipeline"),
                    "agg.preview-toggle" => a.toggle_preview(),
                    _ => return proceed,
                }
            }
            _ if id.starts_with("explain.") => {
                let Some(t) = self.current_tab() else {
                    return proceed;
                };
                if t.stack.visible_child_name().as_deref() != Some("explain") {
                    return proceed;
                }
                let e = t.explain.clone();
                match id {
                    "explain.run" => e.run(),
                    "explain.toggle-view" => e.toggle_view(),
                    "explain.peek" | "explain.peek-enter" => e.peek(),
                    "explain.copy" => e.copy_raw(),
                    "explain.source" => e.toggle_source(),
                    "explain.verbosity" => e.cycle_verbosity(),
                    _ => return proceed,
                }
            }
            _ if id.starts_with("schema.") => {
                let Some(t) = self.current_tab() else {
                    return proceed;
                };
                if t.stack.visible_child_name().as_deref() != Some("schema") {
                    return proceed;
                }
                let s = t.schema.clone();
                match id {
                    "schema.analyze" => s.analyze(),
                    "schema.filter" => s.apply_cursor(),
                    "schema.peek" => s.peek(),
                    "schema.copy-json-schema" => s.copy_json_schema(),
                    _ => return proceed,
                }
            }
            _ if id.starts_with("validation.") => {
                let Some(t) = self.current_tab() else {
                    return proceed;
                };
                if t.stack.visible_child_name().as_deref() != Some("validation") {
                    return proceed;
                }
                let v = t.validation.clone();
                match id {
                    "validation.edit" => v.focus_editor(),
                    "validation.edit-external" => v.edit_external(),
                    "validation.generate" => v.generate(),
                    "validation.refresh" => v.load(),
                    "validation.save" => v.save(),
                    _ => return proceed,
                }
            }
            _ => return proceed,
        }
        stop
    }

    fn save_favourite(self: &Rc<Self>, d: &crate::ui::documents::DocumentsPane) {
        let q = d.query_bar.query();
        let ns = d.ns.to_string();
        let conn = d.conn;
        let dialog = adw::AlertDialog::new(Some("Save query as favourite"), Some(&q.summary()));
        let entry = gtk::Entry::builder()
            .placeholder_text("Name")
            .activates_default(true)
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        dialog.set_default_response(Some("save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let a = self.clone();
        let entry2 = entry.clone();
        dialog.connect_response(None, move |_, r| {
            if r != "save" {
                return;
            }
            let name = entry2.text().trim().to_string();
            a.config.borrow_mut().queries.push(config::SavedQuery {
                name: (!name.is_empty()).then_some(name),
                ns: ns.clone(),
                query: q.clone(),
                favourite: true,
                conn: Some(conn),
                ..Default::default()
            });
            a.schedule_save();
            a.toast("Saved");
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    pub fn run_command(self: &Rc<Self>, line: &str) {
        let inv = match crate::commands::parse(line) {
            Ok(i) => i,
            Err(e) => {
                self.toast(&e);
                return;
            }
        };
        let arg = inv.args.first().cloned().unwrap_or_default();
        match inv.name {
            "db" => {
                if arg.is_empty() {
                    self.toast("usage: :db <name>");
                    return;
                }
                let Some(conn) = self.current_conn.get() else {
                    self.toast("Not connected");
                    return;
                };
                *self.current_db.borrow_mut() = Some(arg.clone());
                self.select_db_row(conn, &arg);
            }
            "coll" => {
                let Some(conn) = self.current_conn.get() else {
                    self.toast("Not connected");
                    return;
                };
                let (db, coll) = match arg.split_once('.') {
                    Some((d, c)) => (d.to_string(), c.to_string()),
                    None => match self.current_db.borrow().clone() {
                        Some(d) => (d, arg.clone()),
                        None => {
                            self.toast("usage: :coll <db>.<collection> or :db first");
                            return;
                        }
                    },
                };
                if coll.is_empty() {
                    self.toast("usage: :coll <name>");
                    return;
                }
                self.open_collection(conn, Namespace::new(&db, &coll));
            }
            "conn" if arg == "new" => crate::ui::connections::show_editor(self, None),
            "conn" => {
                let profiles = self.config.borrow().connections.clone();
                match profiles.iter().find(|p| {
                    p.display_name().eq_ignore_ascii_case(&arg) || p.name.eq_ignore_ascii_case(&arg)
                }) {
                    Some(p) => self.connect_profile(p.id),
                    None => crate::ui::connections::show_manager(self),
                }
            }
            "disconnect" => {
                if let Some(c) = self.current_conn.get() {
                    self.disconnect(c);
                }
            }
            "find" => {
                if let Some(d) = self.docs() {
                    let mut q = d.current_query();
                    q.filter = inv.rest.clone();
                    d.query_bar.set_query(&q);
                    d.run_query(q);
                } else {
                    self.toast("Open a collection first");
                }
            }
            "view" => {
                if let Some(d) = self.docs() {
                    match arg.as_str() {
                        "list" => d.set_view(DocView::List),
                        "json" => d.set_view(DocView::Json),
                        "table" => d.set_view(DocView::Table),
                        _ => self.toast("usage: :view list|json|table"),
                    }
                }
            }
            "page" => {
                if let (Some(d), Ok(n)) = (self.docs(), arg.parse::<u64>()) {
                    d.goto_page(n);
                }
            }
            "set" => self.set_setting(&arg, inv.args.get(1).map(String::as_str)),
            "explain" => {
                let on_agg = self.current_tab().is_some_and(|t| {
                    t.stack.visible_child_name().as_deref() == Some("aggregations")
                });
                self.explain_current(if on_agg { "pipeline" } else { "query" });
            }
            "agg" | "index" | "schema" | "validation" => {
                if let Some(t) = self.current_tab() {
                    t.show_page(match inv.name {
                        "agg" => "aggregations",
                        "index" => "indexes",
                        "validation" => "validation",
                        _ => "schema",
                    });
                    self.sync_page_picker();
                } else {
                    self.toast("Open a collection first");
                }
            }
            "mkdb" => {
                let Some(conn) = self.current_conn.get() else {
                    self.toast("Not connected");
                    return;
                };
                crate::ui::manage::create_collection(self, conn, None, None);
            }
            "mkcoll" => {
                let Some(conn) = self.current_conn.get() else {
                    self.toast("Not connected");
                    return;
                };
                let db = match arg.split_once('.') {
                    Some((d, _)) => Some(d.to_string()),
                    None => self.current_db.borrow().clone(),
                };
                match db {
                    Some(db) => crate::ui::manage::create_collection(self, conn, Some(db), None),
                    None => self.toast("usage: :mkcoll <db>.<name>, or :db first"),
                }
            }
            "drop" => {
                let Some(conn) = self.current_conn.get() else {
                    self.toast("Not connected");
                    return;
                };
                if arg.is_empty() {
                    match self.current_tab() {
                        Some(t) => crate::ui::manage::drop_collection(self, t.conn, t.ns.clone()),
                        None => self.toast("usage: :drop <db>.<collection> | :drop <db>"),
                    }
                } else if let Some((db, coll)) = arg.split_once('.') {
                    crate::ui::manage::drop_collection(self, conn, Namespace::new(db, coll));
                } else if let Some(db) = self.current_db.borrow().clone()
                    && self
                        .sidebar
                        .collections(conn)
                        .iter()
                        .any(|(d, c)| *d == db && *c == arg)
                {
                    crate::ui::manage::drop_collection(self, conn, Namespace::new(&db, &arg));
                } else {
                    crate::ui::manage::drop_database(self, conn, &arg);
                }
            }
            "rename" => match self.current_tab() {
                Some(t) => crate::ui::manage::rename_collection(self, t.conn, t.ns.clone()),
                None => self.toast("Open a collection first"),
            },
            "update" => match self.docs() {
                Some(d) => d.bulk_update((!inv.rest.is_empty()).then(|| inv.rest.clone())),
                None => self.toast("Open a collection first"),
            },
            "delete" => match self.docs() {
                Some(d) => d.bulk_delete(),
                None => self.toast("Open a collection first"),
            },
            "queries" => crate::ui::my_queries::show(self),
            "export" if arg == "language" || arg == "lang" => match self.current_tab() {
                Some(t) if t.stack.visible_child_name().as_deref() == Some("aggregations") => {
                    t.agg.export_language()
                }
                Some(t) => t.docs.export_language(),
                None => self.toast("Open a collection first"),
            },
            "export" => match self.current_tab() {
                Some(t) => {
                    let format = match arg.as_str() {
                        "json" | "csv" => Some(arg.as_str()),
                        "" => None,
                        other => {
                            self.toast(&format!(
                                "unknown export format `{other}` (json, csv, language)"
                            ));
                            return;
                        }
                    };
                    let on_agg = t.stack.visible_child_name().as_deref() == Some("aggregations");
                    let ctx = if on_agg {
                        crate::ui::export::Context::Aggregation(t.agg.clone())
                    } else {
                        crate::ui::export::Context::Documents(t.docs.clone())
                    };
                    crate::ui::export::show(self, t.conn, t.ns.clone(), ctx, format);
                }
                None => self.toast("Open a collection first"),
            },
            "import" => match self.current_tab() {
                Some(t) => {
                    let path = inv.args.first().map(|p| PathBuf::from(shellexpand_home(p)));
                    crate::ui::import::show(self, t.conn, t.ns.clone(), Some(t.docs.clone()), path);
                }
                None => self.toast("Open a collection first"),
            },
            "ai" => self.toast("AI generation arrives in phase 5"),
            "shell" => self.toggle_shell(),
            "settings" => crate::ui::settings::show(self),
            "help" => crate::ui::help::show(self),
            "quit" => {
                self.save_now();
                self.window.close();
            }
            other => self.toast(&format!("unhandled command {other}")),
        }
    }

    fn set_setting(self: &Rc<Self>, key: &str, value: Option<&str>) {
        let on = |v: Option<&str>| matches!(v, None | Some("on") | Some("true") | Some("1"));
        match key {
            "readonly" => {
                let v = on(value);
                self.config.borrow_mut().settings.read_only = v;
                self.update_title();
                self.toast(if v {
                    "Read-only mode on"
                } else {
                    "Read-only mode off"
                });
            }
            "pagesize" => match value.and_then(|v| v.parse::<u32>().ok()) {
                Some(n) if [25, 50, 75, 100].contains(&n) => {
                    self.config.borrow_mut().settings.page_size = n;
                    if let Some(d) = self.docs() {
                        d.page_size.set(n as u64);
                        d.page.set(0);
                        d.load();
                    }
                }
                _ => self.toast("usage: :set pagesize 25|50|75|100"),
            },
            "view" => self.run_command(&format!("view {}", value.unwrap_or(""))),
            "theme" => {
                let t = match value {
                    Some("light") => Theme::Light,
                    Some("dark") => Theme::Dark,
                    _ => Theme::System,
                };
                self.config.borrow_mut().settings.theme = t;
                self.apply_theme();
            }
            "maxtime" => match value.and_then(|v| v.parse::<u64>().ok()) {
                Some(ms) => self.config.borrow_mut().settings.max_time_ms = ms,
                None => self.toast("usage: :set maxtime <ms>"),
            },
            _ => {
                self.toast("settings: readonly, pagesize, view, theme, maxtime");
                return;
            }
        }
        self.schedule_save();
    }

    /// `:db name`: expand the connection and select that database's row.
    fn select_db_row(&self, conn: ConnectionId, db: &str) {
        if self.sidebar.select_db(conn, db) {
            self.sidebar.sections.grab_focus();
        } else {
            self.toast(&format!("no database {db} (expand the connection first)"));
        }
    }
}

impl crate::commands::CompletionCtx for App {
    fn databases(&self) -> Vec<String> {
        let Some(conn) = self.current_conn.get() else {
            return vec![];
        };
        self.sidebar.databases(conn)
    }
    fn collections(&self) -> Vec<String> {
        let Some(conn) = self.current_conn.get() else {
            return vec![];
        };
        let cur = self.current_db.borrow().clone();
        self.sidebar
            .collections(conn)
            .into_iter()
            .map(|(db, name)| {
                if cur.as_deref() == Some(db.as_str()) {
                    name
                } else {
                    format!("{db}.{name}")
                }
            })
            .collect()
    }
    fn connections(&self) -> Vec<String> {
        self.config
            .borrow()
            .connections
            .iter()
            .map(|p| p.display_name())
            .collect()
    }
}

/// `~/x` -> `$HOME/x` for `:import` paths.
fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::Path::new(&home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    p.to_string()
}
