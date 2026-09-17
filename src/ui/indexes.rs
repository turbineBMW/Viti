//! The Indexes page of a collection tab: every index with its keys, type,
//! size, usage (`$indexStats`) and properties; create with all key types and
//! options, hide/unhide, drop with a typed confirmation. Vi keys: `j`/`k`,
//! `A` add, `Ctrl+d` drop, `H` hide/unhide, `o`/Enter details, `r` refresh.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, IndexInfo, Namespace};
use adw::prelude::*;
use bson::{Bson, Document, doc};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// (label, key value)
const KEY_TYPES: &[(&str, &str)] = &[
    ("1 (ascending)", "1"),
    ("-1 (descending)", "-1"),
    ("text", "text"),
    ("2dsphere", "2dsphere"),
    ("2d", "2d"),
    ("hashed", "hashed"),
    ("wildcard ($**)", "wildcard"),
];

pub struct IndexesPane {
    pub root: gtk::Box,
    view: gtk::ColumnView,
    model: gio::ListStore,
    selection: gtk::SingleSelection,
    title: gtk::Label,
    spinner: gtk::Spinner,
    conn: ConnectionId,
    ns: Namespace,
    app: Weak<App>,
    indexes: RefCell<Vec<IndexInfo>>,
    loaded: Cell<bool>,
    me: RefCell<Weak<IndexesPane>>,
}

fn label_column<F>(view: &gtk::ColumnView, title: &str, expand: bool, f: F) -> gtk::ColumnViewColumn
where
    F: Fn(&IndexInfo, &gtk::Label) + 'static,
{
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        item.set_child(Some(&label));
    });
    let f = Rc::new(f);
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
            return;
        };
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let ix = obj.borrow::<IndexInfo>();
        f(&ix, &label);
    });
    let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    col.set_expand(expand);
    col.set_resizable(true);
    view.append_column(&col);
    col
}

/// `field ↑ · other ↓ · loc 2dsphere`. A text index lists the fields it
/// weights instead of its internal `_fts`/`_ftsx` keys.
pub fn keys_text(keys: &Document, options: &Document) -> String {
    if keys.get_str("_fts").ok() == Some("text") {
        let mut parts: Vec<String> = options
            .get_document("weights")
            .map(|w| w.keys().map(|k| format!("{k} text")).collect())
            .unwrap_or_default();
        parts.extend(
            keys.iter()
                .filter(|(k, _)| !k.starts_with("_fts"))
                .map(|(k, v)| match v {
                    Bson::Int32(-1) | Bson::Int64(-1) => format!("{k} ↓"),
                    _ => format!("{k} ↑"),
                }),
        );
        return parts.join("  ·  ");
    }
    keys.iter()
        .map(|(k, v)| match v {
            Bson::Int32(1) | Bson::Int64(1) => format!("{k} ↑"),
            Bson::Int32(-1) | Bson::Int64(-1) => format!("{k} ↓"),
            Bson::Double(d) if *d == 1.0 => format!("{k} ↑"),
            Bson::Double(d) if *d == -1.0 => format!("{k} ↓"),
            Bson::String(s) => format!("{k} {s}"),
            other => format!("{k} {}", ejson::summary(other, 12)),
        })
        .collect::<Vec<_>>()
        .join("  ·  ")
}

