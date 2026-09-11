//! Connections and their database/collection trees. One `Section` per saved
//! connection: a header row plus its own scrolled `gtk::ListView` over a
//! `TreeListModel` (Db -> Coll); every row's item is a `BoxedAnyObject` holding
//! a `Node`. Only the *active* connection is expanded (all connected ones while
//! filtering, sharing the height equally); the others stay as header rows.
//! Collection stores are kept in `children` so async loaders can fill them
//! after the row has been expanded.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ops::{CollInfo, CollKind, DbInfo, Namespace};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Conn(ConnectionId),
    Db {
        conn: ConnectionId,
        name: String,
    },
    Coll {
        conn: ConnectionId,
        db: String,
        name: String,
        kind: CollKind,
    },
}

impl Node {
    fn key(&self) -> String {
        match self {
            Node::Conn(id) => format!("c:{id}"),
            Node::Db { conn, name } => format!("d:{conn}:{name}"),
            Node::Coll { conn, db, name, .. } => format!("x:{conn}:{db}:{name}"),
        }
    }
    pub fn conn(&self) -> ConnectionId {
        match self {
            Node::Conn(id) => *id,
            Node::Db { conn, .. } | Node::Coll { conn, .. } => *conn,
        }
    }
}

/// Where the vi cursor is: a section's header, or a row of its tree (the row
/// itself is the section's `SingleSelection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cursor {
    section: usize,
    header: bool,
}

/// One connection: header + its database tree.
struct Section {
    id: ConnectionId,
    root: gtk::Box,
    header: gtk::Box,
    icon: gtk::Image,
    dot: gtk::Label,
    name: gtk::Label,
    scroller: gtk::ScrolledWindow,
    list: gtk::ListView,
    roots: gio::ListStore,
    tree: gtk::TreeListModel,
    filter_model: gtk::FilterListModel,
    selection: gtk::SingleSelection,
}

impl Section {
    fn is_expanded(&self) -> bool {
        self.scroller.is_visible()
    }

    fn selected_row(&self) -> Option<gtk::TreeListRow> {
        self.selection
            .selected_item()
            .and_downcast::<gtk::TreeListRow>()
    }

    fn expand_all(&self) {
        let mut i = 0;
        while i < self.tree.n_items() {
            if let Some(row) = self.tree.row(i)
                && row.is_expandable()
            {
                row.set_expanded(true);
            }
            i += 1;
        }
    }

    fn collapse_all(&self) {
        for i in (0..self.tree.n_items()).rev() {
            if let Some(row) = self.tree.row(i)
                && row.depth() == 0
            {
                row.set_expanded(false);
            }
        }
    }

    fn unselect(&self) {
        if self.selection.selected() != gtk::INVALID_LIST_POSITION {
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    }
}

pub struct Sidebar {
    pub root: gtk::Box,
    pub filter: gtk::SearchEntry,
    /// The focusable pane that owns the vi keys; holds one `Section` per profile.
    pub sections: gtk::Box,
    sects: RefCell<Vec<Rc<Section>>>,
    active: Cell<Option<ConnectionId>>,
    cursor: Cell<Option<Cursor>>,
    /// Guards against selection-notify re-entering `set_cursor`.
    syncing: Cell<bool>,
    /// Collection stores per database key.
    children: RefCell<HashMap<String, gio::ListStore>>,
    /// Extra per-node text (counts, sizes) shown dimmed after the name.
    details: RefCell<HashMap<String, String>>,
    app: RefCell<Weak<App>>,
}

impl Sidebar {
    pub fn new() -> Rc<Self> {
        let filter = gtk::SearchEntry::builder()
            .placeholder_text("Filter (/)")
            .build();
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        top.set_margin_start(6);
        top.set_margin_end(6);
        top.set_margin_top(6);
        top.set_margin_bottom(4);
        filter.set_hexpand(true);
        top.append(&filter);

        let sections = gtk::Box::new(gtk::Orientation::Vertical, 4);
        sections.set_vexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-sidebar");
        root.append(&top);
        root.append(&sections);

        let sb = Rc::new(Self {
            root,
            filter,
            sections,
            sects: RefCell::new(Vec::new()),
            active: Cell::new(None),
            cursor: Cell::new(None),
            syncing: Cell::new(false),
            children: RefCell::new(HashMap::new()),
            details: RefCell::new(HashMap::new()),
            app: RefCell::new(Weak::new()),
        });

        {
            let sb2 = sb.clone();
            sb.filter
                .connect_search_changed(move |e| sb2.apply_filter(&e.text()));
        }
        {
            let sb2 = sb.clone();
            sb.filter.connect_activate(move |_| {
                sb2.sections.grab_focus();
                if sb2.cursor.get().is_none() {
                    sb2.move_cursor(1);
                }
            });
        }
        {
            let sb2 = sb.clone();
            sb.filter.connect_stop_search(move |_| {
                sb2.sections.grab_focus();
            });
        }
        sb
    }

