//! The Documents tab: one toolbar row (view switcher, query bar, pager, menu)
//! and three views over one page of documents. All state lives here; the views
//! in `list`/`json`/`table` are factories over the shared `model`/`selection`.
pub mod json;
pub mod list;
mod pagination;
mod preview;
pub mod table;

use crate::app::App;
use crate::config::{DocView, Query};
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, FindSpec, Namespace, OpCtx};
use crate::ui::editor_pane::JobKind;
use crate::ui::query_bar::QueryBar;
use adw::prelude::*;
use bson::{Bson, Document};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::rc::{Rc, Weak};

pub struct DocumentsPane {
    pub root: gtk::Box,
    /// The views container: the widget that owns `Scope::Documents`.
    pub views: gtk::Stack,
    pub query_bar: Rc<QueryBar>,
    pub model: gio::ListStore,
    pub selection: gtk::SingleSelection,
    list_view: gtk::ListView,
    json_view: gtk::ListView,
    table_view: gtk::ColumnView,
    status: gtk::Label,
    page_label: gtk::Label,
    view_dropdown: gtk::DropDown,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    goto_btn: gtk::Button,
    page_picker: RefCell<Option<pagination::PagePicker>>,
    /// Stateful menu actions kept in sync with the keyboard equivalents.
    expand_action: gio::SimpleAction,

    pub conn: ConnectionId,
    pub ns: Namespace,
    app: Weak<App>,
    pub docs: RefCell<Vec<Document>>,
    /// Every field path seen on any page so far, for the query bar's completions.
    fields: RefCell<Vec<crate::query_complete::Field>>,
    pub marked: RefCell<BTreeSet<usize>>,
    pub view: Cell<DocView>,
    pub page: Cell<u64>,
    pub page_size: Cell<u64>,
    pub total: Cell<Option<u64>>,
    query: RefCell<Query>,
    spec: RefCell<FindSpec>,
    pub expanded_all: Cell<bool>,
    pub hidden_columns: RefCell<HashSet<String>>,
    pub columns: RefCell<Vec<String>>,
    /// Current column in the table view (h/l move it; S sorts by it, H hides it).
    pub column_cursor: Cell<usize>,
    inflight: RefCell<Option<(OpCtx, tokio::task::AbortHandle)>>,
    generation: Cell<u64>,
    count_generation: Cell<u64>,
    count_inflight: RefCell<Option<tokio::task::AbortHandle>>,
    busy: Cell<bool>,
    has_more: Cell<bool>,
    loaded_page: Cell<u64>,
    /// Weak self for the view factories (they outlive any borrow of `pane`).
    pub(super) me: RefCell<Weak<DocumentsPane>>,
}

impl DocumentsPane {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let settings = app.config.borrow().settings.clone();
        let query_bar = QueryBar::new();
        let model = gio::ListStore::new::<BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(false);