impl IndexesPane {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let model = gio::ListStore::new::<BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(false);
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("viti-table");
        view.add_css_class("data-table");
        view.set_can_focus(false);
        label_column(&view, "Name", true, |ix, l| {
            l.set_text(&ix.name);
            l.add_css_class("viti-mono");
            if ix.is_hidden() {
                l.add_css_class("dim-label");
            } else {
                l.remove_css_class("dim-label");
            }
        });
        label_column(&view, "Keys", true, |ix, l| {
            l.set_text(&keys_text(&ix.keys, &ix.options));
            l.add_css_class("viti-mono");
        });
        label_column(&view, "Type", false, |ix, l| l.set_text(ix.kind()));
        label_column(&view, "Size", false, |ix, l| {
            l.set_text(
                &ix.size
                    .map(crate::ui::human_bytes)
                    .unwrap_or_else(|| "—".into()),
            )
        });
        label_column(&view, "Usage", false, |ix, l| match ix.usage_ops {
            Some(n) => {
                l.set_text(&crate::ui::thousands(n.max(0) as u64));
                if let Some(since) = ix.usage_since {
                    l.set_tooltip_text(Some(&format!(
                        "{n} operations since {}",
                        since
                            .to_chrono()
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                    )));
                }
            }
            None => l.set_text("—"),
        });
        label_column(&view, "Properties", true, |ix, l| {
            l.set_text(&ix.properties().join(", "))
        });

        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh (r)")
            .focus_on_click(false)
            .build();
        let add = gtk::Button::builder()
            .label("Create index")
            .tooltip_text("Create index (A)")
            .focus_on_click(false)
            .css_classes(["suggested-action"])
            .build();
        let hide = gtk::Button::builder()
            .icon_name("view-conceal-symbolic")
            .tooltip_text("Hide / unhide (H)")
            .focus_on_click(false)
            .build();
        let drop = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Drop index (Ctrl+D)")
            .focus_on_click(false)
            .css_classes(["destructive-action"])
            .build();
        let ai_btn = gtk::Button::builder()
            .label("Suggest with AI")
            .tooltip_text("Ask the AI backend which indexes would help (Ctrl+I)")
            .focus_on_click(false)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&title);
        bar.append(&spinner);
        bar.append(&refresh);
        bar.append(&hide);
        bar.append(&drop);
        bar.append(&ai_btn);
        bar.append(&add);

        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-indexes");
        root.append(&bar);
        root.append(&scroller);

        let pane = Rc::new(Self {
            root,
            view: view.clone(),
            model,
            selection: selection.clone(),
            title,
            spinner,
            conn,
            ns,
            app: Rc::downgrade(app),
            indexes: RefCell::new(Vec::new()),
            loaded: Cell::new(false),
            me: RefCell::new(Weak::new()),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);
        {
            let p = pane.clone();
            refresh.connect_clicked(move |_| p.load());
        }
        {
            let p = pane.clone();
            add.connect_clicked(move |_| p.add());
        }
        {
            let p = pane.clone();
            ai_btn.connect_clicked(move |_| {
                if let Some(app) = p.app() {
                    crate::ui::ai::ask(
                        &app,
                        Some(crate::ai::Task::IndexSuggest),
                        Some(String::new()),
                    );
                }
            });
        }
        {
            let p = pane.clone();
            hide.connect_clicked(move |_| p.toggle_hidden());
        }
        {
            let p = pane.clone();
            drop.connect_clicked(move |_| p.drop_selected());
        }
        {
            let p = pane.clone();
            view.connect_activate(move |_, _| p.peek());
        }
        {
            // Clicking a row focuses the pane so vi keys apply.
            let root = pane.root.clone();
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |_, _, _, _| {
                root.grab_focus();
            });
            view.add_controller(click);
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    fn me(&self) -> Option<Rc<Self>> {
        self.me.borrow().upgrade()
    }

    /// Load once, when the page is first shown.
    pub fn ensure_loaded(&self) {
        if !self.loaded.replace(true) {
            self.load();
        }
    }

    pub fn load(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        self.spinner.set_visible(true);
        self.spinner.set_spinning(true);
        glib::spawn_future_local(async move {
            let ns2 = ns.clone();
            let r = crate::rt::io(async move { ops::list_indexes(&client, &ns2).await }).await;
            me.spinner.set_visible(false);
            me.spinner.set_spinning(false);
            match r {
                Ok(list) => me.set_indexes(list),
                Err(e) => {
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("indexes of {ns}"), &e);
                    }
                }
            }
        });
    }

    fn set_indexes(&self, list: Vec<IndexInfo>) {
        let keep = self.cursor().unwrap_or(0);
        let total: u64 = list.iter().filter_map(|i| i.size).sum();
        self.title.set_text(&format!(
            "{} index{}{}",
            list.len(),
            if list.len() == 1 { "" } else { "es" },
            if total > 0 {
                format!("  ·  {}", crate::ui::human_bytes(total))
            } else {
                String::new()
            }
        ));
        *self.indexes.borrow_mut() = list.clone();
        self.model.remove_all();
        for ix in list {
            self.model.append(&BoxedAnyObject::new(ix));
        }
        if self.model.n_items() > 0 {
            self.set_cursor(keep);
        }
    }

    pub fn cursor(&self) -> Option<usize> {
        let s = self.selection.selected();
        (s != gtk::INVALID_LIST_POSITION).then_some(s as usize)
    }

    fn set_cursor(&self, i: usize) {
        let n = self.model.n_items() as usize;
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        self.selection.set_selected(i as u32);
        self.view
            .scroll_to(i as u32, None, gtk::ListScrollFlags::NONE, None);
    }

    pub fn move_cursor(&self, delta: i64) {
        let n = self.model.n_items() as i64;
        if n == 0 {
            return;
        }
        let cur = self.cursor().map(|c| c as i64).unwrap_or(-1);
        let next = if cur < 0 {
            if delta > 0 { 0 } else { n - 1 }
        } else {
            (cur + delta).clamp(0, n - 1)
        };
        self.set_cursor(next as usize);
    }

    pub fn top(&self) {
        self.set_cursor(0);
    }

    pub fn bottom(&self) {
        let n = self.model.n_items() as usize;
        if n > 0 {
            self.set_cursor(n - 1);
        }
    }

    fn selected(&self) -> Option<IndexInfo> {
        let i = self.cursor()?;
        self.indexes.borrow().get(i).cloned()
    }

    /// `o` / Enter: the full index spec as JSON.
    pub fn peek(&self) {
        let Some(ix) = self.selected() else { return };
        let Some(app) = self.app() else { return };
        let mut d = doc! { "name": ix.name.clone(), "key": ix.keys.clone() };
        for (k, v) in &ix.options {
            d.insert(k, v.clone());
        }
        if let Some(n) = ix.usage_ops {
            d.insert("usage", doc! { "ops": n, "since": ix.usage_since.map(Bson::DateTime).unwrap_or(Bson::Null) });
        }
        if let Some(s) = ix.size {
            d.insert("sizeBytes", s as i64);
        }
        let dialog = adw::Dialog::builder()
            .title(&ix.name)
            .content_width(640)
            .content_height(480)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let view = crate::ui::json_view(&ejson::pretty(&d, Mode::Relaxed), false);
        toolbar.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .build(),
        ));
        dialog.set_child(Some(&toolbar));
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let d2 = dialog.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            if matches!(key, Key::Escape | Key::q | Key::o) {
                d2.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(keys);
        dialog.present(Some(&app.window));
    }

    /// `H`: hide or unhide the selected index (`_id_` cannot be hidden).
    pub fn toggle_hidden(&self) {
        let Some(ix) = self.selected() else { return };
        let Some(app) = self.app() else { return };
        if app.write_guard().is_err() {
            return;
        }
        if ix.name == "_id_" {
            app.toast("The _id index cannot be hidden");
            return;
        }
        let Some(me) = self.me() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let hidden = !ix.is_hidden();
        let name = ix.name.clone();
        glib::spawn_future_local(async move {
            let (ns2, name2) = (ns.clone(), name.clone());
            match crate::rt::io(async move {
                ops::set_index_hidden(&client, &ns2, &name2, hidden).await
            })
            .await
            {
                Ok(()) => {
                    app.toast(&format!("{} {name}", if hidden { "Hid" } else { "Unhid" }));
                    me.load();
                }
                Err(e) => app.toast_error(&format!("hide index {name} on {ns}"), &e),
            }
        });
    }

    /// `Ctrl+d`: drop after typing the index name.
    pub fn drop_selected(&self) {
        let Some(ix) = self.selected() else { return };
        let Some(app) = self.app() else { return };
        if app.write_guard().is_err() {
            return;
        }
        if ix.name == "_id_" {
            app.toast("The _id index cannot be dropped");
            return;
        }
        let Some(me) = self.me() else { return };
        let name = ix.name.clone();
        let name2 = name.clone();
        crate::ui::confirm_typed(
            &app.window.clone(),
            "Drop index?",
            &format!(
                "Queries relying on {name} on {} will slow down. Type the index name to confirm.",
                self.ns
            ),
            &name,
            "Drop index",
            move || {
                let Some(app) = me.app() else { return };
                let Some(conn) = app.conn(me.conn) else {
                    return;
                };
                let client = conn.client.clone();
                let ns = me.ns.clone();
                let name = name2.clone();
                let me = me.clone();
                glib::spawn_future_local(async move {
                    let (ns2, n2) = (ns.clone(), name.clone());
                    match crate::rt::io(async move { ops::drop_index(&client, &ns2, &n2).await })
                        .await
                    {
                        Ok(()) => {
                            app.toast(&format!("Dropped index {name}"));
                            me.load();
                        }
                        Err(e) => app.toast_error(&format!("drop index {name} on {ns}"), &e),
                    }
                });
            },
        );
    }

    /// `A`: the create-index dialog.
    pub fn add(&self) {
        self.add_prefilled(None);
    }

    /// The create dialog, optionally filled in (keys, options) — e.g. from an
    /// AI suggestion.
    pub fn add_prefilled(&self, initial: Option<(Document, Document)>) {
        let Some(app) = self.app() else { return };
        let Some(me) = self.me() else { return };
        let dialog = adw::Dialog::builder()
            .title(format!("Create index on {}", self.ns))
            .content_width(640)
            .content_height(700)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let create = gtk::Button::builder()
            .label("Create")
            .css_classes(["suggested-action"])
            .build();
        let cancel = gtk::Button::with_label("Cancel");
        header.pack_start(&cancel);
        header.pack_end(&create);
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();
        let fields = adw::PreferencesGroup::builder()
            .title("Fields")
            .description("Order matters for compound indexes. Wildcard indexes take a field prefix or nothing (all fields).")
            .build();
        let add_field = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add field")
            .css_classes(["flat"])
            .build();
        fields.set_header_suffix(Some(&add_field));
        let rows: Rc<RefCell<Vec<(adw::EntryRow, gtk::DropDown)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let add_row = {
            let fields = fields.clone();
            let rows = rows.clone();
            Rc::new(move |name: &str, kind: u32| {
                let row = adw::EntryRow::builder().title("Field").text(name).build();
                row.add_css_class("viti-mono");
                let dd = gtk::DropDown::from_strings(
                    &KEY_TYPES.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
                );
                dd.set_selected(kind);
                dd.set_valign(gtk::Align::Center);
                let remove = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text("Remove field")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                row.add_suffix(&dd);
                row.add_suffix(&remove);
                fields.add(&row);
                {
                    let fields = fields.clone();
                    let rows = rows.clone();
                    let row2 = row.clone();
                    remove.connect_clicked(move |_| {
                        if rows.borrow().len() <= 1 {
                            return;
                        }
                        rows.borrow_mut().retain(|(r, _)| *r != row2);
                        fields.remove(&row2);
                    });
                }
                rows.borrow_mut().push((row, dd));
            })
        };
        match initial.as_ref().map(|(k, _)| k).filter(|k| !k.is_empty()) {
            Some(keys) => {
                for (field, value) in keys {
                    let (name, kind) = match value {
                        Bson::Int32(-1) | Bson::Int64(-1) => (field.clone(), 1),
                        Bson::Double(v) if *v < 0.0 => (field.clone(), 1),
                        Bson::String(s) => (
                            field.clone(),
                            KEY_TYPES.iter().position(|(_, k)| k == s).unwrap_or(0) as u32,
                        ),
                        _ if field.ends_with("$**") => (
                            field
                                .trim_end_matches("$**")
                                .trim_end_matches('.')
                                .to_string(),
                            6,
                        ),
                        _ => (field.clone(), 0),
                    };
                    add_row(&name, kind);
                }
            }
            None => add_row("", 0),
        }
        {
            let add_row = add_row.clone();
            add_field.connect_clicked(move |_| add_row("", 0));
        }
        page.add(&fields);

        let opts = adw::PreferencesGroup::builder().title("Options").build();
        let name = adw::EntryRow::builder()
            .title("Index name (optional)")
            .build();
        name.add_css_class("viti-mono");
        let unique = adw::SwitchRow::builder().title("Unique").build();
        let sparse = adw::SwitchRow::builder()
            .title("Sparse")
            .subtitle("Only index documents that have the field")
            .build();
        let hidden = adw::SwitchRow::builder()
            .title("Hidden")
            .subtitle("Built but not used by the planner")
            .build();
        let ttl = adw::SpinRow::with_range(0.0, 1e12, 60.0);
        ttl.set_title("TTL: expire documents after (seconds, 0 = off)");
        let partial = adw::EntryRow::builder()
            .title("Partial filter expression (optional)")
            .build();
        partial.add_css_class("viti-mono");
        let collation = adw::EntryRow::builder()
            .title("Collation (optional, e.g. { locale: 'en', strength: 2 })")
            .build();
        collation.add_css_class("viti-mono");
        let projection = adw::EntryRow::builder()
            .title("Wildcard projection (optional)")
            .build();
        projection.add_css_class("viti-mono");
        for r in [&name, &partial, &collation, &projection] {
            opts.add(r);
        }
        opts.add(&unique);
        opts.add(&sparse);
        opts.add(&hidden);
        opts.add(&ttl);
        page.add(&opts);
        if let Some((_, o)) = &initial {
            if let Ok(n) = o.get_str("name") {
                name.set_text(n);
            }
            unique.set_active(o.get_bool("unique").unwrap_or(false));
            sparse.set_active(o.get_bool("sparse").unwrap_or(false));
            hidden.set_active(o.get_bool("hidden").unwrap_or(false));
            if let Some(t) = o
                .get("expireAfterSeconds")
                .and_then(crate::mongo::perf::as_f64)
            {
                ttl.set_value(t.max(0.0));
            }
            for (key, row) in [
                ("partialFilterExpression", &partial),
                ("collation", &collation),
                ("wildcardProjection", &projection),
            ] {
                if let Ok(d) = o.get_document(key) {
                    row.set_text(&ejson::compact(d, ejson::Mode::Relaxed));
                }
            }
        }

        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(
            &gtk::ScrolledWindow::builder()
                .child(&page)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        body.append(&error);
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));

        let commit: Rc<dyn Fn()> = {
            let app = app.clone();
            let dialog = dialog.clone();
            let rows = rows.clone();
            let error = error.clone();
            let (name, unique, sparse, hidden, ttl) = (
                name.clone(),
                unique.clone(),
                sparse.clone(),
                hidden.clone(),
                ttl.clone(),
            );
            let (partial, collation, projection) =
                (partial.clone(), collation.clone(), projection.clone());
            Rc::new(move || {
                let fail = |m: &str| {
                    error.set_text(m);
                    error.set_visible(true);
                };
                let mut keys = Document::new();
                for (row, dd) in rows.borrow().iter() {
                    let field = row.text().trim().to_string();
                    let (_, kind) = KEY_TYPES[dd.selected() as usize];
                    let (field, value) = match kind {
                        "wildcard" => (
                            if field.is_empty() {
                                "$**".to_string()
                            } else {
                                format!("{}.$**", field.trim_end_matches(".$**"))
                            },
                            Bson::Int32(1),
                        ),
                        "1" => (field, Bson::Int32(1)),
                        "-1" => (field, Bson::Int32(-1)),
                        other => (field, Bson::String(other.to_string())),
                    };
                    if field.is_empty() {
                        return fail("Every field needs a name");
                    }
                    keys.insert(field, value);
                }
                if keys.is_empty() {
                    return fail("Add at least one field");
                }
                let mut options = Document::new();
                let n = name.text().trim().to_string();
                if !n.is_empty() {
                    options.insert("name", n);
                }
                if unique.is_active() {
                    options.insert("unique", true);
                }
                if sparse.is_active() {
                    options.insert("sparse", true);
                }
                if hidden.is_active() {
                    options.insert("hidden", true);
                }
                let t = ttl.value() as i64;
                if t > 0 {
                    options.insert("expireAfterSeconds", t);
                }
                for (key, row) in [
                    ("partialFilterExpression", &partial),
                    ("collation", &collation),
                    ("wildcardProjection", &projection),
                ] {
                    let text = row.text();
                    if text.trim().is_empty() {
                        continue;
                    }
                    match ejson::parse_document(&text) {
                        Ok(d) => options.insert(key, d),
                        Err(e) => return fail(&format!("{key}: {e}")),
                    };
                }
                if app.write_guard().is_err() {
                    return;
                }
                let Some(conn) = app.conn(me.conn) else {
                    return fail("Not connected");
                };
                let client = conn.client.clone();
                let ns = me.ns.clone();
                let app = app.clone();
                let me = me.clone();
                dialog.close();
                app.banner.set_title(&format!("Building index on {ns}…"));
                app.banner.set_revealed(true);
                glib::spawn_future_local(async move {
                    let ns2 = ns.clone();
                    let r = crate::rt::io(async move {
                        ops::create_index(&client, &ns2, &keys, &options).await
                    })
                    .await;
                    app.banner.set_revealed(false);
                    match r {
                        Ok(n) => {
                            let msg = format!("Created index {n} on {ns}");
                            app.toast(&msg);
                            app.notify_if_unfocused("index", "Index built", &msg);
                            me.load();
                        }
                        Err(e) => {
                            app.toast_error(&format!("create index on {ns}"), &e);
                            app.notify_if_unfocused(
                                "index",
                                "Index build failed",
                                &format!("{ns}: {e:#}"),
                            );
                        }
                    }
                });
            })
        };
        {
            let commit = commit.clone();
            create.connect_clicked(move |_| commit());
        }
        {
            let dialog = dialog.clone();
            cancel.connect_clicked(move |_| {
                dialog.close();
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let dialog = dialog.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                if key == gtk::gdk::Key::Escape {
                    dialog.close();
                    return glib::Propagation::Stop;
                }
                if ctrl && (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter) {
                    commit();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        dialog.add_controller(keys);
        dialog.present(Some(&app.window));
        if let Some((row, _)) = rows.borrow().first() {
            row.grab_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_render() {
        assert_eq!(
            keys_text(&doc! { "a": 1, "b": -1, "t": "text" }, &doc! {}),
            "a ↑  ·  b ↓  ·  t text"
        );
        assert_eq!(
            keys_text(
                &doc! { "_fts": "text", "_ftsx": 1 },
                &doc! { "weights": { "name": 1, "bio": 2 } }
            ),
            "name text  ·  bio text"
        );
    }
}