    pub fn attach(&self, app: &Rc<App>) {
        *self.app.borrow_mut() = Rc::downgrade(app);
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.borrow().upgrade()
    }

    // ----- sections ------------------------------------------------------------

    fn build_section(self: &Rc<Self>, id: ConnectionId) -> Rc<Section> {
        let me = Rc::downgrade(self);
        let roots = gio::ListStore::new::<BoxedAnyObject>();
        let tree = {
            let me = me.clone();
            gtk::TreeListModel::new(roots.clone(), false, false, move |item| {
                let node = item
                    .downcast_ref::<BoxedAnyObject>()?
                    .borrow::<Node>()
                    .clone();
                let sb = me.upgrade()?;
                sb.child_store(&node)
            })
        };
        let filter_model = gtk::FilterListModel::new(Some(tree.clone()), None::<gtk::CustomFilter>);
        let selection = gtk::SingleSelection::new(Some(filter_model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        {
            let me = me.clone();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let expander = gtk::TreeExpander::new();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let icon = gtk::Image::new();
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .hexpand(true)
                    .build();
                let detail = gtk::Label::builder()
                    .xalign(1.0)
                    .css_classes(["viti-count"])
                    .build();
                row.append(&icon);
                row.append(&label);
                row.append(&detail);
                expander.set_child(Some(&row));
                item.set_child(Some(&expander));
                // Right-click context menu.
                let click = gtk::GestureClick::new();
                click.set_button(3);
                let me = me.clone();
                let item_weak = item.downgrade();
                click.connect_pressed(move |g, _, x, y| {
                    let Some(sb) = me.upgrade() else { return };
                    let Some(item) = item_weak.upgrade() else {
                        return;
                    };
                    let Some(w) = g.widget() else { return };
                    if let Some(i) = sb.index_of(id) {
                        sb.sect(i).selection.set_selected(item.position());
                        sb.set_cursor(Cursor {
                            section: i,
                            header: false,
                        });
                    }
                    sb.context_menu(&w, x, y);
                });
                expander.add_controller(click);
            });
        }
        {
            let me = me.clone();
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
                    return;
                };
                let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
                expander.set_list_row(Some(&row));
                let hbox = expander.child().and_downcast::<gtk::Box>().unwrap();
                let icon = hbox.first_child().and_downcast::<gtk::Image>().unwrap();
                let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let detail = label.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let Some(obj) = row.item().and_downcast::<BoxedAnyObject>() else {
                    return;
                };
                let node = obj.borrow::<Node>().clone();
                let Some(sb) = me.upgrade() else { return };
                match &node {
                    Node::Conn(_) => {}
                    Node::Db { name, .. } => {
                        icon.set_icon_name(Some("folder-symbolic"));
                        label.set_text(name);
                    }
                    Node::Coll { name, kind, .. } => {
                        icon.set_icon_name(Some(match kind {
                            CollKind::View => "view-list-symbolic",
                            CollKind::TimeSeries => "alarm-symbolic",
                            _ => "text-x-generic-symbolic",
                        }));
                        label.set_text(name);
                    }
                }
                detail.set_text(
                    sb.details
                        .borrow()
                        .get(&node.key())
                        .map(String::as_str)
                        .unwrap_or(""),
                );
            });
        }

        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_single_click_activate(false);
        list.set_can_focus(false);
        list.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .visible(false)
            .can_focus(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        // Header: icon, colour dot, name, detail.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("header");
        let icon = gtk::Image::new();
        let dot = gtk::Label::builder().visible(false).build();
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        header.append(&icon);
        header.append(&dot);
        header.append(&name);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-conn");
        root.append(&header);
        root.append(&scroller);

        let section = Rc::new(Section {
            id,
            root,
            header,
            icon,
            dot,
            name,
            scroller,
            list,
            roots,
            tree,
            filter_model,
            selection,
        });

        // Header clicks: select (1 click), activate (2), context menu (right).
        {
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let me = me.clone();
            click.connect_pressed(move |_, n, _, _| {
                let Some(sb) = me.upgrade() else { return };
                let Some(i) = sb.index_of(id) else { return };
                sb.set_cursor(Cursor {
                    section: i,
                    header: true,
                });
                sb.sections.grab_focus();
                if n == 2 {
                    sb.activate_selected();
                }
            });
            section.header.add_controller(click);
        }
        {
            let click = gtk::GestureClick::new();
            click.set_button(3);
            let me = me.clone();
            click.connect_pressed(move |g, _, x, y| {
                let Some(sb) = me.upgrade() else { return };
                let Some(i) = sb.index_of(id) else { return };
                let Some(w) = g.widget() else { return };
                sb.set_cursor(Cursor {
                    section: i,
                    header: true,
                });
                sb.context_menu(&w, x, y);
            });
            section.header.add_controller(click);
        }
        // Row clicks move the cursor into this section and focus the pane.
        {
            let click = gtk::GestureClick::new();
            click.set_button(1);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let me = me.clone();
            click.connect_pressed(move |_, _, _, _| {
                if let Some(sb) = me.upgrade() {
                    sb.sections.grab_focus();
                }
            });
            section.list.add_controller(click);
        }
        {
            let me = me.clone();
            section.selection.connect_selected_notify(move |sel| {
                let Some(sb) = me.upgrade() else { return };
                if sb.syncing.get() || sel.selected() == gtk::INVALID_LIST_POSITION {
                    return;
                }
                if let Some(i) = sb.index_of(id) {
                    sb.set_cursor(Cursor {
                        section: i,
                        header: false,
                    });
                }
            });
        }
        {
            let me = me.clone();
            section.list.connect_activate(move |_, pos| {
                let Some(sb) = me.upgrade() else { return };
                if let Some(i) = sb.index_of(id) {
                    sb.sect(i).selection.set_selected(pos);
                    sb.set_cursor(Cursor {
                        section: i,
                        header: false,
                    });
                    sb.activate_selected();
                }
            });
        }
        section
    }

    fn sect(&self, i: usize) -> Rc<Section> {
        self.sects.borrow()[i].clone()
    }

    fn index_of(&self, id: ConnectionId) -> Option<usize> {
        self.sects.borrow().iter().position(|s| s.id == id)
    }

    fn section_of(&self, id: ConnectionId) -> Option<Rc<Section>> {
        self.sects.borrow().iter().find(|s| s.id == id).cloned()
    }

    fn is_connected(&self, id: ConnectionId) -> bool {
        self.app().is_some_and(|a| a.conn(id).is_some())
    }

    fn filtering(&self) -> bool {
        !self.filter.text().trim().is_empty()
    }

    /// Apply the expansion rules: the active section fills the pane, all
    /// connected ones share it while filtering, the rest are header rows.
    fn relayout(&self) {
        let filtering = self.filtering();
        let active = self.active.get();
        let mut any_connected = false;
        let sects = self.sects.borrow().clone();
        for s in &sects {
            let connected = self.is_connected(s.id);
            any_connected |= connected;
            let expanded = connected && (filtering || Some(s.id) == active);
            s.scroller.set_visible(expanded);
            s.root.set_vexpand(expanded);
            if Some(s.id) == active {
                s.root.add_css_class("active");
            } else {
                s.root.remove_css_class("active");
            }
        }
        // Only expanded sections share the extra height; homogeneous sizing
        // would also stretch disconnected sections with just a header.
        self.sections.set_homogeneous(false);
        if any_connected {
            self.root.remove_css_class("idle");
        } else {
            self.root.add_css_class("idle");
        }
        // A cursor inside a now-collapsed section goes to its header.
        if let Some(c) = self.cursor.get()
            && !c.header
            && sects.get(c.section).is_some_and(|s| !s.is_expanded())
        {
            self.set_cursor(Cursor {
                section: c.section,
                header: true,
            });
        }
    }

    /// Update a section's header (icon, name, colour) from the profile.
    fn refresh_section(&self, s: &Section) {
        let Some(app) = self.app() else { return };
        let cfg = app.config.borrow();
        let profile = cfg.profile(s.id);
        s.name.set_text(
            &profile
                .map(|p| p.display_name())
                .unwrap_or_else(|| s.id.to_string()),
        );
        s.icon.set_icon_name(Some(if app.conn(s.id).is_some() {
            "network-server-symbolic"
        } else {
            "network-offline-symbolic"
        }));
        match profile.and_then(|p| p.colour.clone()) {
            Some(colour) => {
                s.dot
                    .set_markup(&format!("<span foreground=\"{colour}\">●</span>"));
                s.dot.set_visible(true);
            }
            None => s.dot.set_visible(false),
        }
    }

    /// Make a connection the expanded one and put the cursor on its header.
    pub fn set_active(&self, id: ConnectionId) {
        self.active.set(Some(id));
        let Some(i) = self.index_of(id) else { return };
        let s = self.sect(i);
        if s.roots.n_items() == 0
            && let Some(app) = self.app()
        {
            app.load_databases(id);
        }
        self.relayout();
        self.set_cursor(Cursor {
            section: i,
            header: true,
        });
    }

    /// Child model for a database node: a (possibly still empty) store, loading on demand.
    fn child_store(self: &Rc<Self>, node: &Node) -> Option<gio::ListModel> {
        match node {
            Node::Coll { .. } | Node::Conn(_) => None,
            Node::Db { conn, name } => {
                let app = self.app()?;
                let store = self.store_for(&node.key());
                if store.n_items() == 0 {
                    app.load_collections(*conn, name);
                }
                Some(store.upcast())
            }
        }
    }

    fn store_for(&self, key: &str) -> gio::ListStore {
        self.children
            .borrow_mut()
            .entry(key.to_string())
            .or_insert_with(gio::ListStore::new::<BoxedAnyObject>)
            .clone()
    }

    /// Rebuild the sections from the saved profiles (order: favourites, then by
    /// name), keeping already-built sections so loaded trees survive.
    pub fn reload_connections(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        let mut profiles = app.config.borrow().connections.clone();
        profiles.sort_by(|a, b| {
            b.favourite.cmp(&a.favourite).then_with(|| {
                a.display_name()
                    .to_lowercase()
                    .cmp(&b.display_name().to_lowercase())
            })
        });
        let cursor_id = self.cursor_node().map(|n| n.conn());
        let old: Vec<Rc<Section>> = self.sects.borrow().clone();
        let mut new = Vec::with_capacity(profiles.len());
        for p in &profiles {
            let s = old
                .iter()
                .find(|s| s.id == p.id)
                .cloned()
                .unwrap_or_else(|| self.build_section(p.id));
            new.push(s);
        }
        for s in &old {
            s.root.unparent();
        }
        for s in &new {
            self.sections.append(&s.root);
            self.refresh_section(s);
        }
        *self.sects.borrow_mut() = new;
        if self
            .active
            .get()
            .is_some_and(|id| self.index_of(id).is_none())
        {
            self.active.set(None);
        }
        self.relayout();
        // Keep the cursor on the same connection if it still exists.
        let target = cursor_id.and_then(|id| self.index_of(id));
        match (target, self.cursor.get()) {
            (Some(i), Some(c)) if c.section == i => {}
            (Some(i), _) => self.set_cursor(Cursor {
                section: i,
                header: true,
            }),
            (None, _) => {
                self.cursor.set(None);
                if !self.sects.borrow().is_empty() {
                    self.set_cursor(Cursor {
                        section: 0,
                        header: true,
                    });
                }
            }
        }
    }

    /// Refresh a connection's header (icon/name), keeping expansion state.
    pub fn refresh_connection(&self, id: ConnectionId) {
        if let Some(s) = self.section_of(id) {
            self.refresh_section(&s);
        }
        self.relayout();
    }

    /// Drop a connection's cached tree (on disconnect).
    pub fn clear_connection(&self, id: ConnectionId) {
        let prefix_d = format!("d:{id}:");
        self.children
            .borrow_mut()
            .retain(|k, _| !k.starts_with(&prefix_d));
        self.details
            .borrow_mut()
            .retain(|k, _| !k.contains(&id.to_string()));
        if let Some(s) = self.section_of(id) {
            s.unselect();
            s.roots.remove_all();
        }
        if self.active.get() == Some(id) {
            let next = self
                .sects
                .borrow()
                .iter()
                .map(|s| s.id)
                .find(|&other| other != id && self.is_connected(other));
            self.active.set(next);
        }
        self.refresh_connection(id);
    }

    pub fn set_databases(&self, conn: ConnectionId, dbs: Vec<DbInfo>) {
        let Some(s) = self.section_of(conn) else {
            return;
        };
        s.roots.remove_all();
        let nodes: Vec<Node> = dbs
            .iter()
            .map(|db| Node::Db {
                conn,
                name: db.name.clone(),
            })
            .collect();
        {
            // Appending re-binds rows synchronously, and bind reads `details`.
            let mut details = self.details.borrow_mut();
            for (node, db) in nodes.iter().zip(&dbs) {
                details.insert(node.key(), crate::ui::human_bytes(db.size_on_disk));
            }
        }
        // Collections of a database that was open get reloaded lazily.
        let prefix = format!("d:{conn}:");
        self.children
            .borrow_mut()
            .retain(|k, _| !k.starts_with(&prefix));
        for node in nodes {
            s.roots.append(&BoxedAnyObject::new(node));
        }
        if self.filtering() {
            s.expand_all();
        }
    }

    pub fn set_collections(&self, conn: ConnectionId, db: &str, colls: Vec<CollInfo>) {
        let key = Node::Db {
            conn,
            name: db.to_string(),
        }
        .key();
        let store = self.store_for(&key);
        store.remove_all();
        self.details
            .borrow_mut()
            .insert(key, format!("{}", colls.len()));
        for c in colls {
            store.append(&BoxedAnyObject::new(Node::Coll {
                conn,
                db: db.to_string(),
                name: c.name,
                kind: c.kind,
            }));
        }
        self.refresh_db_row(conn, db);
    }

    fn refresh_db_row(&self, conn: ConnectionId, db: &str) {
        let Some(s) = self.section_of(conn) else {
            return;
        };
        for i in 0..s.roots.n_items() {
            let Some(obj) = s.roots.item(i).and_downcast::<BoxedAnyObject>() else {
                continue;
            };
            if matches!(&*obj.borrow::<Node>(), Node::Db { name, .. } if name == db) {
                s.roots.items_changed(i, 1, 1);
            }
        }
    }

    // ----- cursor --------------------------------------------------------------

    fn set_cursor(&self, c: Cursor) {
        let sects = self.sects.borrow().clone();
        if c.section >= sects.len() {
            return;
        }
        self.syncing.set(true);
        for (i, s) in sects.iter().enumerate() {
            let on_header = i == c.section && c.header;
            if on_header {
                s.header.add_css_class("selected");
            } else {
                s.header.remove_css_class("selected");
            }
            if i != c.section || c.header {
                s.unselect();
            }
        }
        let s = &sects[c.section];
        if !c.header {
            let pos = s.selection.selected();
            if pos == gtk::INVALID_LIST_POSITION {
                // Nothing selected in the tree: fall back to the header.
                self.syncing.set(false);
                self.set_cursor(Cursor {
                    section: c.section,
                    header: true,
                });
                return;
            }
            s.list.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        }
        self.cursor.set(Some(c));
        self.syncing.set(false);
    }

    /// The virtual, linear list the vi cursor walks: each header, then the
    /// rows of every expanded section.
    fn entries(&self) -> Vec<(Cursor, u32)> {
        let mut out = Vec::new();
        for (i, s) in self.sects.borrow().iter().enumerate() {
            out.push((
                Cursor {
                    section: i,
                    header: true,
                },
                0,
            ));
            if s.is_expanded() {
                for r in 0..s.selection.n_items() {
                    out.push((
                        Cursor {
                            section: i,
                            header: false,
                        },
                        r,
                    ));
                }
            }
        }
        out
    }

    fn cursor_index(&self, entries: &[(Cursor, u32)]) -> Option<usize> {
        let c = self.cursor.get()?;
        let row = if c.header {
            0
        } else {
            self.sect(c.section).selection.selected()
        };
        entries.iter().position(|(e, r)| *e == c && *r == row)
    }

    fn go_to_entry(&self, (c, row): (Cursor, u32)) {
        if !c.header {
            self.syncing.set(true);
            self.sect(c.section).selection.set_selected(row);
            self.syncing.set(false);
        }
        self.set_cursor(c);
    }

    pub fn move_cursor(&self, delta: i64) {
        let entries = self.entries();
        if entries.is_empty() {
            return;
        }
        let n = entries.len() as i64;
        let next = match self.cursor_index(&entries) {
            None => {
                if delta > 0 {
                    0
                } else {
                    n - 1
                }
            }
            Some(cur) => (cur as i64 + delta).clamp(0, n - 1),
        };
        self.go_to_entry(entries[next as usize]);
    }

    pub fn move_to(&self, pos: u32) {
        let entries = self.entries();
        if let Some(e) = entries.get((pos as usize).min(entries.len().saturating_sub(1))) {
            self.go_to_entry(*e);
        }
    }

    pub fn last_index(&self) -> u32 {
        (self.entries().len().saturating_sub(1)) as u32
    }

    fn cursor_node(&self) -> Option<Node> {
        let c = self.cursor.get()?;
        let s = self.sects.borrow().get(c.section)?.clone();
        if c.header {
            return Some(Node::Conn(s.id));
        }
        let row = s.selected_row()?;
        node_of(&row.item()?)
    }

    pub fn selected_node(&self) -> Option<Node> {
        self.cursor_node()
    }

    /// The tree row under the cursor, if it is on one.
    fn cursor_row(&self) -> Option<(Rc<Section>, gtk::TreeListRow)> {
        let c = self.cursor.get()?;
        if c.header {
            return None;
        }
        let s = self.sects.borrow().get(c.section)?.clone();
        let row = s.selected_row()?;
        Some((s, row))
    }

    // ----- actions -------------------------------------------------------------

    /// Enter / double-click: open a collection, expand a database, activate or
    /// connect a connection.
    pub fn activate_selected(&self) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let Some(app) = self.app() else { return };
        match node {
            Node::Coll { conn, db, name, .. } => {
                app.open_collection(conn, Namespace::new(&db, &name))
            }
            Node::Conn(id) => {
                if app.conn(id).is_none() {
                    app.connect_profile(id);
                } else {
                    self.set_active(id);
                }
            }
            Node::Db { .. } => self.toggle_expand(),
        }
    }

    pub fn toggle_expand(&self) {
        if let Some((_, row)) = self.cursor_row()
            && row.is_expandable()
        {
            row.set_expanded(!row.is_expanded());
        }
    }

    /// `l`: expand a database, open a collection, activate or connect a connection.
    pub fn expand(&self) {
        match self.cursor_row() {
            Some((_, row)) if row.is_expandable() => {
                if !row.is_expanded() {
                    row.set_expanded(true);
                }
            }
            _ => self.activate_selected(),
        }
    }

    /// `h`: collapse, or jump to the parent (the header at depth 0).
    pub fn collapse(&self) {
        let Some((s, row)) = self.cursor_row() else {
            return;
        };
        if row.is_expandable() && row.is_expanded() {
            row.set_expanded(false);
        } else if let Some(parent) = row.parent() {
            self.syncing.set(true);
            s.selection.set_selected(parent.position());
            self.syncing.set(false);
            self.set_cursor(self.cursor.get().unwrap());
        } else if let Some(i) = self.index_of(s.id) {
            self.set_cursor(Cursor {
                section: i,
                header: true,
            });
        }
    }

    pub fn expand_all(&self) {
        for s in self.sects.borrow().iter() {
            if s.is_expanded() {
                s.expand_all();
            }
        }
    }

    pub fn collapse_all(&self) {
        for s in self.sects.borrow().iter() {
            if s.is_expanded() {
                s.collapse_all();
            }
        }
    }

    /// `:db name`: activate the connection, expand and select that database.
    pub fn select_db(&self, conn: ConnectionId, db: &str) -> bool {
        self.set_active(conn);
        let Some(i) = self.index_of(conn) else {
            return false;
        };
        let s = self.sect(i);
        for pos in 0..s.tree.n_items() {
            let Some(row) = s.tree.row(pos) else { continue };
            if let Some(Node::Db { name, .. }) = row.item().as_ref().and_then(node_of)
                && name == db
            {
                row.set_expanded(true);
                // Position in the (possibly filtered) selection model.
                for p in 0..s.selection.n_items() {
                    if s.selection.item(p).as_ref() == Some(row.upcast_ref()) {
                        self.go_to_entry((
                            Cursor {
                                section: i,
                                header: false,
                            },
                            p,
                        ));
                        return true;
                    }
                }
                return true;
            }
        }
        false
    }

    /// Names of a connection's loaded databases.
    pub fn databases(&self, conn: ConnectionId) -> Vec<String> {
        let Some(s) = self.section_of(conn) else {
            return vec![];
        };
        (0..s.roots.n_items())
            .filter_map(|i| s.roots.item(i).as_ref().and_then(node_of))
            .filter_map(|n| match n {
                Node::Db { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    /// `(db, coll)` of every loaded collection of a connection.
    pub fn collections(&self, conn: ConnectionId) -> Vec<(String, String)> {
        let prefix = format!("d:{conn}:");
        let stores: Vec<gio::ListStore> = self
            .children
            .borrow()
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .collect();
        let mut out = Vec::new();
        for store in stores {
            for i in 0..store.n_items() {
                if let Some(Node::Coll { db, name, .. }) = store.item(i).as_ref().and_then(node_of)
                {
                    out.push((db, name));
                }
            }
        }
        out.sort();
        out
    }

    fn apply_filter(&self, text: &str) {
        let text = text.trim().to_lowercase();
        self.relayout();
        let sects = self.sects.borrow().clone();
        if text.is_empty() {
            for s in &sects {
                s.filter_model.set_filter(None::<&gtk::CustomFilter>);
            }
            return;
        }
        self.expand_all();
        for s in &sects {
            let needle = text.clone();
            let filter = gtk::CustomFilter::new(move |obj| {
                let Some(row) = obj.downcast_ref::<gtk::TreeListRow>() else {
                    return true;
                };
                let Some(item) = row.item().and_downcast::<BoxedAnyObject>() else {
                    return true;
                };
                let node = item.borrow::<Node>();
                match &*node {
                    Node::Conn(_) => true,
                    Node::Db { name, .. } => {
                        // Keep a database if it matches or any loaded child matches.
                        name.to_lowercase().contains(&needle)
                            || row.children().is_some_and(|c| {
                                (0..c.n_items()).any(|i| {
                                    c.item(i)
                                        .and_downcast::<BoxedAnyObject>()
                                        .is_some_and(|o| matches!(&*o.borrow::<Node>(), Node::Coll { name, .. } if name.to_lowercase().contains(&needle)))
                                })
                            })
                    }
                    Node::Coll { name, db, .. } => {
                        name.to_lowercase().contains(&needle) || db.to_lowercase().contains(&needle)
                    }
                }
            });
            s.filter_model.set_filter(Some(&filter));
        }
    }

    fn context_menu(self: &Rc<Self>, anchor: &gtk::Widget, x: f64, y: f64) {
        let Some(node) = self.selected_node() else {
            return;
        };
        let Some(app) = self.app() else { return };
        let menu = gio::Menu::new();
        match &node {
            Node::Conn(id) => {
                if app.conn(*id).is_some() {
                    menu.append(Some("New database…"), Some("sidebar.new-db"));
                    menu.append(Some("Refresh"), Some("sidebar.refresh"));
                    menu.append(Some("Disconnect"), Some("sidebar.disconnect"));
                } else {
                    menu.append(Some("Connect"), Some("sidebar.connect"));
                }
                menu.append(Some("Edit…"), Some("sidebar.edit"));
                menu.append(Some("Copy connection string"), Some("sidebar.copy-uri"));
                menu.append(Some("Remove"), Some("sidebar.remove"));
            }
            Node::Db { .. } => {
                menu.append(Some("New collection…"), Some("sidebar.new-coll"));
                menu.append(Some("New view…"), Some("sidebar.new-view"));
                menu.append(Some("Refresh"), Some("sidebar.refresh"));
                menu.append(Some("Drop database…"), Some("sidebar.drop"));
            }
            Node::Coll { .. } => {
                menu.append(Some("Open"), Some("sidebar.open"));
                menu.append(Some("Open in new tab"), Some("sidebar.open-new-tab"));
                menu.append(Some("Indexes"), Some("sidebar.indexes"));
                menu.append(Some("Copy namespace"), Some("sidebar.copy-ns"));
                menu.append(Some("Rename…"), Some("sidebar.rename"));
                menu.append(Some("Drop collection…"), Some("sidebar.drop"));
            }
        }
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: Box<dyn Fn()>| {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| f());
            group.add_action(&a);
        };
        {
            let app = app.clone();
            let n = node.clone();
            add("connect", Box::new(move || app.connect_profile(n.conn())));
        }
        {
            let app = app.clone();
            let n = node.clone();
            add("disconnect", Box::new(move || app.disconnect(n.conn())));
        }
        {
            let app = app.clone();
            let n = node.clone();
            add(
                "edit",
                Box::new(move || crate::ui::connections::show_editor(&app, Some(n.conn()))),
            );
        }
        {
            let app = app.clone();
            let n = node.clone();
            add(
                "remove",
                Box::new(move || crate::ui::connections::remove_profile(&app, n.conn())),
            );
        }
        {
            let app = app.clone();
            let n = node.clone();
            add(
                "copy-uri",
                Box::new(move || {
                    if let Some(p) = app.config.borrow().profile(n.conn()) {
                        crate::ui::copy_text(&p.uri);
                    }
                    app.toast("Connection string copied (without password)");
                }),
            );
        }
        {
            let sb = self.clone();
            add("refresh", Box::new(move || sb.refresh_selected()));
        }
        {
            let sb = self.clone();
            add("open", Box::new(move || sb.activate_selected()));
        }
        for (name, action) in [
            ("open-new-tab", "sidebar.open-new-tab"),
            ("indexes", "sidebar.indexes"),
            ("new-coll", "sidebar.add-collection"),
            ("new-db", "sidebar.add-collection"),
            ("drop", "sidebar.delete"),
            ("rename", "sidebar.rename"),
        ] {
            let app = app.clone();
            add(
                name,
                Box::new(move || {
                    app.run_action(action);
                }),
            );
        }
        {
            let app = app.clone();
            let n = node.clone();
            add(
                "new-view",
                Box::new(move || {
                    if let Node::Db { conn, name } = &n {
                        crate::ui::manage::create_collection(
                            &app,
                            *conn,
                            Some(name.clone()),
                            Some("View"),
                        );
                    }
                }),
            );
        }
        {
            let app = app.clone();
            let n = node.clone();
            add(
                "copy-ns",
                Box::new(move || {
                    if let Node::Coll { db, name, .. } = &n {
                        crate::ui::copy_text(&format!("{db}.{name}"));
                        app.toast("Namespace copied");
                    }
                }),
            );
        }
        anchor.insert_action_group("sidebar", Some(&group));
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(anchor);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            let p = p.clone();
            crate::ui::idle(move || p.unparent());
        });
        popover.popup();
    }

    /// `r`: reload the selected connection's databases or database's collections.
    pub fn refresh_selected(&self) {
        let Some(app) = self.app() else { return };
        match self.selected_node() {
            Some(Node::Conn(id)) => app.load_databases(id),
            Some(Node::Db { conn, name }) => app.load_collections(conn, &name),
            Some(Node::Coll { conn, db, .. }) => app.load_collections(conn, &db),
            None => {}
        }
    }
}

pub fn node_of(obj: &glib::Object) -> Option<Node> {
    obj.downcast_ref::<BoxedAnyObject>()
        .map(|o| o.borrow::<Node>().clone())
}