        // Everything lives on the query bar's single row: the view switcher
        // leads, the pager and the overflow menu trail.
        let tip = |base: &str, id: &str| {
            let accel = crate::keybinds::accel_for(&settings, id);
            if accel.is_empty() {
                base.to_string()
            } else {
                format!("{base} ({})", crate::keybinds::pretty_accel(&accel))
            }
        };
        let view_dropdown = gtk::DropDown::from_strings(&["List", "JSON", "Table"]);
        view_dropdown.set_tooltip_text(Some(&tip("View", "docs.cycle-view")));
        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(tip("Previous page", "docs.prev-page"))
            .focus_on_click(false)
            .build();
        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text(tip("Next page", "docs.next-page"))
            .focus_on_click(false)
            .build();
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let goto_btn = gtk::Button::builder()
            .label("Go to")
            .tooltip_text(tip("Go to page", "docs.goto-page"))
            .focus_on_click(false)
            .build();
        pager.add_css_class("linked");
        pager.append(&prev_btn);
        pager.append(&goto_btn);
        pager.append(&next_btn);
        let page_label = gtk::Label::builder().css_classes(["viti-count"]).build();
        let status = gtk::Label::builder()
            .xalign(1.0)
            .max_width_chars(24)
            .css_classes(["viti-count"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();

        let actions = gio::SimpleActionGroup::new();
        let act_insert = gio::SimpleAction::new("insert", None);
        let act_refresh = gio::SimpleAction::new("refresh", None);
        let act_bulk_update = gio::SimpleAction::new("bulk-update", None);
        let act_bulk_delete = gio::SimpleAction::new("bulk-delete", None);
        let act_explain = gio::SimpleAction::new("explain", None);
        let act_export_lang = gio::SimpleAction::new("export-language", None);
        let act_export = gio::SimpleAction::new("export", None);
        let act_import = gio::SimpleAction::new("import", None);
        let act_ai = gio::SimpleAction::new("ai", None);
        let expand_action =
            gio::SimpleAction::new_stateful("expand-all", None, &false.to_variant());
        let act_size = gio::SimpleAction::new_stateful(
            "page-size",
            Some(glib::VariantTy::STRING),
            &settings.page_size.to_string().to_variant(),
        );
        for a in [
            &act_insert,
            &act_refresh,
            &expand_action,
            &act_size,
            &act_bulk_update,
            &act_bulk_delete,
            &act_explain,
            &act_export_lang,
            &act_export,
            &act_import,
            &act_ai,
        ] {
            actions.add_action(a);
        }
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(
            Some(&tip("Insert document", "docs.add")),
            Some("docs.insert"),
        );
        section.append(Some(&tip("Refresh", "docs.refresh")), Some("docs.refresh"));
        section.append(
            Some(&tip("Expand all fields", "docs.expand-all")),
            Some("docs.expand-all"),
        );
        menu.append_section(None, &section);
        let bulk = gio::Menu::new();
        bulk.append(
            Some(&tip("Update matching documents…", "docs.bulk-update")),
            Some("docs.bulk-update"),
        );
        bulk.append(
            Some(&tip("Delete matching documents…", "docs.bulk-delete")),
            Some("docs.bulk-delete"),
        );
        menu.append_section(Some("Bulk"), &bulk);
        let data = gio::Menu::new();
        data.append(
            Some(&tip("Export to JSON / CSV…", "docs.export")),
            Some("docs.export"),
        );
        data.append(
            Some(&tip("Import JSON / CSV…", "docs.import")),
            Some("docs.import"),
        );
        menu.append_section(Some("Data"), &data);
        let tools = gio::Menu::new();
        tools.append(
            Some(&tip("Explain plan", "docs.explain")),
            Some("docs.explain"),
        );
        tools.append(
            Some(&tip("Export query to language…", "docs.export-language")),
            Some("docs.export-language"),
        );
        tools.append(
            Some(&tip("Generate query with AI…", "global.ai")),
            Some("docs.ai"),
        );
        menu.append_section(None, &tools);
        let sizes = gio::Menu::new();
        for n in ["25", "50", "75", "100"] {
            let item = gio::MenuItem::new(Some(n), None);
            item.set_action_and_target_value(Some("docs.page-size"), Some(&n.to_variant()));
            sizes.append_item(&item);
        }
        let size_section = gio::Menu::new();
        size_section.append_submenu(Some("Documents per page"), &sizes);
        menu.append_section(None, &size_section);
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .focus_on_click(false)
            .menu_model(&menu)
            .build();

        let row = &query_bar.row;
        row.prepend(&view_dropdown);
        row.append(&status);
        row.append(&page_label);
        row.append(&pager);
        row.append(&menu_btn);

        let views = gtk::Stack::new();
        views.set_vexpand(true);
        views.add_css_class("viti-docs");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&query_bar.root);
        root.append(&views);
        root.insert_action_group("docs", Some(&actions));

        let pane = Rc::new(Self {
            root,
            views: views.clone(),
            query_bar: query_bar.clone(),
            model,
            selection: selection.clone(),
            list_view: gtk::ListView::new(
                None::<gtk::SingleSelection>,
                None::<gtk::SignalListItemFactory>,
            ),
            json_view: gtk::ListView::new(
                None::<gtk::SingleSelection>,
                None::<gtk::SignalListItemFactory>,
            ),
            table_view: gtk::ColumnView::new(None::<gtk::SingleSelection>),
            status,
            page_label,
            view_dropdown: view_dropdown.clone(),
            prev_btn: prev_btn.clone(),
            next_btn: next_btn.clone(),
            goto_btn: goto_btn.clone(),
            page_picker: RefCell::new(None),
            expand_action: expand_action.clone(),
            conn,
            ns,
            app: Rc::downgrade(app),
            docs: RefCell::new(Vec::new()),
            fields: RefCell::new(Vec::new()),
            marked: RefCell::new(BTreeSet::new()),
            view: Cell::new(settings.default_view),
            page: Cell::new(0),
            page_size: Cell::new(settings.page_size.clamp(1, 100) as u64),
            total: Cell::new(None),
            query: RefCell::new(Query::default()),
            spec: RefCell::new(FindSpec::default()),
            expanded_all: Cell::new(false),
            hidden_columns: RefCell::new(HashSet::new()),
            columns: RefCell::new(Vec::new()),
            column_cursor: Cell::new(0),
            inflight: RefCell::new(None),
            generation: Cell::new(0),
            count_generation: Cell::new(0),
            count_inflight: RefCell::new(None),
            busy: Cell::new(false),
            has_more: Cell::new(false),
            loaded_page: Cell::new(0),
            me: RefCell::new(Weak::new()),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);

        // Views share the selection model.
        list::setup(&pane);
        json::setup(&pane);
        table::setup(&pane);
        let scroll = |w: &gtk::Widget| {
            gtk::ScrolledWindow::builder()
                .child(w)
                .vexpand(true)
                .build()
        };
        views.add_named(&scroll(pane.list_view.upcast_ref()), Some("list"));
        views.add_named(&scroll(pane.json_view.upcast_ref()), Some("json"));
        let table_scroll = gtk::ScrolledWindow::builder()
            .child(&pane.table_view)
            .vexpand(true)
            .build();
        views.add_named(&table_scroll, Some("table"));
        pane.apply_view();

        {
            let p = pane.clone();
            query_bar.set_on_run(move |q| p.run_query(q));
        }
        {
            let p = pane.clone();
            query_bar.set_on_history_pick(move |sq| {
                p.query_bar.set_query(&sq.query);
                p.run_query(sq.query);
            });
        }
        {
            let p = pane.clone();
            query_bar.set_on_history_open(move || p.show_history());
        }
        {
            let p = pane.clone();
            view_dropdown.connect_selected_notify(move |d| {
                let v = match d.selected() {
                    1 => DocView::Json,
                    2 => DocView::Table,
                    _ => DocView::List,
                };
                if p.view.get() != v {
                    p.view.set(v);
                    p.apply_view();
                }
            });
        }
        {
            let p = pane.clone();
            act_size.connect_activate(move |a, param| {
                let Some(n) = param
                    .and_then(|v| v.str())
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    return;
                };
                let n = n.clamp(1, 100);
                a.set_state(&n.to_string().to_variant());
                if p.page_size.get() != n {
                    p.page_size.set(n);
                    if let Some(app) = p.app() {
                        app.config.borrow_mut().settings.page_size = n as u32;
                        app.schedule_save();
                    }
                    p.page.set(0);
                    p.load();
                }
            });
        }
        {
            let p = pane.clone();
            prev_btn.connect_clicked(move |_| p.prev_page());
        }
        {
            let p = pane.clone();
            next_btn.connect_clicked(move |_| p.next_page());
        }
        {
            let p = pane.clone();
            goto_btn.connect_clicked(move |_| p.show_page_picker());
        }
        {
            let p = pane.clone();
            act_insert.connect_activate(move |_, _| p.add());
        }
        {
            let p = pane.clone();
            act_refresh.connect_activate(move |_, _| p.load());
        }
        {
            let p = pane.clone();
            act_bulk_update.connect_activate(move |_, _| p.bulk_update(None));
        }
        {
            let p = pane.clone();
            act_bulk_delete.connect_activate(move |_, _| p.bulk_delete());
        }
        {
            let p = pane.clone();
            act_explain.connect_activate(move |_, _| {
                if let Some(app) = p.app() {
                    app.explain_current("query");
                }
            });
        }
        {
            let p = pane.clone();
            act_export_lang.connect_activate(move |_, _| p.export_language());
        }
        {
            let p = pane.clone();
            let ask = move || {
                if let Some(app) = p.app() {
                    crate::ui::ai::ask(&app, Some(crate::ai::Task::Query), None);
                }
            };
            let ask2 = ask.clone();
            act_ai.connect_activate(move |_, _| ask());
            pane.query_bar.ai_btn.connect_clicked(move |_| ask2());
        }
        {
            let p = pane.clone();
            act_export.connect_activate(move |_, _| p.export());
        }
        {
            let p = pane.clone();
            act_import.connect_activate(move |_, _| p.import());
        }
        {
            let p = pane.clone();
            query_bar.set_on_history_star(move |id| {
                if let Some(app) = p.app() {
                    app.toggle_favourite(id);
                }
            });
        }
        {
            let p = pane.clone();
            query_bar.set_on_history_delete(move |id| {
                if let Some(app) = p.app() {
                    app.config.borrow_mut().queries.retain(|q| q.id != id);
                    app.schedule_save();
                }
            });
        }
        {
            let p = pane.clone();
            expand_action.connect_activate(move |_, _| p.toggle_expand_all());
        }
        {
            let p = pane.clone();
            query_bar.stop.connect_clicked(move |_| p.cancel());
        }
        {
            let p = pane.clone();
            selection.connect_selected_notify(move |s| {
                if s.selected() != gtk::INVALID_LIST_POSITION {
                    p.update_status();
                }
            });
        }
        {
            let p = pane.clone();
            pane.list_view.connect_activate(move |_, _| p.peek(false));
        }
        {
            let p = pane.clone();
            pane.json_view.connect_activate(move |_, _| p.peek(false));
        }
        {
            let p = pane.clone();
            pane.table_view.connect_activate(move |_, _| p.peek(false));
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    // ----- state helpers -------------------------------------------------

    pub fn cursor(&self) -> Option<usize> {
        let s = self.selection.selected();
        if s == gtk::INVALID_LIST_POSITION {
            None
        } else {
            Some(s as usize)
        }
    }

    pub fn current_doc(&self) -> Option<Document> {
        let i = self.cursor()?;
        self.docs.borrow().get(i).cloned()
    }

    /// The documents an action applies to: marked ones, else the cursor.
    pub fn targets(&self) -> Vec<usize> {
        let marked = self.marked.borrow();
        if !marked.is_empty() {
            marked.iter().copied().collect()
        } else {
            self.cursor().into_iter().collect()
        }
    }

    fn set_cursor(&self, i: usize) {
        let n = self.model.n_items() as usize;
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        self.selection.set_selected(i as u32);
        let flags = gtk::ListScrollFlags::NONE;
        match self.view.get() {
            DocView::List => self.list_view.scroll_to(i as u32, flags, None),
            DocView::Json => self.json_view.scroll_to(i as u32, flags, None),
            DocView::Table => self.table_view.scroll_to(i as u32, None, flags, None),
        }
        self.update_status();
    }

    pub fn move_cursor(&self, delta: i64) {
        let cur = self.cursor().map(|c| c as i64).unwrap_or(-1);
        let n = self.model.n_items() as i64;
        if n == 0 {
            return;
        }
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

    /// `h`/`l`: move the column cursor in the table view; no-op elsewhere.
    pub fn move_column(&self, delta: i64) {
        if self.view.get() != DocView::Table {
            return;
        }
        let n = self.visible_columns().len() as i64;
        if n == 0 {
            return;
        }
        let next = (self.column_cursor.get() as i64 + delta).clamp(0, n - 1);
        self.column_cursor.set(next as usize);
        table::highlight_column(self, next as usize);
        self.update_status();
    }

    pub fn visible_columns(&self) -> Vec<String> {
        let hidden = self.hidden_columns.borrow();
        self.columns
            .borrow()
            .iter()
            .filter(|c| !hidden.contains(*c))
            .cloned()
            .collect()
    }

    pub fn current_column(&self) -> Option<String> {
        self.visible_columns()
            .get(self.column_cursor.get())
            .cloned()
    }

    fn apply_view(&self) {
        let v = self.view.get();
        self.detach_views();
        self.attach_view();
        self.views.set_visible_child_name(match v {
            DocView::List => "list",
            DocView::Json => "json",
            DocView::Table => "table",
        });
        self.view_dropdown.set_selected(match v {
            DocView::List => 0,
            DocView::Json => 1,
            DocView::Table => 2,
        });
        if let Some(c) = self.cursor() {
            self.set_cursor(c);
        }
        self.update_status();
    }

    fn detach_views(&self) {
        self.list_view.set_model(None::<&gtk::SingleSelection>);
        self.json_view.set_model(None::<&gtk::SingleSelection>);
        self.table_view.set_model(None::<&gtk::SingleSelection>);
    }

    fn attach_view(&self) {
        match self.view.get() {
            DocView::List => self.list_view.set_model(Some(&self.selection)),
            DocView::Json => self.json_view.set_model(Some(&self.selection)),
            DocView::Table => {
                table::rebuild_columns(self);
                self.table_view.set_model(Some(&self.selection));
            }
        }
    }

    pub fn cycle_view(&self) {
        self.view.set(self.view.get().next());
        self.apply_view();
    }

    pub fn set_view(&self, v: DocView) {
        self.view.set(v);
        self.apply_view();
    }

    fn update_status(&self) {
        let n = self.docs.borrow().len() as u64;
        let start = self.loaded_page.get().saturating_mul(self.page_size.get());
        let range = if n == 0 {
            "0".to_string()
        } else {
            format!(
                "{}–{}",
                crate::ui::thousands(start.saturating_add(1)),
                crate::ui::thousands(start.saturating_add(n))
            )
        };
        let total = match self.total.get() {
            Some(t) => crate::ui::thousands(t),
            None => "many".into(),
        };
        self.page_label.set_text(&format!("{range} of {total}"));
        let marked = self.marked.borrow().len();
        let mut parts: Vec<String> = Vec::new();
        if marked > 0 {
            parts.push(format!("{marked} selected"));
        }
        if self.view.get() == DocView::Table
            && let Some(col) = self.current_column()
        {
            parts.push(col);
        }
        if self.view.get() == DocView::Table && self.columns.borrow().len() == 64 {
            parts.push("64 column preview".into());
        }
        self.status
            .set_tooltip_text(Some("Open a document to see all fields and values"));
        self.status.set_text(&parts.join(" · "));
        self.next_btn
            .set_sensitive(!self.busy.get() && self.has_more.get());
        self.prev_btn
            .set_sensitive(!self.busy.get() && self.page.get() > 0);
        self.goto_btn.set_sensitive(!self.busy.get());
        if let Some(picker) = self.page_picker.borrow().as_ref() {
            picker.update(self);
        }
    }

    // ----- loading --------------------------------------------------------

    pub fn run_query(&self, q: Query) {
        let default_sort = self
            .app()
            .map(|a| a.config.borrow().settings.default_sort.clone())
            .unwrap_or_default();
        match FindSpec::from_query(&q, &default_sort) {
            Ok(spec) => {
                self.query_bar.set_error(None);
                *self.spec.borrow_mut() = spec;
                if let Some(app) = self.app()
                    && !q.is_default()
                {
                    app.config
                        .borrow_mut()
                        .record_query(&self.ns.to_string(), &q, Some(self.conn));
                    app.schedule_save();
                }
                *self.query.borrow_mut() = q;
                self.page.set(0);
                self.marked.borrow_mut().clear();
                self.load();
            }
            Err(e) => {
                self.query_bar.set_error(Some(&e));
                self.query_bar.focus_filter();
            }
        }
    }

    pub fn current_query(&self) -> Query {
        self.query.borrow().clone()
    }

    /// The parsed query as last run (explain, export to language).
    pub fn current_spec(&self) -> FindSpec {
        self.spec.borrow().clone()
    }

    /// `Ctrl+Shift+X`: the query as driver code.
    pub fn export_language(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        let spec = self.current_spec();
        crate::ui::export_lang::show(
            &app,
            self.conn,
            self.ns.clone(),
            crate::export_to_language::Input::Find(Box::new(
                crate::export_to_language::FindInput {
                    filter: spec.filter,
                    projection: spec.projection,
                    sort: spec.sort,
                    collation: spec.collation,
                    skip: spec.skip,
                    limit: spec.limit,
                },
            )),
        );
    }

    /// `X`: the export dialog for this collection / the current query.
    pub fn export(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        crate::ui::export::show(
            &app,
            self.conn,
            self.ns.clone(),
            crate::ui::export::Context::Documents(self.clone()),
            None,
        );
    }

    /// `I`: the import dialog for this collection.
    pub fn import(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        crate::ui::import::show(&app, self.conn, self.ns.clone(), Some(self.clone()), None);
    }

    pub fn cancel(&self) {
        if let Some((ctx, handle)) = self.inflight.borrow_mut().take() {
            handle.abort();
            self.generation.set(self.generation.get() + 1);
            if let Some(app) = self.app() {
                if let Some(conn) = app.conn(self.conn) {
                    let client = conn.client.clone();
                    crate::rt::spawn(async move {
                        match ops::kill_by_comment(&client, &ctx.comment).await {
                            Ok(n) => tracing::info!("killed {n} op(s) for {}", ctx.comment),
                            Err(e) => tracing::warn!("killOp: {e:#}"),
                        }
                    });
                }
                app.toast("Query cancelled");
            }
            self.page.set(self.loaded_page.get());
            self.set_busy(false);
        }
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.query_bar.set_busy(busy);
        self.update_status();
    }

    /// Refresh the query/count after edits or an explicit refresh. Page turns
    /// only reload the page; they do not rescan matches for a fresh count.
    pub fn load(&self) {
        self.refresh_count();
        self.load_page();
    }

    fn refresh_count(&self) {
        self.total.set(None);
        if let Some(handle) = self.count_inflight.borrow_mut().take() {
            handle.abort();
        }
        let generation = self.count_generation.get() + 1;
        self.count_generation.set(generation);
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let spec = self.current_spec();
        let ctx = OpCtx::new(app.max_time_ms());
        let (tx, rx) = async_channel::bounded(1);
        let handle = crate::rt::spawn(async move {
            // The count is advisory. Bound wall time as well as server time.
            let total = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ops::count(&client, &ns, &spec, &ctx),
            )
            .await
            .ok()
            .flatten();
            let _ = tx.send(total).await;
        });
        *self.count_inflight.borrow_mut() = Some(handle.abort_handle());
        let weak = self.me.borrow().clone();
        glib::spawn_future_local(async move {
            let Ok(total) = rx.recv().await else { return };
            let Some(me) = weak.upgrade() else { return };
            if me.count_generation.get() == generation {
                me.count_inflight.borrow_mut().take();
                me.total.set(total);
                me.update_status();
            }
        });
    }

    fn load_page(&self) {
        let Some(me) = self.me.borrow().upgrade() else {
            return;
        };
        let this = me.clone();
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            app.toast("Not connected");
            return;
        };
        if let Some((_, h)) = self.inflight.borrow_mut().take() {
            h.abort();
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ctx = OpCtx::new(app.max_time_ms());
        let spec = self.spec.borrow().clone();
        let ns = self.ns.clone();
        let client = conn.client.clone();
        let page = self.page.get();
        let page_skip = page.saturating_mul(self.page_size.get());
        let page_size = self.page_size.get();
        let (tx, rx) = async_channel::bounded(1);
        let ctx2 = ctx.clone();
        let handle = crate::rt::spawn(async move {
            // One lookahead document determines Next without waiting for a
            // count, including filtered queries and query-bar limits.
            let result = ops::find_page(&client, &ns, &spec, page_skip, page_size + 1, &ctx2)
                .await
                .map(|mut docs| {
                    let has_more = docs.len() as u64 > page_size;
                    if has_more {
                        docs.pop();
                    }
                    let columns = page_columns(&docs);
                    (docs, columns, has_more)
                });
            let _ = tx.send(result).await;
        });
        *self.inflight.borrow_mut() = Some((ctx, handle.abort_handle()));
        self.set_busy(true);
        let me = this;
        glib::spawn_future_local(async move {
            let Ok(result) = rx.recv().await else { return };
            if me.generation.get() != generation {
                crate::rt::rt().spawn_blocking(move || drop(result));
                return;
            }
            me.inflight.borrow_mut().take();
            match result {
                Ok((docs, columns, has_more)) => {
                    me.loaded_page.set(page);
                    me.has_more.set(has_more);
                    me.set_docs(docs, columns);
                }
                Err(e) => {
                    me.page.set(me.loaded_page.get());
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("query on {}", me.ns), &e);
                    }
                }
            }
            me.set_busy(false);
        });
    }

