//! The Explain Plan page: explain the query bar's find or the aggregation
//! pipeline, show the headline numbers as tiles, the plan as a tree (with
//! per-stage details beside it) or the raw explain JSON.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::explain::{self, PlanNode, Summary};
use crate::mongo::ops::{self, Namespace, OpCtx, VERBOSITIES};
use crate::ui::aggregation::AggregationPane;
use crate::ui::documents::DocumentsPane;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub struct ExplainPane {
    pub root: gtk::Box,
    source: gtk::DropDown,
    verbosity: gtk::DropDown,
    run_btn: gtk::Button,
    stop_btn: gtk::Button,
    spinner: gtk::Spinner,
    stack: gtk::Stack,
    raw_toggle: gtk::ToggleButton,
    tiles: Vec<(gtk::Label, gtk::Label)>,
    tree_view: gtk::ListView,
    roots: gio::ListStore,
    tree: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    details: sourceview5::View,
    raw_view: sourceview5::View,
    status: gtk::Label,
    explanation: gtk::Box,
    explanation_label: gtk::Label,

    pub conn: ConnectionId,
    pub ns: Namespace,
    app: Weak<App>,
    docs: RefCell<Weak<DocumentsPane>>,
    agg: RefCell<Weak<AggregationPane>>,
    summary: RefCell<Option<Summary>>,
    raw: RefCell<String>,
    inflight: RefCell<Option<(OpCtx, tokio::task::AbortHandle)>>,
    generation: Cell<u64>,
    me: RefCell<Weak<Self>>,
}

const TILES: [&str; 6] = [
    "Documents returned",
    "Execution time",
    "Documents examined",
    "Keys examined",
    "Index used",
    "Sort in memory",
];