    /// Reload the current page and put the cursor back where it was.
    pub fn reload_keep_cursor(&self) {
        let Some(me) = self.me.borrow().upgrade() else {
            return;
        };
        let cur = self.cursor();
        self.load();
        if let Some(c) = cur {
            // The load replaces the model asynchronously; restore afterwards.
            glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                if me.model.n_items() > 0 {
                    me.set_cursor(c);
                }
            });
        }
    }

    fn set_docs(&self, docs: Vec<Document>, columns: Vec<String>) {
        let keep = self.cursor().unwrap_or(0);
        self.marked.borrow_mut().clear();
        self.detach_views();
        *self.columns.borrow_mut() = columns;
        {
            let mut fields = self.fields.borrow_mut();
            let before = fields.len();
            crate::query_complete::merge_fields(&mut fields, &docs);
            if fields.len() != before {
                self.query_bar.set_fields(Rc::new(fields.clone()));
            }
        }
        let n = docs.len();
        let old = self.docs.replace(docs);
        // Freeing large BSON trees can itself stall GTK.
        crate::rt::rt().spawn_blocking(move || drop(old));
        let items: Vec<BoxedAnyObject> = (0..n).map(BoxedAnyObject::new).collect();
        self.model.splice(0, self.model.n_items(), &items);
        self.attach_view();
        if n > 0 {
            self.set_cursor(keep.min(n - 1));
        }
        self.update_status();
    }

    /// Re-bind every row (marks, expansion) without reloading.
    pub fn refresh_rows(&self) {
        let n = self.model.n_items();
        if n > 0 {
            self.model.items_changed(0, n, n);
            if let Some(c) = self.cursor() {
                self.selection.set_selected(c as u32);
            }
        }
    }

    pub fn next_page(&self) {
        if !self.next_btn.is_sensitive() {
            return;
        }
        self.page.set(self.page.get() + 1);
        self.load_page();
    }

    pub fn prev_page(&self) {
        if self.busy.get() || self.page.get() == 0 {
            return;
        }
        self.page.set(self.page.get() - 1);
        self.load_page();
    }

    pub fn goto_page(&self, page: u64) {
        if self.busy.get() {
            return;
        }
        self.page.set(page.saturating_sub(1));
        self.load_page();
    }

    fn show_history(&self) {
        let Some(app) = self.app() else { return };
        let entries: Vec<_> = app
            .config
            .borrow()
            .history(&self.ns.to_string())
            .into_iter()
            .cloned()
            .collect();
        self.query_bar.show_history(entries);
    }

    pub fn open_history(&self) {
        self.query_bar.open_history();
    }

    // ----- document actions ----------------------------------------------

    pub fn toggle_mark(&self) {
        let Some(c) = self.cursor() else { return };
        {
            let mut m = self.marked.borrow_mut();
            if !m.remove(&c) {
                m.insert(c);
            }
        }
        self.model.items_changed(c as u32, 1, 1);
        self.selection.set_selected(c as u32);
        self.move_cursor(1);
        self.update_status();
    }

    pub fn clear_marks(&self) -> bool {
        let had = !self.marked.borrow().is_empty();
        self.marked.borrow_mut().clear();
        if had {
            self.refresh_rows();
            self.update_status();
        }
        had
    }

    pub fn peek(self: &Rc<Self>, full: bool) {
        let Some(doc) = self.current_doc() else {
            return;
        };
        let Some(app) = self.app() else { return };
        let text = ejson::pretty(&doc, Mode::Relaxed);
        let title = doc
            .get("_id")
            .map(ejson::id_display)
            .unwrap_or_else(|| "document".into());
        let dialog = adw::Dialog::builder().title(title).build();
        if full {
            dialog.set_content_width(app.window.width() - 80);
            dialog.set_content_height(app.window.height() - 80);
        } else {
            dialog.set_content_width(760);
            dialog.set_content_height(620);
        }
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy (C)")
            .build();
        let edit = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit (e)")
            .build();
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete (Ctrl+D)")
            .css_classes(["destructive-action", "flat"])
            .visible(doc.contains_key("_id"))
            .build();
        header.pack_end(&copy);
        header.pack_end(&edit);
        header.pack_start(&delete);
        toolbar.add_top_bar(&header);
        let view = crate::ui::json_view(&text, false);
        view.set_can_focus(true);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();
        toolbar.set_content(Some(&scroller));
        dialog.set_child(Some(&toolbar));
        {
            let text = text.clone();
            let app = app.clone();
            copy.connect_clicked(move |_| {
                crate::ui::copy_text(&text);
                app.toast("Document copied");
            });
        }
        {
            let dialog = dialog.clone();
            let app = app.clone();
            edit.connect_clicked(move |_| {
                dialog.close();
                if let Some(tab) = app.current_tab() {
                    tab.docs.edit();
                }
            });
        }
        // Confirm over the quick view; it closes once the delete is confirmed.
        let ask_delete: Rc<dyn Fn()> = {
            let dialog = dialog.clone();
            let me = self.clone();
            let id = doc.get("_id").cloned();
            Rc::new(move || {
                let Some(id) = id.clone() else { return };
                let Some(app) = me.app() else { return };
                if app.write_guard().is_err() {
                    return;
                }
                let parent = dialog.clone();
                let dialog = dialog.clone();
                let me = me.clone();
                crate::ui::confirm(
                    &parent,
                    "Delete this document?",
                    &format!(
                        "{} from {}. This cannot be undone.",
                        ejson::id_display(&id),
                        me.ns
                    ),
                    "Delete",
                    true,
                    move || {
                        dialog.close();
                        me.delete_ids(vec![id.clone()], false);
                    },
                );
            })
        };
        {
            let ask = ask_delete.clone();
            delete.connect_clicked(move |_| ask());
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let dialog = dialog.clone();
            let text = text.clone();
            let app = app.clone();
            let scroller = scroller.clone();
            let ask_delete = ask_delete.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                use gtk::gdk::Key;
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                match key {
                    Key::Escape | Key::q | Key::o | Key::O => {
                        dialog.close();
                        glib::Propagation::Stop
                    }
                    Key::C => {
                        crate::ui::copy_text(&text);
                        app.toast("Document copied");
                        glib::Propagation::Stop
                    }
                    Key::e if !ctrl => {
                        dialog.close();
                        if let Some(tab) = app.current_tab() {
                            tab.docs.edit();
                        }
                        glib::Propagation::Stop
                    }
                    Key::E => {
                        dialog.close();
                        if let Some(tab) = app.current_tab() {
                            tab.docs.edit_external();
                        }
                        glib::Propagation::Stop
                    }
                    Key::d if ctrl => {
                        ask_delete();
                        glib::Propagation::Stop
                    }
                    Key::j | Key::k | Key::g | Key::G => {
                        let adj = scroller.vadjustment();
                        let step = adj.step_increment().max(40.0);
                        let v = match key {
                            Key::j => adj.value() + step,
                            Key::k => adj.value() - step,
                            Key::g => adj.lower(),
                            _ => adj.upper(),
                        };
                        adj.set_value(v);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        dialog.add_controller(keys);
        dialog.present(Some(&app.window));
    }

    /// Editable dialog with Save/Cancel; `original` None inserts a new document.
    fn edit_dialog(self: &Rc<Self>, text: String, original_id: Option<Bson>, title: &str) {
        let Some(app) = self.app() else { return };
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(760)
            .content_height(620)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder()
            .label(if original_id.is_some() {
                "Update"
            } else {
                "Insert"
            })
            .css_classes(["suggested-action"])
            .build();
        let cancel = gtk::Button::with_label("Cancel");
        header.pack_start(&cancel);
        header.pack_end(&save);
        toolbar.add_top_bar(&header);
        let view = crate::ui::json_view(&text, true);
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(8)
            .margin_end(8)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.append(&scroller);
        body.append(&error);
        body.append(
            &gtk::Label::builder()
                .label("Ctrl+Enter saves · Esc cancels")
                .css_classes(["dim-label", "caption"])
                .margin_bottom(6)
                .build(),
        );
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));

        let commit: Rc<dyn Fn()> = {
            let me = self.clone();
            let view = view.clone();
            let error = error.clone();
            let dialog = dialog.clone();
            let app = app.clone();
            Rc::new(move || {
                let text = crate::ui::buffer_text(&view.buffer());
                match ejson::parse_document(&text) {
                    Err(e) => {
                        error.set_text(&e.to_string());
                        error.set_visible(true);
                    }
                    Ok(doc) => {
                        if app.write_guard().is_err() {
                            return;
                        }
                        dialog.close();
                        me.save_document(original_id.clone(), doc);
                    }
                }
            })
        };
        {
            let commit = commit.clone();
            save.connect_clicked(move |_| commit());
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
                if ctrl
                    && (key == gtk::gdk::Key::Return
                        || key == gtk::gdk::Key::KP_Enter
                        || key == gtk::gdk::Key::s)
                {
                    commit();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        dialog.add_controller(keys);
        dialog.present(Some(&app.window));
        view.grab_focus();
    }

    /// Insert (no id) or replace by id, then reload the page.
    pub fn save_document(self: &Rc<Self>, original_id: Option<Bson>, doc: Document) {
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let me = self.clone();
        glib::spawn_future_local(async move {
            let r = crate::rt::io(async move {
                match original_id {
                    Some(id) => ops::replace_by_id(&client, &ns, &id, doc)
                        .await
                        .map(|_| "Document updated".to_string()),
                    None => ops::insert_one(&client, &ns, doc)
                        .await
                        .map(|id| format!("Inserted {}", ejson::id_display(&id))),
                }
            })
            .await;
            match r {
                Ok(msg) => {
                    app.toast(&msg);
                    me.reload_keep_cursor();
                }
                Err(e) => app.toast_error(&format!("save to {}", me.ns), &e),
            }
        });
    }

    /// `e` / the pencil: the in-app dialog unless settings pick the external
    /// editor (explicitly, or by naming an editor command).
    pub fn edit(self: &Rc<Self>) {
        let external = self
            .app()
            .is_some_and(|a| a.config.borrow().settings.uses_external_editor());
        if external {
            self.edit_external();
        } else {
            self.edit_inline();
        }
    }

    pub fn edit_inline(self: &Rc<Self>) {
        let Some(doc) = self.current_doc() else {
            return;
        };
        let title = doc
            .get("_id")
            .map(ejson::id_display)
            .unwrap_or_else(|| "document".into());
        self.edit_dialog(
            ejson::pretty(&doc, Mode::Canonical),
            doc.get("_id").cloned(),
            &format!("Edit {title}"),
        );
    }

    pub fn add(self: &Rc<Self>) {
        let template = "{\n  \n}";
        self.edit_dialog(
            template.to_string(),
            None,
            &format!("Insert into {}", self.ns),
        );
    }

    pub fn duplicate(self: &Rc<Self>, confirm: bool) {
        let Some(mut doc) = self.current_doc() else {
            return;
        };
        doc.remove("_id");
        if confirm {
            self.edit_dialog(
                ejson::pretty(&doc, Mode::Canonical),
                None,
                "Duplicate document",
            );
        } else {
            if let Some(app) = self.app()
                && app.write_guard().is_err()
            {
                return;
            }
            self.save_document(None, doc);
        }
    }

    /// `E`: the external editor, but only when settings pick it; with the
    /// in-app editor chosen this is the same as `e`.
    pub fn edit_external(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        if !app.config.borrow().settings.uses_external_editor() {
            self.edit_inline();
            return;
        }
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let docs = self.docs.borrow();
        let settings = app.config.borrow().settings.clone();
        let result = if targets.len() == 1 {
            let doc = &docs[targets[0]];
            let id = doc.get("_id").cloned();
            let title = format!(
                "{} — {}",
                self.ns,
                id.as_ref().map(ejson::id_display).unwrap_or_default()
            );
            app.editor_pane.open(
                &settings,
                JobKind::Document {
                    conn: self.conn,
                    ns: self.ns.clone(),
                    id,
                    is_new: false,
                },
                crate::ui::editor_pane::document_text(doc),
                &title,
            )
        } else {
            let many: Vec<Document> = targets.iter().map(|&i| docs[i].clone()).collect();
            app.editor_pane.open(
                &settings,
                JobKind::Documents {
                    conn: self.conn,
                    ns: self.ns.clone(),
                },
                ejson::pretty_many(&many, Mode::Canonical),
                &format!("{} — {} documents", self.ns, many.len()),
            )
        };
        if let Err(e) = result {
            app.toast_error("open editor", &e);
        }
    }

    pub fn add_external(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        let settings = app.config.borrow().settings.clone();
        if let Err(e) = app.editor_pane.open(
            &settings,
            JobKind::Document {
                conn: self.conn,
                ns: self.ns.clone(),
                id: None,
                is_new: true,
            },
            "{\n  \n}\n".into(),
            &format!("{} — new document", self.ns),
        ) {
            app.toast_error("open editor", &e);
        }
    }

    pub fn delete(self: &Rc<Self>, confirm: bool) {
        let Some(app) = self.app() else { return };
        if app.write_guard().is_err() {
            return;
        }
        let targets = self.targets();
        let ids: Vec<Bson> = {
            let docs = self.docs.borrow();
            targets
                .iter()
                .filter_map(|&i| docs.get(i).and_then(|d| d.get("_id").cloned()))
                .collect()
        };
        self.delete_ids(ids, confirm);
    }

    /// Delete these `_id`s (the quick view deletes the one it shows).
    pub fn delete_ids(self: &Rc<Self>, ids: Vec<Bson>, confirm: bool) {
        let Some(app) = self.app() else { return };
        if ids.is_empty() || app.write_guard().is_err() {
            return;
        }
        let n = ids.len();
        let me = self.clone();
        let go = move || {
            let Some(app) = me.app() else { return };
            let Some(conn) = app.conn(me.conn) else {
                return;
            };
            let client = conn.client.clone();
            let ns = me.ns.clone();
            let ids = ids.clone();
            let me = me.clone();
            glib::spawn_future_local(async move {
                match crate::rt::io(async move { ops::delete_by_ids(&client, &ns, ids).await })
                    .await
                {
                    Ok(n) => {
                        app.toast(&format!(
                            "Deleted {n} document{}",
                            if n == 1 { "" } else { "s" }
                        ));
                        me.reload_keep_cursor();
                    }
                    Err(e) => app.toast_error(&format!("delete from {}", me.ns), &e),
                }
            });
        };
        if confirm {
            crate::ui::confirm(
                &app.window.clone(),
                &format!("Delete {n} document{}?", if n == 1 { "" } else { "s" }),
                &format!("From {}. This cannot be undone.", self.ns),
                "Delete",
                true,
                go,
            );
        } else {
            go();
        }
    }

    /// `c`: the current column's value in the table view, else the `_id`.
    pub fn copy_value(&self) {
        let Some(doc) = self.current_doc() else {
            return;
        };
        let Some(app) = self.app() else { return };
        let (label, value) = if self.view.get() == DocView::Table {
            match self
                .current_column()
                .and_then(|c| doc.get(&c).map(|v| (c, v.clone())))
            {
                Some((c, v)) => (c, v),
                None => return,
            }
        } else {
            match doc.get("_id") {
                Some(v) => ("_id".to_string(), v.clone()),
                None => return,
            }
        };
        let text = match &value {
            Bson::String(s) => s.clone(),
            Bson::ObjectId(o) => o.to_hex(),
            Bson::Document(d) => ejson::compact(d, Mode::Relaxed),
            other => ejson::summary(other, usize::MAX),
        };
        crate::ui::copy_text(&text);
        app.toast(&format!("Copied {label}"));
    }

    pub fn copy_document(&self) {
        let Some(app) = self.app() else { return };
        let targets = self.targets();
        let docs = self.docs.borrow();
        let selected: Vec<Document> = targets
            .iter()
            .filter_map(|&i| docs.get(i).cloned())
            .collect();
        if selected.is_empty() {
            return;
        }
        let text = if selected.len() == 1 {
            ejson::pretty(&selected[0], Mode::Relaxed)
        } else {
            ejson::pretty_many(&selected, Mode::Relaxed)
        };
        crate::ui::copy_text(&text);
        app.toast(&format!(
            "Copied {} document{}",
            selected.len(),
            if selected.len() == 1 { "" } else { "s" }
        ));
    }

    pub fn toggle_expand_all(&self) {
        let on = !self.expanded_all.get();
        self.expanded_all.set(on);
        self.expand_action.set_state(&on.to_variant());
        self.refresh_rows();
    }

    /// `S`: sort by the current column (table) or `_id`, toggling direction.
    pub fn sort_by_column(self: &Rc<Self>) {
        let col = if self.view.get() == DocView::Table {
            self.current_column()
        } else {
            None
        }
        .unwrap_or_else(|| "_id".into());
        let mut q = self.current_query();
        let asc = format!("{{ \"{col}\": 1 }}");
        let desc = format!("{{ \"{col}\": -1 }}");
        q.sort = if q.sort.replace(' ', "") == asc.replace(' ', "") {
            desc
        } else {
            asc
        };
        self.query_bar.set_query(&q);
        self.run_query(q);
    }

    pub fn hide_column(&self) {
        if let Some(c) = self.current_column() {
            self.hidden_columns.borrow_mut().insert(c);
            table::rebuild_columns(self);
            self.move_column(0);
        }
    }

    pub fn reset_columns(&self) {
        self.hidden_columns.borrow_mut().clear();
        table::rebuild_columns(self);
        self.move_column(0);
    }

    pub fn focus_views(&self) {
        self.views.grab_focus();
    }

    /// `u`: bulk update everything the current filter matches.
    pub fn bulk_update(self: &Rc<Self>, initial: Option<String>) {
        if let Some(app) = self.app() {
            crate::ui::bulk::update_dialog(&app, self, initial);
        }
    }

    /// `Ctrl+Shift+d`: bulk delete everything the current filter matches.
    pub fn bulk_delete(self: &Rc<Self>) {
        if let Some(app) = self.app() {
            crate::ui::bulk::delete_dialog(&app, self);
        }
    }
}

/// Bound table widget creation for very wide / heterogeneous documents. The
/// complete fields are available by opening a document. Runs off the UI thread.
fn page_columns(docs: &[Document]) -> Vec<String> {
    const MAX_COLUMNS: usize = 64;
    let mut columns = Vec::new();
    let mut seen = HashSet::new();
    if docs.iter().any(|d| d.contains_key("_id")) {
        columns.push("_id".into());
        seen.insert("_id");
    }
    for doc in docs {
        for key in doc.keys().take(MAX_COLUMNS) {
            if columns.len() == MAX_COLUMNS {
                return columns;
            }
            if seen.insert(key.as_str()) {
                columns.push(key.clone());
            }
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;
    use std::time::{Duration, Instant};

    #[test]
    fn columns_are_bounded_and_keep_first_seen_order() {
        assert_eq!(
            page_columns(&[doc! { "b": 1, "_id": 0 }, doc! { "a": 2, "b": 3 }]),
            vec!["_id", "b", "a"]
        );
        let wide: Document = (0..10_000)
            .map(|i| (format!("k{i}"), Bson::Int32(i)))
            .collect();
        assert_eq!(page_columns(&[wide]).len(), 64);
        assert!(page_columns(&[]).is_empty());
    }

    /// Run under xvfb-run with an isolated XDG_CONFIG_HOME. Optionally set
    /// VITI_TEST_URI to an isolated MongoDB with enableTestCommands=1 to verify
    /// that a delayed count does not delay pages. Never use a production server.
    #[test]
    #[ignore = "requires GTK display and isolated config; optional isolated MongoDB failpoints"]
    fn gtk_large_document_pagination() {
        adw::init().unwrap();
        let application = adw::Application::builder()
            .application_id("dev.turbinebmw.Viti.PaginationTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let app = App::build(&application);
        let conn_id = uuid::Uuid::new_v4();
        let pane = DocumentsPane::new(
            &app,
            conn_id,
            Namespace::new(
                &format!("viti_it_{}", uuid::Uuid::new_v4().simple()),
                "pages",
            ),
        );
        app.window.set_content(Some(&pane.root));
        let context = glib::MainContext::default();
        context.block_on(async {
            for view in [DocView::List, DocView::Json, DocView::Table] {
                pane.set_view(view);
                for expanded in [false, true] {
                    pane.expanded_all.set(expanded);
                    for page in 0..3 {
                        // ~30 MB per page, with both large strings and arrays.
                        let docs = crate::rt::io(async move {
                            (0..25).map(|i| doc! { "_id": page * 25 + i,
                                "text": "界".repeat(350_000),
                                "array": vec![doc! { "x": 1 }; 10_000],
                                "nested": { "a": { "b": { "c": [1, 2, 3] } } }
                            }).collect::<Vec<_>>()
                        }).await;
                        let cols = page_columns(&docs);
                        let started = Instant::now();
                        pane.page.set(page as u64);
                        pane.loaded_page.set(page as u64);
                        pane.set_docs(docs, cols);
                        pane.bottom();
                        pane.top();
                        glib::timeout_future(Duration::from_millis(25)).await;
                        let elapsed = started.elapsed();
                        eprintln!("{view:?} expanded={expanded} page={page}: {elapsed:?}");
                        assert!(elapsed < Duration::from_secs(2), "GTK stalled: {elapsed:?}");
                        assert_eq!(pane.list_view.model().is_some(), view == DocView::List);
                        assert_eq!(pane.json_view.model().is_some(), view == DocView::Json);
                        assert_eq!(pane.table_view.model().is_some(), view == DocView::Table);
                        assert_eq!(pane.docs.borrow()[0].get_str("text").unwrap().len(), 1_050_000);
                    }
                }
            }
            if let Ok(uri) = std::env::var("VITI_TEST_URI") {
                let ns = pane.ns.clone();
                let client = crate::rt::io(async move {
                    let client = mongodb::Client::with_uri_str(uri).await.unwrap();
                    ops::insert_many(&client, &ns, (0..55).map(|i| doc! { "_id": i }).collect()).await.unwrap();
                    client.database("admin").run_command(doc! {
                        "configureFailPoint": "failCommand", "mode": { "times": 1 },
                        "data": { "failCommands": ["count"], "blockConnection": true, "blockTimeMS": 2500 }
                    }).await.unwrap();
                    client
                }).await;
                app.conns.borrow_mut().push(Rc::new(crate::mongo::Conn {
                    id: conn_id, client: client.clone(), profile: Default::default(),
                    server: Default::default(), tunnel: None,
                }));
                pane.page_size.set(25);
                pane.page.set(0);
                pane.set_view(DocView::List);
                let started = Instant::now();
                pane.load();
                wait_for_page(&pane).await;
                assert!(started.elapsed() < Duration::from_secs(2));
                assert!(pane.count_inflight.borrow().is_some(), "page should arrive before count");
                let count_generation = pane.count_generation.get();
                assert_eq!(pane.docs.borrow().len(), 25);
                assert!(pane.next_btn.is_sensitive());
                pane.next_page();
                wait_for_page(&pane).await;
                assert_eq!(pane.docs.borrow()[0].get_i32("_id").unwrap(), 25);
                pane.next_page();
                wait_for_page(&pane).await;
                assert_eq!(pane.docs.borrow().len(), 5);
                assert!(!pane.next_btn.is_sensitive());
                pane.prev_page();
                wait_for_page(&pane).await;
                assert_eq!(pane.docs.borrow()[0].get_i32("_id").unwrap(), 25);
                assert_eq!(pane.count_generation.get(), count_generation, "page turns must reuse count");
                let db = pane.ns.db.clone();
                crate::rt::io(async move {
                    client.database("admin").run_command(doc! {
                        "configureFailPoint": "failCommand", "mode": "off"
                    }).await.unwrap();
                    client.database(&db).drop().await.unwrap();
                }).await;
            }
        });
        app.window.destroy();
    }

    async fn wait_for_page(pane: &DocumentsPane) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.busy.get() {
            assert!(Instant::now() < deadline, "page load timed out");
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    }
}