fn tile(caption: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let value = gtk::Label::builder()
        .label("—")
        .xalign(0.0)
        .css_classes(["value"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let cap = gtk::Label::builder()
        .label(caption)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build();
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("viti-tile");
    b.set_hexpand(true);
    b.append(&value);
    b.append(&cap);
    (b, value, cap)
}

fn ms(n: Option<i64>) -> String {
    match n {
        Some(v) => format!("{v} ms"),
        None => "—".into(),
    }
}

fn count(n: Option<i64>) -> String {
    match n {
        Some(v) => crate::ui::thousands(v.max(0) as u64),
        None => "—".into(),
    }
}

impl ExplainPane {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let settings = app.config.borrow().settings.clone();
        let tip = |base: &str, id: &str| {
            let accel = crate::keybinds::accel_for(&settings, id);
            if accel.is_empty() {
                base.to_string()
            } else {
                format!("{base} ({})", crate::keybinds::pretty_accel(&accel))
            }
        };
        let source = gtk::DropDown::from_strings(&["Query", "Pipeline"]);
        source.set_tooltip_text(Some(&tip(
            "Explain the query bar's find or the pipeline",
            "explain.source",
        )));
        let verbosity = gtk::DropDown::from_strings(&VERBOSITIES);
        verbosity.set_selected(1);
        verbosity.set_tooltip_text(Some(&tip("Verbosity", "explain.verbosity")));
        let run_btn = gtk::Button::builder()
            .label("Explain")
            .tooltip_text(tip("Run explain", "explain.run"))
            .focus_on_click(false)
            .css_classes(["suggested-action"])
            .build();
        let stop_btn = gtk::Button::builder()
            .label("Stop")
            .tooltip_text("Cancel (Esc)")
            .focus_on_click(false)
            .css_classes(["destructive-action"])
            .visible(false)
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let raw_toggle = gtk::ToggleButton::builder()
            .icon_name("text-x-generic-symbolic")
            .tooltip_text(tip("Raw explain JSON", "explain.toggle-view"))
            .focus_on_click(false)
            .build();
        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(tip("Copy raw explain", "explain.copy"))
            .focus_on_click(false)
            .build();
        let status = gtk::Label::builder()
            .xalign(1.0)
            .hexpand(true)
            .css_classes(["viti-count"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&source);
        bar.append(&verbosity);
        bar.append(&status);
        bar.append(&spinner);
        let ai_btn = gtk::Button::builder()
            .label("Explain with AI")
            .tooltip_text("Ask the AI backend to explain this plan (Ctrl+I)")
            .focus_on_click(false)
            .build();
        bar.append(&ai_btn);
        bar.append(&raw_toggle);
        bar.append(&copy);
        bar.append(&run_btn);
        bar.append(&stop_btn);

        // The AI's explanation, shown under the tiles once asked for.
        let explanation_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        let explanation_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Dismiss")
            .valign(gtk::Align::Start)
            .css_classes(["flat"])
            .build();
        let explanation_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        explanation_head.append(
            &gtk::Label::builder()
                .label("AI explanation")
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        explanation_head.append(&explanation_close);
        let explanation = gtk::Box::new(gtk::Orientation::Vertical, 4);
        explanation.add_css_class("viti-ai-card");
        explanation.set_margin_start(8);
        explanation.set_margin_end(8);
        explanation.set_margin_bottom(6);
        explanation.set_visible(false);
        explanation.append(&explanation_head);
        explanation.append(&explanation_label);

        let tiles_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        tiles_box.set_margin_start(8);
        tiles_box.set_margin_end(8);
        tiles_box.set_margin_bottom(6);
        let mut tiles = Vec::new();
        for t in TILES {
            let (b, v, c) = tile(t);
            tiles_box.append(&b);
            tiles.push((v, c));
        }

        // Plan tree: roots → children through a TreeListModel of PlanNodes.
        let roots = gio::ListStore::new::<BoxedAnyObject>();
        let tree = gtk::TreeListModel::new(roots.clone(), false, true, |obj| {
            let node = obj.downcast_ref::<BoxedAnyObject>()?.borrow::<PlanNode>();
            if node.children.is_empty() {
                return None;
            }
            let store = gio::ListStore::new::<BoxedAnyObject>();
            for c in &node.children {
                store.append(&BoxedAnyObject::new(c.clone()));
            }
            Some(store.upcast())
        });
        let selection = gtk::SingleSelection::new(Some(tree.clone()));
        selection.set_autoselect(true);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let expander = gtk::TreeExpander::new();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_margin_top(3);
            row.set_margin_bottom(3);
            let stage = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["viti-key"])
                .build();
            let badges = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["viti-count"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .hexpand(true)
                .build();
            row.append(&stage);
            row.append(&badges);
            expander.set_child(Some(&row));
            item.set_child(Some(&expander));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
            let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
                return;
            };
            expander.set_list_row(Some(&row));
            let Some(obj) = row.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            let node = obj.borrow::<PlanNode>();
            let content = expander.child().and_downcast::<gtk::Box>().unwrap();
            let stage = content.first_child().and_downcast::<gtk::Label>().unwrap();
            let badges = content.last_child().and_downcast::<gtk::Label>().unwrap();
            let mut label = node.label();
            if let Some(s) = &node.shard {
                label = format!("{s}: {label}");
            }
            stage.set_text(&label);
            if node.stage == "COLLSCAN" {
                stage.add_css_class("viti-collscan");
            } else {
                stage.remove_css_class("viti-collscan");
            }
            let mut parts: Vec<String> = Vec::new();
            if let Some(n) = node.n_returned {
                parts.push(format!(
                    "{} returned",
                    crate::ui::thousands(n.max(0) as u64)
                ));
            }
            if let Some(t) = node.exec_ms {
                parts.push(format!("{t} ms"));
            }
            if let Some(d) = node.docs_examined {
                parts.push(format!(
                    "{} docs examined",
                    crate::ui::thousands(d.max(0) as u64)
                ));
            }
            if let Some(k) = node.keys_examined {
                parts.push(format!(
                    "{} keys examined",
                    crate::ui::thousands(k.max(0) as u64)
                ));
            }
            if let Ok(f) = node.details.get_document("filter")
                && !f.is_empty()
            {
                parts.push(format!(
                    "filter {}",
                    ejson::truncate(&ejson::compact(f, Mode::Relaxed), 60)
                ));
            }
            if let Ok(s) = node.details.get_document("sortPattern") {
                parts.push(format!("sort {}", ejson::compact(s, Mode::Relaxed)));
            }
            badges.set_text(&parts.join("  ·  "));
        });
        let tree_view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        tree_view.add_css_class("navigation-sidebar");
        tree_view.set_can_focus(false);
        let tree_scroller = gtk::ScrolledWindow::builder()
            .child(&tree_view)
            .vexpand(true)
            .hexpand(true)
            .build();
        let details = crate::ui::json_view("", false);
        let details_scroller = gtk::ScrolledWindow::builder()
            .child(&details)
            .vexpand(true)
            .build();
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&tree_scroller));
        paned.set_end_child(Some(&details_scroller));
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_position(560);
        let raw_view = crate::ui::json_view("", false);
        let raw_scroller = gtk::ScrolledWindow::builder()
            .child(&raw_view)
            .vexpand(true)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("dialog-information-symbolic")
            .title("Explain plan")
            .description("Press R (or Explain) to run explain for the query bar's find, or switch the source to the pipeline.")
            .build();
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&paned, Some("tree"));
        stack.add_named(&raw_scroller, Some("raw"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-explain");
        root.append(&bar);
        root.append(&tiles_box);
        root.append(&explanation);
        root.append(&stack);

        let pane = Rc::new(Self {
            root,
            source: source.clone(),
            verbosity: verbosity.clone(),
            run_btn: run_btn.clone(),
            stop_btn: stop_btn.clone(),
            spinner,
            stack,
            raw_toggle: raw_toggle.clone(),
            tiles,
            tree_view: tree_view.clone(),
            roots,
            tree,
            selection: selection.clone(),
            details,
            raw_view,
            status,
            explanation: explanation.clone(),
            explanation_label,
            conn,
            ns,
            app: Rc::downgrade(app),
            docs: RefCell::new(Weak::new()),
            agg: RefCell::new(Weak::new()),
            summary: RefCell::new(None),
            raw: RefCell::new(String::new()),
            inflight: RefCell::new(None),
            generation: Cell::new(0),
            me: RefCell::new(Weak::new()),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);
        {
            let p = pane.clone();
            run_btn.connect_clicked(move |_| p.run());
        }
        {
            let p = pane.clone();
            stop_btn.connect_clicked(move |_| p.cancel());
        }
        {
            let p = pane.clone();
            raw_toggle.connect_toggled(move |_| p.apply_view());
        }
        {
            let p = pane.clone();
            copy.connect_clicked(move |_| p.copy_raw());
        }
        {
            let p = pane.clone();
            ai_btn.connect_clicked(move |_| p.ai_explain());
        }
        {
            let p = pane.clone();
            explanation_close.connect_clicked(move |_| p.set_explanation(None));
        }
        {
            let p = pane.clone();
            selection.connect_selected_notify(move |_| p.show_details());
        }
        {
            let p = pane.clone();
            tree_view.connect_activate(move |_, _| p.peek());
        }
        {
            let root = pane.root.clone();
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |_, _, _, _| {
                root.grab_focus();
            });
            tree_view.add_controller(click);
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    fn me(&self) -> Option<Rc<Self>> {
        self.me.borrow().upgrade()
    }

    /// Where the query and the pipeline come from (the tab's other pages).
    pub fn set_sources(&self, docs: &Rc<DocumentsPane>, agg: &Rc<AggregationPane>) {
        *self.docs.borrow_mut() = Rc::downgrade(docs);
        *self.agg.borrow_mut() = Rc::downgrade(agg);
    }

    /// "query" or "pipeline".
    pub fn set_source(&self, source: &str) {
        self.source
            .set_selected(if source == "pipeline" { 1 } else { 0 });
    }

    pub fn toggle_source(&self) {
        self.source.set_selected(1 - self.source.selected().min(1));
        self.run();
    }

    pub fn cycle_verbosity(&self) {
        let n = VERBOSITIES.len() as u32;
        self.verbosity
            .set_selected((self.verbosity.selected() + 1) % n);
        if let Some(app) = self.app() {
            app.toast(&format!(
                "Verbosity: {}",
                VERBOSITIES[self.verbosity.selected() as usize]
            ));
        }
    }

    fn set_busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.run_btn.set_visible(!busy);
        self.stop_btn.set_visible(busy);
    }

    pub fn cancel(&self) {
        if let Some((ctx, handle)) = self.inflight.borrow_mut().take() {
            handle.abort();
            self.generation.set(self.generation.get() + 1);
            if let Some(app) = self.app() {
                if let Some(conn) = app.conn(self.conn) {
                    let client = conn.client.clone();
                    crate::rt::spawn(async move {
                        if let Err(e) = ops::kill_by_comment(&client, &ctx.comment).await {
                            tracing::warn!("killOp: {e:#}");
                        }
                    });
                }
                app.toast("Explain cancelled");
            }
            self.set_busy(false);
        }
    }

    pub fn run(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            app.toast("Not connected");
            return;
        };
        let mut verbosity = VERBOSITIES[self.verbosity.selected() as usize];
        let pipeline_source = self.source.selected() == 1;
        enum Job {
            Find(Box<ops::FindSpec>),
            Agg(Vec<bson::Document>, ops::AggOpts),
        }
        let job = if pipeline_source {
            let Some(agg) = self.agg.borrow().upgrade() else {
                return;
            };
            let Some((docs, opts)) = agg.pipeline_for_run() else {
                return;
            };
            if crate::mongo::pipeline::Pipeline::from_documents(&docs)
                .map(|p| p.has_write_stage())
                .unwrap_or(false)
                && verbosity != "queryPlanner"
            {
                app.toast("Pipelines with $out / $merge are explained with queryPlanner only");
                verbosity = "queryPlanner";
            }
            Job::Agg(docs, opts)
        } else {
            let Some(docs) = self.docs.borrow().upgrade() else {
                return;
            };
            Job::Find(Box::new(docs.current_spec()))
        };
        if let Some((_, h)) = self.inflight.borrow_mut().take() {
            h.abort();
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ctx = OpCtx::new(app.max_time_ms());
        let ctx2 = ctx.clone();
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let verbosity = verbosity.to_string();
        let (tx, rx) = async_channel::bounded(1);
        let handle = crate::rt::spawn(async move {
            let started = std::time::Instant::now();
            let r = match job {
                Job::Find(spec) => ops::explain_find(&client, &ns, &spec, &verbosity, &ctx2).await,
                Job::Agg(p, o) => {
                    ops::explain_aggregate(&client, &ns, p, &o, &verbosity, &ctx2).await
                }
            };
            let _ = tx.send((r, started.elapsed())).await;
        });
        *self.inflight.borrow_mut() = Some((ctx, handle.abort_handle()));
        self.set_busy(true);
        self.status.set_text(&format!(
            "Explaining the {}…",
            if pipeline_source { "pipeline" } else { "query" }
        ));
        glib::spawn_future_local(async move {
            let Ok((result, elapsed)) = rx.recv().await else {
                return;
            };
            if me.generation.get() != generation {
                return;
            }
            me.inflight.borrow_mut().take();
            me.set_busy(false);
            match result {
                Ok(doc) => {
                    me.status.set_text(&format!(
                        "{} · explained in {} ms",
                        if pipeline_source { "pipeline" } else { "query" },
                        elapsed.as_millis()
                    ));
                    me.show(doc);
                }
                Err(e) => {
                    me.status.set_text("");
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("explain on {}", me.ns), &e);
                    }
                }
            }
        });
    }

    fn show(&self, doc: bson::Document) {
        let summary = explain::parse(&doc);
        let raw = ejson::pretty(&doc, Mode::Relaxed);
        self.raw_view.buffer().set_text(&raw);
        *self.raw.borrow_mut() = raw;
        // Tiles.
        let set = |i: usize, v: &str| self.tiles[i].0.set_text(v);
        set(0, &count(summary.n_returned));
        set(1, &ms(summary.exec_ms));
        set(2, &count(summary.docs_examined));
        set(3, &count(summary.keys_examined));
        let (index_text, collscan) = if summary.collscan && summary.indexes.is_empty() {
            ("COLLSCAN".to_string(), true)
        } else if summary.indexes.is_empty() {
            ("—".into(), false)
        } else {
            (summary.indexes.join(", "), summary.collscan)
        };
        set(4, &index_text);
        if collscan {
            self.tiles[4].0.add_css_class("viti-collscan");
        } else {
            self.tiles[4].0.remove_css_class("viti-collscan");
        }
        set(
            5,
            if summary.sort_spilled {
                "yes, spilled to disk"
            } else if summary.sort_in_memory {
                "yes"
            } else {
                "no"
            },
        );
        self.tiles[1].1.set_text(if summary.has_execution_stats {
            "Execution time"
        } else {
            "Execution time (queryPlanner: not run)"
        });
        self.tiles[4].1.set_text(&if summary.rejected_plans > 0 {
            format!("Index used · {} rejected plan(s)", summary.rejected_plans)
        } else if summary.shards > 0 {
            format!("Index used · {} shard(s)", summary.shards)
        } else {
            "Index used".into()
        });
        // Tree.
        self.roots.remove_all();
        if let Some(root) = &summary.root {
            self.roots.append(&BoxedAnyObject::new(root.clone()));
        }
        *self.summary.borrow_mut() = Some(summary);
        self.apply_view();
        self.expand_all();
        if self.tree.n_items() > 0 {
            self.selection.set_selected(0);
        }
        self.show_details();
    }

    fn apply_view(&self) {
        let has = self.summary.borrow().is_some();
        self.stack.set_visible_child_name(if !has {
            "empty"
        } else if self.raw_toggle.is_active() {
            "raw"
        } else {
            "tree"
        });
    }

    pub fn toggle_view(&self) {
        self.raw_toggle.set_active(!self.raw_toggle.is_active());
    }

    /// The last explain output as Relaxed Extended JSON (empty until run).
    pub fn raw_text(&self) -> String {
        self.raw.borrow().clone()
    }

    /// Ask the AI backend to explain the current plan.
    pub fn ai_explain(&self) {
        if let Some(app) = self.app() {
            crate::ui::ai::ask(
                &app,
                Some(crate::ai::Task::ExplainPlan),
                Some(String::new()),
            );
        }
    }

    /// Show (or, with `None`, hide) the AI's explanation under the tiles.
    pub fn set_explanation(&self, text: Option<&str>) {
        match text {
            Some(t) => {
                self.explanation_label.set_text(t.trim());
                self.explanation.set_visible(true);
            }
            None => self.explanation.set_visible(false),
        }
    }

    pub fn copy_raw(&self) {
        let raw = self.raw.borrow();
        if raw.is_empty() {
            return;
        }
        crate::ui::copy_text(&raw);
        if let Some(app) = self.app() {
            app.toast("Explain copied");
        }
    }

    fn expand_all(&self) {
        // Rows are created lazily; expanding in order reaches every level.
        let mut i = 0;
        while i < self.tree.n_items() {
            if let Some(row) = self.tree.row(i) {
                row.set_expanded(true);
            }
            i += 1;
        }
    }

    fn current_row(&self) -> Option<gtk::TreeListRow> {
        let s = self.selection.selected();
        if s == gtk::INVALID_LIST_POSITION {
            return None;
        }
        self.tree.row(s)
    }

    fn current_node(&self) -> Option<PlanNode> {
        let row = self.current_row()?;
        let obj = row.item().and_downcast::<BoxedAnyObject>()?;
        let node = obj.borrow::<PlanNode>().clone();
        Some(node)
    }

    fn show_details(&self) {
        let text = match self.current_node() {
            Some(n) => ejson::pretty(&n.details, Mode::Relaxed),
            None => String::new(),
        };
        self.details.buffer().set_text(&text);
    }

    pub fn cursor(&self) -> Option<usize> {
        let s = self.selection.selected();
        (s != gtk::INVALID_LIST_POSITION).then_some(s as usize)
    }

    pub fn move_cursor(&self, delta: i64) {
        let n = self.tree.n_items() as i64;
        if n == 0 {
            return;
        }
        let cur = self.cursor().map(|c| c as i64).unwrap_or(-1);
        let next = if cur < 0 {
            0
        } else {
            (cur + delta).clamp(0, n - 1)
        };
        self.selection.set_selected(next as u32);
        self.tree_view
            .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
    }

    pub fn top(&self) {
        if self.tree.n_items() > 0 {
            self.selection.set_selected(0);
        }
    }

    pub fn bottom(&self) {
        let n = self.tree.n_items();
        if n > 0 {
            self.selection.set_selected(n - 1);
        }
    }

    /// `l` / `h`: expand or collapse the current node.
    pub fn set_expanded(&self, expanded: bool) {
        if let Some(row) = self.current_row() {
            row.set_expanded(expanded);
        }
    }

    pub fn peek(&self) {
        let Some(app) = self.app() else { return };
        let Some(node) = self.current_node() else {
            return;
        };
        crate::ui::peek_document(
            &app,
            &node.label(),
            &ejson::pretty(&node.details, Mode::Relaxed),
        );
    }
}
