//! The Aggregations page: Compass's pipeline builder with vi keys. Stage
//! cards (operator, body editor, enable switch, live output preview) or one
//! text editor for the whole array; results paged below; a focus mode for one
//! stage with its input and output side by side; save / open pipelines,
//! create a view, export to language, explain, and `Ctrl+E` for the external
//! editor.
use crate::app::App;
use crate::config::SavedPipeline;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, AggOpts, Namespace, OpCtx};
use crate::mongo::pipeline::{self, Pipeline, STAGES, Stage};
use adw::prelude::*;
use bson::{Document, doc};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Documents shown per stage preview.
const PREVIEW_N: i64 = 10;
/// Results per page.
const PAGE: u64 = 20;
const PREVIEW_DEBOUNCE_MS: u64 = 600;

type Callback = Rc<dyn Fn()>;

/// One stage card.
pub struct StageCard {
    root: gtk::Box,
    number: gtk::Label,
    operator: gtk::DropDown,
    enabled: gtk::Switch,
    editor: sourceview5::View,
    error: gtk::Label,
    preview_title: gtk::Label,
    preview_box: gtk::Box,
    preview_docs: RefCell<Vec<Document>>,
    generation: Cell<u64>,
    /// The focus-mode dialog, when open, redraws its output on this.
    on_preview: RefCell<Option<Callback>>,
    /// Template of the operator currently selected, to know whether the body
    /// was touched when the operator changes.
    last_template: RefCell<String>,
}

pub struct AggregationPane {
    pub root: gtk::Box,
    name_label: gtk::Label,
    mode_btn: gtk::ToggleButton,
    preview_btn: gtk::ToggleButton,
    run_btn: gtk::Button,
    stop_btn: gtk::Button,
    spinner: gtk::Spinner,
    status: gtk::Label,
    saved_btn: gtk::MenuButton,
    saved_popover: gtk::Popover,
    saved_list: gtk::ListBox,
    options: gtk::Revealer,
    collation: gtk::Entry,
    max_time: gtk::SpinButton,
    disk_use: gtk::CheckButton,
    mode_stack: gtk::Stack,
    stages_box: gtk::Box,
    stages_scroller: gtk::ScrolledWindow,
    empty_hint: gtk::Label,
    text_view: sourceview5::View,
    text_error: gtk::Label,
    paned: gtk::Paned,
    results_root: gtk::Box,
    results_list: gtk::ListView,
    results_model: gio::ListStore,
    results_selection: gtk::SingleSelection,
    results_status: gtk::Label,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,

    pub conn: ConnectionId,
    pub ns: Namespace,
    app: Weak<App>,
    cards: RefCell<Vec<Rc<StageCard>>>,
    cursor: Cell<usize>,
    results_focused: Cell<bool>,
    results: RefCell<Vec<Document>>,
    page: Cell<u64>,
    /// The saved pipeline this was opened from, for Save = update.
    loaded: RefCell<Option<uuid::Uuid>>,
    inflight: RefCell<Option<(OpCtx, tokio::task::AbortHandle)>>,
    generation: Cell<u64>,
    preview_debounce: RefCell<Option<glib::SourceId>>,
    preview_dirty_from: Cell<usize>,
    me: RefCell<Weak<Self>>,
}

fn stage_names() -> Vec<&'static str> {
    STAGES.iter().map(|s| s.name).collect()
}

/// A one-line rendering of a preview document; the tooltip carries it whole.
fn preview_row(app: &Rc<App>, doc: &Document) -> gtk::Widget {
    let text = ejson::compact(doc, Mode::Relaxed);
    let label = gtk::Label::builder()
        .label(&text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["viti-preview-doc"])
        .build();
    label.set_tooltip_text(Some(&ejson::truncate(
        &ejson::pretty(doc, Mode::Relaxed),
        1500,
    )));
    let click = gtk::GestureClick::new();
    let app = app.clone();
    let pretty = ejson::pretty(doc, Mode::Relaxed);
    let title = doc
        .get("_id")
        .map(ejson::id_display)
        .unwrap_or_else(|| "document".into());
    click.connect_released(move |_, _, _, _| {
        crate::ui::peek_document(&app, &title, &pretty);
    });
    label.add_controller(click);
    label.upcast()
}

impl StageCard {
    fn new(app: &Rc<App>, stage: &Stage) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-stage");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("viti-stage-header");
        let number = gtk::Label::builder().css_classes(["viti-count"]).build();
        let operator = gtk::DropDown::from_strings(&stage_names());
        operator.set_enable_search(true);
        operator.set_expression(Some(&gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        )));
        operator.set_selected(pipeline::stage_index(&stage.operator).unwrap_or(0) as u32);
        operator.set_tooltip_text(pipeline::stage_info(&stage.operator).map(|s| s.help));
        let enabled = gtk::Switch::builder()
            .active(stage.enabled)
            .valign(gtk::Align::Center)
            .tooltip_text("Enabled (t)")
            .build();
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Remove stage (Ctrl+D)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Move up (K)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Move down (J)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let focus = gtk::Button::builder()
            .icon_name("view-fullscreen-symbolic")
            .tooltip_text("Focus mode (f)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&number);
        header.append(&operator);
        header.append(&spacer);
        header.append(&enabled);
        header.append(&focus);
        header.append(&up);
        header.append(&down);
        header.append(&delete);
        root.append(&header);

        let editor = crate::ui::json_view(&stage.body, true);
        editor.set_show_line_numbers(false);
        let editor_scroller = gtk::ScrolledWindow::builder()
            .child(&editor)
            .propagate_natural_height(true)
            .max_content_height(360)
            .min_content_height(72)
            .hexpand(true)
            .build();
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(4)
            .build();
        let editor_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        editor_col.append(&editor_scroller);
        editor_col.append(&error);

        let preview_title = gtk::Label::builder()
            .label("Output")
            .xalign(0.0)
            .css_classes(["viti-count"])
            .margin_start(8)
            .margin_top(4)
            .build();
        let preview_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        preview_box.set_margin_start(4);
        preview_box.set_margin_end(4);
        preview_box.set_margin_bottom(4);
        let preview_scroller = gtk::ScrolledWindow::builder()
            .child(&preview_box)
            .propagate_natural_height(true)
            .max_content_height(360)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let preview_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        preview_col.add_css_class("viti-stage-preview");
        preview_col.append(&preview_title);
        preview_col.append(&preview_scroller);
        let body = gtk::Paned::new(gtk::Orientation::Horizontal);
        body.set_start_child(Some(&editor_col));
        body.set_end_child(Some(&preview_col));
        body.set_shrink_start_child(false);
        body.set_shrink_end_child(false);
        body.set_resize_start_child(true);
        body.set_resize_end_child(true);
        body.set_position(520);
        root.append(&body);

        let card = Rc::new(StageCard {
            root,
            number,
            operator: operator.clone(),
            enabled: enabled.clone(),
            editor,
            error,
            preview_title,
            preview_box,
            preview_docs: RefCell::new(Vec::new()),
            generation: Cell::new(0),
            on_preview: RefCell::new(None),
            last_template: RefCell::new(
                pipeline::stage_info(&stage.operator)
                    .map(|s| s.template.to_string())
                    .unwrap_or_default(),
            ),
        });
        card.set_enabled_look(stage.enabled);
        let _ = app;
        // Buttons are wired by the pane (it owns the card list).
        card.root.set_widget_name("stage");
        for (btn, name) in [
            (&delete, "delete"),
            (&up, "up"),
            (&down, "down"),
            (&focus, "focus"),
        ] {
            btn.set_widget_name(name);
        }
        card
    }

    fn stage(&self) -> Stage {
        Stage {
            operator: STAGES
                .get(self.operator.selected() as usize)
                .map(|s| s.name.to_string())
                .unwrap_or_else(|| "$match".into()),
            body: crate::ui::buffer_text(&self.editor.buffer()),
            enabled: self.enabled.is_active(),
        }
    }

    fn set_enabled_look(&self, on: bool) {
        if on {
            self.root.remove_css_class("viti-stage-disabled");
        } else {
            self.root.add_css_class("viti-stage-disabled");
        }
    }

    fn set_error(&self, msg: Option<&str>) {
        match msg {
            Some(m) => {
                self.error.set_text(m);
                self.error.set_visible(true);
            }
            None => self.error.set_visible(false),
        }
    }

    fn clear_preview(&self, note: &str) {
        while let Some(c) = self.preview_box.first_child() {
            self.preview_box.remove(&c);
        }
        self.preview_docs.borrow_mut().clear();
        self.preview_title.set_text(note);
        if let Some(f) = self.on_preview.borrow().clone() {
            f();
        }
    }

    fn set_preview(&self, app: &Rc<App>, docs: Vec<Document>, elapsed_ms: u128) {
        while let Some(c) = self.preview_box.first_child() {
            self.preview_box.remove(&c);
        }
        self.preview_title.set_text(&if docs.is_empty() {
            format!("Output: no documents · {elapsed_ms} ms")
        } else if docs.len() as i64 >= PREVIEW_N {
            format!("Output: first {} · {elapsed_ms} ms", docs.len())
        } else {
            format!(
                "Output: {} document{} · {elapsed_ms} ms",
                docs.len(),
                if docs.len() == 1 { "" } else { "s" }
            )
        });
        for d in &docs {
            self.preview_box.append(&preview_row(app, d));
        }
        *self.preview_docs.borrow_mut() = docs;
        if let Some(f) = self.on_preview.borrow().clone() {
            f();
        }
    }
}

/// Collapsed one-document-per-card JSON list for the results.
fn results_factory(pane: &Rc<AggregationPane>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("viti-doc-json");
        let view = crate::ui::json_view("", false);
        view.set_can_focus(false);
        view.set_focusable(false);
        frame.append(&view);
        item.set_child(Some(&frame));
    });
    let weak = Rc::downgrade(pane);
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let frame = item.child().and_downcast::<gtk::Box>().unwrap();
        let view = frame
            .first_child()
            .and_downcast::<sourceview5::View>()
            .unwrap();
        let Some(pane) = weak.upgrade() else { return };
        let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
            return;
        };
        let idx = *obj.borrow::<usize>();
        let docs = pane.results.borrow();
        if let Some(doc) = docs.get(idx) {
            view.buffer()
                .set_text(&crate::ui::documents::json::collapsed_text(doc));
        }
    });
    factory
}

impl AggregationPane {
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
        // ----- toolbar -----
        let mode_btn = gtk::ToggleButton::builder()
            .icon_name("text-x-generic-symbolic")
            .tooltip_text(tip(
                "Text mode: edit the whole pipeline as one array",
                "agg.text-mode",
            ))
            .focus_on_click(false)
            .build();
        let name_label = gtk::Label::builder()
            .label("Untitled pipeline")
            .xalign(0.0)
            .css_classes(["heading"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let saved_popover = gtk::Popover::new();
        let saved_list = gtk::ListBox::new();
        saved_list.add_css_class("boxed-list");
        saved_list.set_selection_mode(gtk::SelectionMode::None);
        let saved_scroller = gtk::ScrolledWindow::builder()
            .child(&saved_list)
            .propagate_natural_height(true)
            .max_content_height(420)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        saved_scroller.set_size_request(420, -1);
        saved_popover.set_child(Some(&saved_scroller));
        let saved_btn = gtk::MenuButton::builder()
            .icon_name("document-open-recent-symbolic")
            .tooltip_text(tip("Saved pipelines", "agg.open"))
            .focus_on_click(false)
            .css_classes(["flat"])
            .popover(&saved_popover)
            .build();
        let save_btn = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(tip("Save pipeline", "agg.save"))
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let status = gtk::Label::builder()
            .xalign(1.0)
            .hexpand(true)
            .css_classes(["viti-count"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let options_btn = gtk::ToggleButton::builder()
            .icon_name("pan-down-symbolic")
            .tooltip_text("Options: collation, max time, allow disk use (Alt+O)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let preview_btn = gtk::ToggleButton::builder()
            .label("Preview")
            .tooltip_text(tip(
                "Run each stage's preview while typing",
                "agg.preview-toggle",
            ))
            .active(settings.agg_auto_preview)
            .focus_on_click(false)
            .build();
        let run_btn = gtk::Button::builder()
            .label("Run")
            .tooltip_text(tip("Run the pipeline", "agg.run"))
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
        let actions = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        let mut acts: Vec<(gio::SimpleAction, &str)> = Vec::new();
        for (name, title, id) in [
            ("add-stage", "Add stage", "agg.add-stage"),
            ("clear", "Clear all stages", "agg.clear"),
            (
                "edit-external",
                "Edit pipeline in external editor",
                "agg.edit-external",
            ),
            (
                "create-view",
                "Create a view from this pipeline…",
                "agg.create-view",
            ),
            (
                "export-language",
                "Export pipeline to language…",
                "agg.export-language",
            ),
            ("explain", "Explain pipeline", "agg.explain"),
        ] {
            let a = gio::SimpleAction::new(name, None);
            actions.add_action(&a);
            menu.append(Some(&tip(title, id)), Some(&format!("agg.{name}")));
            acts.push((a, name));
        }
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .focus_on_click(false)
            .menu_model(&menu)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&mode_btn);
        bar.append(&name_label);
        bar.append(&saved_btn);
        bar.append(&save_btn);
        bar.append(&status);
        bar.append(&spinner);
        bar.append(&options_btn);
        bar.append(&preview_btn);
        bar.append(&run_btn);
        bar.append(&stop_btn);
        bar.append(&menu_btn);

        let opts_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        opts_row.set_margin_start(8);
        opts_row.set_margin_end(8);
        opts_row.set_margin_bottom(6);
        let lbl = |t: &str| {
            gtk::Label::builder()
                .label(t)
                .css_classes(["dim-label"])
                .build()
        };
        let collation = gtk::Entry::builder()
            .placeholder_text("{ locale: 'en' }")
            .hexpand(true)
            .css_classes(["viti-mono"])
            .build();
        let max_time = gtk::SpinButton::with_range(0.0, 1e9, 1000.0);
        let disk_use = gtk::CheckButton::with_label("Allow disk use");
        opts_row.append(&lbl("Collation"));
        opts_row.append(&collation);
        opts_row.append(&lbl("Max time (ms)"));
        opts_row.append(&max_time);
        opts_row.append(&disk_use);
        let options = gtk::Revealer::builder()
            .child(&opts_row)
            .reveal_child(false)
            .build();
        options_btn
            .bind_property("active", &options, "reveal-child")
            .bidirectional()
            .sync_create()
            .build();
        options_btn.connect_toggled(|b| {
            b.set_icon_name(if b.is_active() {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            });
        });

        // ----- stage cards / text -----
        let stages_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        stages_box.set_margin_start(8);
        stages_box.set_margin_end(8);
        stages_box.set_margin_bottom(8);
        let empty_hint = gtk::Label::builder()
            .label("No stages yet — press a or click Add stage")
            .css_classes(["dim-label"])
            .margin_top(24)
            .build();
        let add_btn = gtk::Button::builder()
            .label("Add stage")
            .tooltip_text(tip("Add stage", "agg.add-stage"))
            .focus_on_click(false)
            .halign(gtk::Align::Start)
            .build();
        let stages_col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        stages_col.append(&stages_box);
        stages_col.append(&empty_hint);
        let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        add_row.set_margin_start(8);
        add_row.set_margin_bottom(8);
        add_row.append(&add_btn);
        stages_col.append(&add_row);
        let stages_scroller = gtk::ScrolledWindow::builder()
            .child(&stages_col)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        // Cards get their natural height (editor + preview), not their minimum.
        if let Some(vp) = stages_scroller.child().and_downcast::<gtk::Viewport>() {
            vp.set_vscroll_policy(gtk::ScrollablePolicy::Natural);
        }
        let text_view = crate::ui::json_view("[\n  \n]", true);
        let text_error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(12)
            .margin_end(12)
            .build();
        let text_col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text_col.append(
            &gtk::ScrolledWindow::builder()
                .child(&text_view)
                .vexpand(true)
                .build(),
        );
        text_col.append(&text_error);
        let mode_stack = gtk::Stack::new();
        mode_stack.set_vexpand(true);
        mode_stack.add_named(&stages_scroller, Some("stages"));
        mode_stack.add_named(&text_col, Some("text"));

        // ----- results -----
        let results_model = gio::ListStore::new::<BoxedAnyObject>();
        let results_selection = gtk::SingleSelection::new(Some(results_model.clone()));
        results_selection.set_autoselect(false);
        results_selection.set_can_unselect(false);
        let results_list = gtk::ListView::new(
            Some(results_selection.clone()),
            None::<gtk::SignalListItemFactory>,
        );
        results_list.add_css_class("navigation-sidebar");
        results_list.set_can_focus(false);
        let results_status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(tip("Previous page", "agg.prev-page"))
            .focus_on_click(false)
            .build();
        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text(tip("Next page", "agg.next-page"))
            .focus_on_click(false)
            .build();
        let pager = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pager.add_css_class("linked");
        pager.append(&prev_btn);
        pager.append(&next_btn);
        let close_results = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Hide results")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let results_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        results_bar.set_margin_start(8);
        results_bar.set_margin_end(8);
        results_bar.set_margin_top(4);
        results_bar.set_margin_bottom(4);
        results_bar.append(&results_status);
        results_bar.append(&pager);
        results_bar.append(&close_results);
        let results_root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        results_root.add_css_class("viti-docs");
        results_root.add_css_class("viti-agg-results");
        results_root.append(&results_bar);
        results_root.append(
            &gtk::ScrolledWindow::builder()
                .child(&results_list)
                .vexpand(true)
                .build(),
        );
        results_root.set_visible(false);

        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_start_child(Some(&mode_stack));
        paned.set_end_child(Some(&results_root));
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(true);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_vexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-aggregation");
        root.append(&bar);
        root.append(&options);
        root.append(&paned);
        root.insert_action_group("agg", Some(&actions));

        let pane = Rc::new(Self {
            root,
            name_label,
            mode_btn: mode_btn.clone(),
            preview_btn: preview_btn.clone(),
            run_btn: run_btn.clone(),
            stop_btn: stop_btn.clone(),
            spinner,
            status,
            saved_btn: saved_btn.clone(),
            saved_popover,
            saved_list,
            options,
            collation,
            max_time,
            disk_use,
            mode_stack,
            stages_box,
            stages_scroller,
            empty_hint,
            text_view,
            text_error,
            paned,
            results_root,
            results_list: results_list.clone(),
            results_model,
            results_selection: results_selection.clone(),
            results_status,
            prev_btn: prev_btn.clone(),
            next_btn: next_btn.clone(),
            conn,
            ns,
            app: Rc::downgrade(app),
            cards: RefCell::new(Vec::new()),
            cursor: Cell::new(0),
            results_focused: Cell::new(false),
            results: RefCell::new(Vec::new()),
            page: Cell::new(0),
            loaded: RefCell::new(None),
            inflight: RefCell::new(None),
            generation: Cell::new(0),
            preview_debounce: RefCell::new(None),
            preview_dirty_from: Cell::new(usize::MAX),
            me: RefCell::new(Weak::new()),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);
        results_list.set_factory(Some(&results_factory(&pane)));

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
            add_btn.connect_clicked(move |_| p.add_stage());
        }
        {
            let p = pane.clone();
            mode_btn.connect_toggled(move |b| p.set_text_mode(b.is_active()));
        }
        {
            let p = pane.clone();
            preview_btn.connect_toggled(move |b| {
                if let Some(app) = p.app() {
                    app.config.borrow_mut().settings.agg_auto_preview = b.is_active();
                    app.schedule_save();
                }
                if b.is_active() {
                    p.schedule_previews(0);
                }
            });
        }
        {
            let p = pane.clone();
            save_btn.connect_clicked(move |_| p.save());
        }
        {
            let p = pane.clone();
            saved_btn.set_create_popup_func(move |_| p.fill_saved());
        }
        for (a, name) in acts {
            let p = pane.clone();
            match name {
                "add-stage" => a.connect_activate(move |_, _| p.add_stage()),
                "clear" => a.connect_activate(move |_, _| p.clear()),
                "edit-external" => a.connect_activate(move |_, _| p.edit_external()),
                "create-view" => a.connect_activate(move |_, _| p.create_view()),
                "export-language" => a.connect_activate(move |_, _| p.export_language()),
                _ => a.connect_activate(move |_, _| p.explain()),
            };
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
            close_results.connect_clicked(move |_| p.hide_results());
        }
        {
            let p = pane.clone();
            results_list.connect_activate(move |_, _| p.peek());
        }
        {
            // Clicking results makes j/k/o act on them; clicking a card, on cards.
            let p = pane.clone();
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |_, _, _, _| {
                p.results_focused.set(true);
                p.root.grab_focus();
            });
            results_list.add_controller(click);
        }
        {
            let p = pane.clone();
            pane.text_view.buffer().connect_changed(move |_| {
                p.text_error.set_visible(false);
            });
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    fn me(&self) -> Option<Rc<Self>> {
        self.me.borrow().upgrade()
    }

    fn text_mode(&self) -> bool {
        self.mode_stack.visible_child_name().as_deref() == Some("text")
    }

    // ----- pipeline model ---------------------------------------------------

    /// The pipeline as the cards (or the text editor) currently hold it.
    pub fn pipeline(&self) -> Result<Pipeline, String> {
        if self.text_mode() {
            Pipeline::from_text(&crate::ui::buffer_text(&self.text_view.buffer()))
        } else {
            Ok(Pipeline {
                stages: self.cards.borrow().iter().map(|c| c.stage()).collect(),
            })
        }
    }

    /// Enabled stages as BSON plus the options row, or `None` after showing
    /// the error on the offending card / text editor.
    pub fn pipeline_for_run(&self) -> Option<(Vec<Document>, AggOpts)> {
        let p = match self.pipeline() {
            Ok(p) => p,
            Err(e) => {
                self.text_error.set_text(&e);
                self.text_error.set_visible(true);
                return None;
            }
        };
        let docs = match p.documents() {
            Ok(d) => d,
            Err((i, e)) => {
                if let Some(c) = self.cards.borrow().get(i) {
                    c.set_error(Some(&e.to_string()));
                }
                if let Some(app) = self.app() {
                    app.toast(&format!("Stage {}: {e}", i + 1));
                }
                return None;
            }
        };
        let opts = match self.opts() {
            Ok(o) => o,
            Err(e) => {
                if let Some(app) = self.app() {
                    app.toast(&format!("collation: {e}"));
                }
                self.options.set_reveal_child(true);
                self.collation.grab_focus();
                return None;
            }
        };
        Some((docs, opts))
    }

    fn opts(&self) -> Result<AggOpts, ejson::ParseError> {
        let collation = self.collation.text();
        let mt = self.max_time.value() as u64;
        Ok(AggOpts {
            collation: if collation.trim().is_empty() {
                None
            } else {
                Some(ejson::parse_document(&collation)?)
            },
            allow_disk_use: self.disk_use.is_active(),
            max_time_ms: (mt > 0).then_some(mt),
        })
    }

    /// Replace every card with `p` (also leaves text mode).
    pub fn set_pipeline(&self, p: &Pipeline) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        for c in self.cards.borrow().iter() {
            self.stages_box.remove(&c.root);
        }
        self.cards.borrow_mut().clear();
        for s in &p.stages {
            let card = StageCard::new(&app, s);
            self.wire_card(&me, &card);
            self.stages_box.append(&card.root);
            self.cards.borrow_mut().push(card);
        }
        if self.text_mode() {
            self.text_view.buffer().set_text(&p.to_text());
        }
        self.cursor.set(0);
        self.renumber();
        self.schedule_previews(0);
    }

    fn renumber(&self) {
        let cards = self.cards.borrow();
        for (i, c) in cards.iter().enumerate() {
            c.number.set_text(&format!("{}", i + 1));
            if i == self.cursor.get() {
                c.root.add_css_class("viti-stage-current");
            } else {
                c.root.remove_css_class("viti-stage-current");
            }
        }
        self.empty_hint.set_visible(cards.is_empty());
    }

    fn wire_card(&self, me: &Rc<Self>, card: &Rc<StageCard>) {
        // Header buttons find the card by identity when clicked.
        let header = card.root.first_child().and_downcast::<gtk::Box>().unwrap();
        let mut child = header.first_child();
        while let Some(w) = child {
            if let Some(b) = w.downcast_ref::<gtk::Button>() {
                let name = b.widget_name().to_string();
                let p = me.clone();
                let c = Rc::downgrade(card);
                b.connect_clicked(move |_| {
                    let Some(c) = c.upgrade() else { return };
                    let Some(i) = p.index_of(&c) else { return };
                    p.cursor.set(i);
                    p.renumber();
                    match name.as_str() {
                        "delete" => p.delete_stage(),
                        "up" => p.move_stage(-1),
                        "down" => p.move_stage(1),
                        _ => p.focus_mode(),
                    }
                });
            }
            child = w.next_sibling();
        }
        {
            let p = me.clone();
            let c = Rc::downgrade(card);
            card.enabled.connect_active_notify(move |s| {
                let Some(c) = c.upgrade() else { return };
                c.set_enabled_look(s.is_active());
                if let Some(i) = p.index_of(&c) {
                    p.schedule_previews(i);
                }
            });
        }
        {
            let p = me.clone();
            let c = Rc::downgrade(card);
            card.operator.connect_selected_notify(move |dd| {
                let Some(c) = c.upgrade() else { return };
                let Some(info) = STAGES.get(dd.selected() as usize) else {
                    return;
                };
                dd.set_tooltip_text(Some(info.help));
                // Swap in the new template unless the body was hand-written.
                let body = crate::ui::buffer_text(&c.editor.buffer());
                let untouched =
                    body.trim().is_empty() || body.trim() == c.last_template.borrow().trim();
                if untouched {
                    c.editor.buffer().set_text(info.template);
                }
                *c.last_template.borrow_mut() = info.template.to_string();
                if let Some(i) = p.index_of(&c) {
                    p.schedule_previews(i);
                }
            });
        }
        {
            let p = me.clone();
            let c = Rc::downgrade(card);
            card.editor.buffer().connect_changed(move |_| {
                let Some(c) = c.upgrade() else { return };
                c.set_error(None);
                if let Some(i) = p.index_of(&c) {
                    p.schedule_previews(i);
                }
            });
        }
        {
            // Typing in a card makes it the current one.
            let p = me.clone();
            let c = Rc::downgrade(card);
            let focus = gtk::EventControllerFocus::new();
            focus.connect_enter(move |_| {
                let Some(c) = c.upgrade() else { return };
                if let Some(i) = p.index_of(&c) {
                    p.results_focused.set(false);
                    p.cursor.set(i);
                    p.renumber();
                }
            });
            card.editor.add_controller(focus);
        }
        {
            // Clicking anywhere on a card selects it for the vi keys.
            let p = me.clone();
            let c = Rc::downgrade(card);
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |_, _, _, _| {
                let Some(c) = c.upgrade() else { return };
                if let Some(i) = p.index_of(&c) {
                    p.results_focused.set(false);
                    p.cursor.set(i);
                    p.renumber();
                }
            });
            card.root.add_controller(click);
        }
    }

    fn index_of(&self, card: &Rc<StageCard>) -> Option<usize> {
        self.cards.borrow().iter().position(|c| Rc::ptr_eq(c, card))
    }

    fn current_card(&self) -> Option<Rc<StageCard>> {
        self.cards.borrow().get(self.cursor.get()).cloned()
    }

    // ----- stage actions ----------------------------------------------------

    /// `a`: a new `$match` after the current card; its editor takes focus.
    pub fn add_stage(&self) {
        self.add_stage_with(Stage::new("$match"));
    }

    fn add_stage_with(&self, stage: Stage) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        if self.text_mode() {
            self.leave_text_mode();
            if self.text_mode() {
                return;
            }
        }
        let card = StageCard::new(&app, &stage);
        self.wire_card(&me, &card);
        let at = if self.cards.borrow().is_empty() {
            0
        } else {
            self.cursor.get() + 1
        };
        {
            let mut cards = self.cards.borrow_mut();
            let after = if at == 0 {
                None
            } else {
                cards.get(at - 1).map(|c| c.root.clone())
            };
            self.stages_box
                .insert_child_after(&card.root, after.as_ref());
            cards.insert(at, card.clone());
        }
        self.cursor.set(at);
        self.renumber();
        card.editor.grab_focus();
        // Put the caret inside the template's braces.
        let buffer = card.editor.buffer();
        let mut it = buffer.start_iter();
        if buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .starts_with("{\n  ")
        {
            it.set_line(1);
            it.forward_chars(2);
        }
        buffer.place_cursor(&it);
        self.schedule_previews(at);
    }

    pub fn delete_stage(&self) {
        let Some(card) = self.current_card() else {
            return;
        };
        let i = self.cursor.get();
        self.stages_box.remove(&card.root);
        self.cards.borrow_mut().remove(i);
        let n = self.cards.borrow().len();
        if n > 0 {
            self.cursor.set(i.min(n - 1));
        } else {
            self.cursor.set(0);
        }
        self.renumber();
        self.schedule_previews(i);
        if let Some(app) = self.app() {
            app.toast(&format!("Removed stage {}", i + 1));
        }
    }

    /// `J` / `K`: swap the current card with its neighbour.
    pub fn move_stage(&self, delta: i64) {
        let i = self.cursor.get();
        let n = self.cards.borrow().len();
        let j = i as i64 + delta;
        if n == 0 || j < 0 || j as usize >= n {
            return;
        }
        let j = j as usize;
        {
            let mut cards = self.cards.borrow_mut();
            cards.swap(i, j);
            // Re-place the moved widget in the box.
            let moved = cards[j].root.clone();
            self.stages_box.remove(&moved);
            let after = if j == 0 {
                None
            } else {
                Some(cards[j - 1].root.clone())
            };
            self.stages_box.insert_child_after(&moved, after.as_ref());
        }
        self.cursor.set(j);
        self.renumber();
        self.schedule_previews(i.min(j));
    }

    /// `t`: enable / disable the current stage.
    pub fn toggle_stage(&self) {
        if let Some(c) = self.current_card() {
            c.enabled.set_active(!c.enabled.is_active());
        }
    }

    /// `e`: put the caret in the current card's editor (or the text editor).
    pub fn edit_stage(&self) {
        if self.text_mode() {
            self.text_view.grab_focus();
        } else if let Some(c) = self.current_card() {
            c.editor.grab_focus();
        } else {
            self.add_stage();
        }
    }

    pub fn move_cursor(&self, delta: i64) {
        if self.results_focused.get() {
            let n = self.results_model.n_items() as i64;
            if n == 0 {
                return;
            }
            let cur = self.results_selection.selected();
            let cur = if cur == gtk::INVALID_LIST_POSITION {
                -1
            } else {
                cur as i64
            };
            let next = if cur < 0 {
                0
            } else {
                (cur + delta).clamp(0, n - 1)
            };
            self.results_selection.set_selected(next as u32);
            self.results_list
                .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
            return;
        }
        let n = self.cards.borrow().len() as i64;
        if n == 0 {
            return;
        }
        let next = (self.cursor.get() as i64 + delta).clamp(0, n - 1) as usize;
        self.cursor.set(next);
        self.renumber();
        if let Some(c) = self.current_card() {
            // Keep the current card in view.
            let adj = self.stages_scroller.vadjustment();
            let y = c
                .root
                .compute_point(&self.stages_box, &gtk::graphene::Point::new(0.0, 0.0))
                .map(|p| p.y() as f64)
                .unwrap_or(0.0);
            let h = c.root.height() as f64;
            if y < adj.value() {
                adj.set_value(y);
            } else if y + h > adj.value() + adj.page_size() {
                adj.set_value((y + h - adj.page_size()).max(0.0));
            }
        }
    }

    pub fn top(&self) {
        if self.results_focused.get() {
            if self.results_model.n_items() > 0 {
                self.results_selection.set_selected(0);
            }
            return;
        }
        self.cursor.set(0);
        self.renumber();
        self.stages_scroller.vadjustment().set_value(0.0);
    }

    pub fn bottom(&self) {
        if self.results_focused.get() {
            let n = self.results_model.n_items();
            if n > 0 {
                self.results_selection.set_selected(n - 1);
            }
            return;
        }
        let n = self.cards.borrow().len();
        if n > 0 {
            self.cursor.set(n - 1);
            self.renumber();
            let adj = self.stages_scroller.vadjustment();
            adj.set_value(adj.upper());
        }
    }

    /// `Ctrl+J`: the vi keys act on the results, or back on the cards.
    pub fn toggle_results_focus(&self) {
        if !self.results_root.is_visible() {
            if let Some(app) = self.app() {
                app.toast("Run the pipeline first (R)");
            }
            return;
        }
        let to_results = !self.results_focused.get();
        self.results_focused.set(to_results);
        if to_results
            && self.results_selection.selected() == gtk::INVALID_LIST_POSITION
            && self.results_model.n_items() > 0
        {
            self.results_selection.set_selected(0);
        }
        if let Some(app) = self.app() {
            app.toast(if to_results {
                "Results: j/k move, o opens"
            } else {
                "Stages"
            });
        }
        self.root.grab_focus();
    }

    /// `C`: remove every stage.
    pub fn clear(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let n = if self.text_mode() {
            1
        } else {
            self.cards.borrow().len()
        };
        if n == 0 {
            return;
        }
        crate::ui::confirm(
            &app.window.clone(),
            "Clear the pipeline?",
            "Every stage is removed. Saved pipelines are not affected.",
            "Clear",
            true,
            move || {
                me.mode_btn.set_active(false);
                me.set_pipeline(&Pipeline::default());
                *me.loaded.borrow_mut() = None;
                me.name_label.set_text("Untitled pipeline");
                me.hide_results();
            },
        );
    }

    // ----- text mode --------------------------------------------------------

    pub fn toggle_text_mode(&self) {
        self.mode_btn.set_active(!self.mode_btn.is_active());
    }

    fn set_text_mode(&self, on: bool) {
        if on == self.text_mode() {
            return;
        }
        if on {
            let p = Pipeline {
                stages: self.cards.borrow().iter().map(|c| c.stage()).collect(),
            };
            let dropped = p.disabled_count();
            self.text_view.buffer().set_text(&p.to_text());
            self.text_error.set_visible(false);
            self.mode_stack.set_visible_child_name("text");
            if dropped > 0
                && let Some(app) = self.app()
            {
                app.toast(&format!(
                    "{dropped} disabled stage{} left out of the text",
                    if dropped == 1 { "" } else { "s" }
                ));
            }
        } else {
            self.leave_text_mode();
        }
    }

    /// Parse the text back into cards; on error stay in text mode.
    fn leave_text_mode(&self) {
        match Pipeline::from_text(&crate::ui::buffer_text(&self.text_view.buffer())) {
            Ok(p) => {
                self.mode_stack.set_visible_child_name("stages");
                self.set_pipeline(&p);
                if self.mode_btn.is_active() {
                    self.mode_btn.set_active(false);
                }
            }
            Err(e) => {
                self.text_error.set_text(&e);
                self.text_error.set_visible(true);
                if !self.mode_btn.is_active() {
                    self.mode_btn.set_active(true);
                }
                self.text_view.grab_focus();
            }
        }
    }

    /// `Ctrl+E`: the whole pipeline in the external editor.
    pub fn edit_external(&self) {
        let Some(app) = self.app() else { return };
        let text = if self.text_mode() {
            crate::ui::buffer_text(&self.text_view.buffer())
        } else {
            match self.pipeline() {
                Ok(p) => p.to_text(),
                Err(e) => {
                    app.toast(&e);
                    return;
                }
            }
        };
        let settings = app.config.borrow().settings.clone();
        if let Err(e) = app.editor_pane.open(
            &settings,
            crate::ui::editor_pane::JobKind::Pipeline {
                conn: self.conn,
                ns: self.ns.clone(),
            },
            text,
            &format!("{} — pipeline", self.ns),
        ) {
            app.toast_error("open editor", &e);
        }
    }

    /// Back from the external editor: the array becomes the cards.
    pub fn load_text(&self, text: &str) -> Result<(), String> {
        let p = Pipeline::from_text(text)?;
        if self.text_mode() {
            self.text_view.buffer().set_text(text);
            self.text_error.set_visible(false);
        } else {
            self.set_pipeline(&p);
        }
        Ok(())
    }

    // ----- previews ---------------------------------------------------------

    /// Debounced: previews for card `from` and every later card.
    fn schedule_previews(&self, from: usize) {
        if !self.preview_btn.is_active() || self.text_mode() {
            return;
        }
        let Some(me) = self.me() else { return };
        self.preview_dirty_from
            .set(self.preview_dirty_from.get().min(from));
        if let Some(src) = self.preview_debounce.borrow_mut().take() {
            src.remove();
        }
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(PREVIEW_DEBOUNCE_MS),
            move || {
                me.preview_debounce.borrow_mut().take();
                let from = me.preview_dirty_from.replace(usize::MAX);
                me.run_previews(from);
            },
        );
        *self.preview_debounce.borrow_mut() = Some(id);
    }

    fn run_previews(&self, from: usize) {
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let cards: Vec<Rc<StageCard>> = self.cards.borrow().clone();
        let stages: Vec<Stage> = cards.iter().map(|c| c.stage()).collect();
        let opts = self.opts().unwrap_or_default();
        // Parse once; the first bad card blocks everything after it.
        let mut parsed: Vec<Option<Document>> = Vec::new();
        let mut first_error: Option<usize> = None;
        for (i, s) in stages.iter().enumerate() {
            if !s.enabled {
                parsed.push(None);
                continue;
            }
            match s.parse() {
                Ok(d) => parsed.push(Some(d)),
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(i);
                    }
                    cards[i].set_error(Some(&e.to_string()));
                    parsed.push(None);
                }
            }
        }
        let write_at = stages.iter().position(|s| s.enabled && s.is_write());
        for i in from..cards.len() {
            let card = &cards[i];
            card.generation.set(card.generation.get() + 1);
            let generation = card.generation.get();
            if !stages[i].enabled {
                card.clear_preview("Disabled — passes documents through");
                continue;
            }
            if let Some(e) = first_error
                && e <= i
            {
                if e < i {
                    card.clear_preview(&format!("Fix stage {} first", e + 1));
                } else {
                    card.clear_preview("Output");
                }
                continue;
            }
            if let Some(w) = write_at
                && w <= i
            {
                card.clear_preview(if w == i {
                    "Writes to a collection — run the pipeline to execute it"
                } else {
                    "Nothing after a $out / $merge stage"
                });
                continue;
            }
            let mut pipeline: Vec<Document> = parsed[..=i].iter().flatten().cloned().collect();
            pipeline.push(doc! { "$limit": PREVIEW_N });
            let client = conn.client.clone();
            let ns = self.ns.clone();
            let ctx = OpCtx::new(app.max_time_ms().min(15_000));
            let opts = opts.clone();
            let card = card.clone();
            let app = app.clone();
            card.preview_title.set_text("Output: running…");
            glib::spawn_future_local(async move {
                let started = std::time::Instant::now();
                let r = crate::rt::io(async move {
                    ops::aggregate(&client, &ns, pipeline, &opts, &ctx).await
                })
                .await;
                if card.generation.get() != generation {
                    return;
                }
                match r {
                    Ok(docs) => card.set_preview(&app, docs, started.elapsed().as_millis()),
                    Err(e) => {
                        let msg = format!("{e:#}");
                        // The server's message, without our "aggregate on x failed" prefix.
                        let msg = msg.rsplit(": ").next().unwrap_or(&msg).to_string();
                        card.set_error(Some(&msg));
                        card.clear_preview("Output");
                    }
                }
            });
        }
    }

    pub fn toggle_preview(&self) {
        self.preview_btn.set_active(!self.preview_btn.is_active());
    }

    // ----- run --------------------------------------------------------------

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
                app.toast("Pipeline cancelled");
            }
            self.set_busy(false);
        }
    }

    /// `R`: run the enabled stages; results page below the cards. Pipelines
    /// with `$out` / `$merge` are confirmed first and run as written.
    pub fn run(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some((docs, opts)) = self.pipeline_for_run() else {
            return;
        };
        let pipeline = Pipeline::from_documents(&docs).unwrap_or_default();
        if pipeline.has_write_stage() {
            if app.write_guard().is_err() {
                return;
            }
            let target = pipeline.write_target().unwrap_or_default();
            crate::ui::confirm(
                &app.window.clone(),
                "Run a pipeline that writes?",
                &format!(
                    "This pipeline ends with {target}. The target collection is replaced ($out) or merged into ($merge)."
                ),
                "Run",
                true,
                move || me.execute(docs.clone(), opts.clone(), true),
            );
            return;
        }
        self.page.set(0);
        self.execute(docs, opts, false);
    }

    fn execute(&self, mut docs: Vec<Document>, opts: AggOpts, writes: bool) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            app.toast("Not connected");
            return;
        };
        if let Some((_, h)) = self.inflight.borrow_mut().take() {
            h.abort();
        }
        if !writes {
            let skip = self.page.get() * PAGE;
            if skip > 0 {
                docs.push(doc! { "$skip": skip as i64 });
            }
            docs.push(doc! { "$limit": PAGE as i64 });
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let ctx = OpCtx::new(app.max_time_ms());
        let ctx2 = ctx.clone();
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let (tx, rx) = async_channel::bounded(1);
        let handle = crate::rt::spawn(async move {
            let started = std::time::Instant::now();
            let r = ops::aggregate(&client, &ns, docs, &opts, &ctx2).await;
            let _ = tx.send((r, started.elapsed())).await;
        });
        *self.inflight.borrow_mut() = Some((ctx, handle.abort_handle()));
        self.set_busy(true);
        self.status.set_text("Running…");
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
                Ok(docs) => {
                    let ms = elapsed.as_millis();
                    if writes {
                        me.status.set_text(&format!("Pipeline ran in {ms} ms"));
                        if let Some(app) = me.app() {
                            let msg = format!(
                                "Pipeline on {} ran in {ms} ms and wrote its output",
                                me.ns
                            );
                            app.toast(&msg);
                            app.notify_if_unfocused("agg", "Pipeline finished", &msg);
                            app.after_namespace_change(me.conn, &me.ns.db.clone());
                        }
                        me.hide_results();
                    } else {
                        me.status.set_text(&format!("{ms} ms"));
                        me.show_results(docs);
                    }
                }
                Err(e) => {
                    me.status.set_text("");
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("aggregate on {}", me.ns), &e);
                    }
                }
            }
        });
    }

    fn show_results(&self, docs: Vec<Document>) {
        let n = docs.len() as u64;
        let start = self.page.get() * PAGE;
        self.results_status.set_text(&if n == 0 {
            if start == 0 {
                "No documents".to_string()
            } else {
                "No more documents".to_string()
            }
        } else {
            format!(
                "Results {}–{}",
                crate::ui::thousands(start + 1),
                crate::ui::thousands(start + n)
            )
        });
        self.next_btn.set_sensitive(n >= PAGE);
        self.prev_btn.set_sensitive(self.page.get() > 0);
        *self.results.borrow_mut() = docs;
        self.results_model.remove_all();
        let items: Vec<BoxedAnyObject> = (0..n as usize).map(BoxedAnyObject::new).collect();
        self.results_model.extend_from_slice(&items);
        if !self.results_root.is_visible() {
            self.results_root.set_visible(true);
            let h = self.paned.height();
            if h > 0 {
                self.paned.set_position(h * 45 / 100);
            }
        }
        if n > 0 {
            self.results_selection.set_selected(0);
        }
    }

    fn hide_results(&self) {
        self.results_root.set_visible(false);
        self.results_focused.set(false);
    }

    pub fn next_page(&self) {
        if !self.results_root.is_visible() || !self.next_btn.is_sensitive() {
            return;
        }
        if let Some((docs, opts)) = self.pipeline_for_run() {
            self.page.set(self.page.get() + 1);
            self.execute(docs, opts, false);
        }
    }

    pub fn prev_page(&self) {
        if self.page.get() == 0 {
            return;
        }
        if let Some((docs, opts)) = self.pipeline_for_run() {
            self.page.set(self.page.get() - 1);
            self.execute(docs, opts, false);
        }
    }

    /// `o`: the current result document (or the current card's first preview).
    pub fn peek(&self) {
        let Some(app) = self.app() else { return };
        let doc = if self.results_focused.get()
            || !self.results_root.is_visible() && self.cards.borrow().is_empty()
        {
            let s = self.results_selection.selected();
            if s == gtk::INVALID_LIST_POSITION {
                return;
            }
            self.results.borrow().get(s as usize).cloned()
        } else {
            self.current_card()
                .and_then(|c| c.preview_docs.borrow().first().cloned())
        };
        let Some(doc) = doc else { return };
        let title = doc
            .get("_id")
            .map(ejson::id_display)
            .unwrap_or_else(|| "document".into());
        crate::ui::peek_document(&app, &title, &ejson::pretty(&doc, Mode::Relaxed));
    }

    // ----- focus mode -------------------------------------------------------

    /// `f`: one stage, full size: input documents | editor | output.
    pub fn focus_mode(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        if self.cards.borrow().is_empty() {
            app.toast("Add a stage first (a)");
            return;
        }
        let dialog = adw::Dialog::builder()
            .content_width(app.window.width() - 60)
            .content_height(app.window.height() - 60)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let prev = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Previous stage (Ctrl+K)")
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Next stage (Ctrl+J)")
            .build();
        header.pack_start(&prev);
        header.pack_start(&next);
        toolbar.add_top_bar(&header);
        let input_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let input_title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .margin_start(8)
            .margin_top(6)
            .margin_bottom(4)
            .build();
        let input_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        input_col.append(&input_title);
        input_col.append(
            &gtk::ScrolledWindow::builder()
                .child(&input_box)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let editor_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let editor_title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .margin_start(8)
            .margin_top(6)
            .margin_bottom(4)
            .build();
        let editor_holder = gtk::ScrolledWindow::builder().vexpand(true).build();
        let editor_error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["error", "caption"])
            .margin_start(8)
            .build();
        editor_col.append(&editor_title);
        editor_col.append(&editor_holder);
        editor_col.append(&editor_error);
        let output_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let output_title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .margin_start(8)
            .margin_top(6)
            .margin_bottom(4)
            .build();
        let output_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        output_col.append(&output_title);
        output_col.append(
            &gtk::ScrolledWindow::builder()
                .child(&output_box)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
        inner.set_start_child(Some(&editor_col));
        inner.set_end_child(Some(&output_col));
        inner.set_shrink_start_child(false);
        inner.set_shrink_end_child(false);
        let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
        outer.set_start_child(Some(&input_col));
        outer.set_end_child(Some(&inner));
        outer.set_shrink_start_child(false);
        outer.set_shrink_end_child(false);
        let w = app.window.width() - 60;
        outer.set_position(w / 4);
        inner.set_position(w * 3 / 8);
        toolbar.set_content(Some(&outer));
        dialog.set_child(Some(&toolbar));

        // Everything below re-renders for the current card.
        let index = Rc::new(Cell::new(self.cursor.get()));
        let render: Rc<RefCell<Option<Callback>>> = Rc::new(RefCell::new(None));
        let fill = |b: &gtk::Box, docs: &[Document], app: &Rc<App>| {
            while let Some(c) = b.first_child() {
                b.remove(&c);
            }
            for d in docs {
                b.append(&preview_row(app, d));
            }
        };
        let show: Callback = {
            let me = me.clone();
            let app = app.clone();
            let index = index.clone();
            let dialog = dialog.clone();
            let render = render.clone();
            let (
                input_box,
                input_title,
                editor_holder,
                editor_title,
                editor_error,
                output_box,
                output_title,
            ) = (
                input_box.clone(),
                input_title.clone(),
                editor_holder.clone(),
                editor_title.clone(),
                editor_error.clone(),
                output_box.clone(),
                output_title.clone(),
            );
            let (prev, next) = (prev.clone(), next.clone());
            Rc::new(move || {
                let cards = me.cards.borrow().clone();
                let n = cards.len();
                if n == 0 {
                    dialog.close();
                    return;
                }
                let i = index.get().min(n - 1);
                index.set(i);
                me.cursor.set(i);
                me.renumber();
                let card = &cards[i];
                let stage = card.stage();
                dialog.set_title(&format!("Stage {} of {n} — {}", i + 1, stage.operator));
                prev.set_sensitive(i > 0);
                next.set_sensitive(i + 1 < n);
                editor_title.set_text(&format!(
                    "{}  ·  {}",
                    stage.operator,
                    pipeline::stage_info(&stage.operator)
                        .map(|s| s.help)
                        .unwrap_or("")
                ));
                // The same buffer as the card: edits sync both ways.
                let view = crate::ui::json_view("", true);
                view.set_buffer(Some(&card.editor.buffer()));
                editor_holder.set_child(Some(&view));
                // Input = previous enabled card's output; for the first stage, a sample.
                let prev_card = cards[..i].iter().rev().find(|c| c.enabled.is_active());
                match prev_card {
                    Some(pc) => {
                        input_title.set_text(&format!(
                            "Input: output of stage {}",
                            me.index_of(pc).map(|k| k + 1).unwrap_or(0)
                        ));
                        fill(&input_box, &pc.preview_docs.borrow(), &app);
                    }
                    None => {
                        input_title.set_text("Input: collection sample");
                        fill(&input_box, &[], &app);
                        if let Some(conn) = app.conn(me.conn) {
                            let client = conn.client.clone();
                            let ns = me.ns.clone();
                            let ctx = OpCtx::new(app.max_time_ms().min(15_000));
                            let input_box = input_box.clone();
                            let app = app.clone();
                            glib::spawn_future_local(async move {
                                if let Ok(docs) = crate::rt::io(async move {
                                    ops::aggregate(
                                        &client,
                                        &ns,
                                        vec![doc! { "$limit": PREVIEW_N }],
                                        &AggOpts::default(),
                                        &ctx,
                                    )
                                    .await
                                })
                                .await
                                {
                                    while let Some(c) = input_box.first_child() {
                                        input_box.remove(&c);
                                    }
                                    for d in &docs {
                                        input_box.append(&preview_row(&app, d));
                                    }
                                }
                            });
                        }
                    }
                }
                output_title.set_text(&card.preview_title.text());
                fill(&output_box, &card.preview_docs.borrow(), &app);
                editor_error.set_text(&card.error.text());
                editor_error.set_visible(card.error.is_visible());
                // Live output while typing.
                let cb: Callback = {
                    let card = card.clone();
                    let app = app.clone();
                    let (output_box, output_title, editor_error) = (
                        output_box.clone(),
                        output_title.clone(),
                        editor_error.clone(),
                    );
                    Rc::new(move || {
                        output_title.set_text(&card.preview_title.text());
                        while let Some(c) = output_box.first_child() {
                            output_box.remove(&c);
                        }
                        for d in card.preview_docs.borrow().iter() {
                            output_box.append(&preview_row(&app, d));
                        }
                        editor_error.set_text(&card.error.text());
                        editor_error.set_visible(card.error.is_visible());
                    })
                };
                for c in &cards {
                    *c.on_preview.borrow_mut() = None;
                }
                *card.on_preview.borrow_mut() = Some(cb);
                let _ = &render;
                view.grab_focus();
            })
        };
        *render.borrow_mut() = Some(show.clone());
        show();
        {
            let (show, index) = (show.clone(), index.clone());
            prev.connect_clicked(move |_| {
                index.set(index.get().saturating_sub(1));
                show();
            });
        }
        {
            let (show, index) = (show.clone(), index.clone());
            next.connect_clicked(move |_| {
                index.set(index.get() + 1);
                show();
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let dialog = dialog.clone();
            let (show, index) = (show.clone(), index.clone());
            keys.connect_key_pressed(move |_, key, _, state| {
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                match key {
                    gtk::gdk::Key::Escape => {
                        dialog.close();
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::j if ctrl => {
                        index.set(index.get() + 1);
                        show();
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::k if ctrl => {
                        index.set(index.get().saturating_sub(1));
                        show();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        dialog.add_controller(keys);
        {
            let me = me.clone();
            dialog.connect_closed(move |_| {
                for c in me.cards.borrow().iter() {
                    *c.on_preview.borrow_mut() = None;
                }
                me.root.grab_focus();
            });
        }
        dialog.present(Some(&app.window));
    }

    // ----- save / open --------------------------------------------------------

    /// `Ctrl+S`: name it; when opened from a saved pipeline, offer to update.
    pub fn save(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let pipeline = match self.pipeline() {
            Ok(p) => p,
            Err(e) => {
                app.toast(&e);
                return;
            }
        };
        if pipeline.stages.is_empty() {
            app.toast("Nothing to save: add a stage first");
            return;
        }
        let loaded = *self.loaded.borrow();
        let existing = loaded.and_then(|id| {
            app.config
                .borrow()
                .pipelines
                .iter()
                .find(|p| p.id == id)
                .cloned()
        });
        let dialog = adw::AlertDialog::new(Some("Save pipeline"), Some(&format!("On {}", self.ns)));
        let entry = gtk::Entry::builder()
            .placeholder_text("Name")
            .text(existing.as_ref().map(|p| p.name.as_str()).unwrap_or(""))
            .activates_default(true)
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel")]);
        if existing.is_some() {
            dialog.add_responses(&[("new", "Save as new"), ("update", "Update")]);
            dialog.set_default_response(Some("update"));
            dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
        } else {
            dialog.add_responses(&[("new", "Save")]);
            dialog.set_default_response(Some("new"));
            dialog.set_response_appearance("new", adw::ResponseAppearance::Suggested);
        }
        let entry2 = entry.clone();
        let window = app.window.clone();
        dialog.connect_response(None, move |_, r| {
            if r == "cancel" {
                return;
            }
            let mut name = entry2.text().trim().to_string();
            if name.is_empty() {
                name = format!("{} pipeline", me.ns);
            }
            let id = match (r, existing.as_ref()) {
                ("update", Some(e)) => {
                    let mut cfg = app.config.borrow_mut();
                    if let Some(p) = cfg.pipelines.iter_mut().find(|p| p.id == e.id) {
                        p.name = name.clone();
                        p.pipeline = pipeline.clone();
                        p.saved = chrono::Utc::now();
                    }
                    e.id
                }
                _ => {
                    let sp = SavedPipeline {
                        name: name.clone(),
                        ns: me.ns.to_string(),
                        conn: Some(me.conn),
                        pipeline: pipeline.clone(),
                        ..Default::default()
                    };
                    let id = sp.id;
                    app.config.borrow_mut().pipelines.push(sp);
                    id
                }
            };
            app.schedule_save();
            *me.loaded.borrow_mut() = Some(id);
            me.name_label.set_text(&name);
            app.toast(&format!("Saved \"{name}\""));
        });
        dialog.present(Some(&window));
        entry.grab_focus();
    }

    /// `Ctrl+Y`: the saved pipelines popover.
    pub fn open_saved(&self) {
        self.saved_btn.popup();
    }

    fn fill_saved(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        while let Some(c) = self.saved_list.first_child() {
            self.saved_list.remove(&c);
        }
        let ns = self.ns.to_string();
        let mut all: Vec<SavedPipeline> = app.config.borrow().pipelines.clone();
        all.sort_by(|a, b| (a.ns != ns).cmp(&(b.ns != ns)).then(b.saved.cmp(&a.saved)));
        if all.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No saved pipelines")
                .subtitle("Ctrl+S saves the current one")
                .build();
            row.add_css_class("dim-label");
            self.saved_list.append(&row);
        }
        for sp in all {
            let stages: Vec<&str> = sp
                .pipeline
                .stages
                .iter()
                .map(|s| s.operator.as_str())
                .collect();
            let subtitle = if sp.ns == ns {
                stages.join(" → ")
            } else {
                format!("{}  ·  {}", sp.ns, stages.join(" → "))
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&sp.name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(true)
                .build();
            row.add_css_class("viti-mono");
            if sp.ns != ns {
                row.add_css_class("dim-label");
            }
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&del);
            {
                let app = app.clone();
                let id = sp.id;
                let popover = self.saved_popover.clone();
                let me = me.clone();
                del.connect_clicked(move |_| {
                    popover.popdown();
                    app.config.borrow_mut().pipelines.retain(|p| p.id != id);
                    app.schedule_save();
                    if *me.loaded.borrow() == Some(id) {
                        *me.loaded.borrow_mut() = None;
                    }
                    app.toast("Pipeline deleted");
                });
            }
            {
                let me = me.clone();
                let popover = self.saved_popover.clone();
                row.connect_activated(move |_| {
                    popover.popdown();
                    me.load_saved(&sp);
                });
            }
            self.saved_list.append(&row);
        }
    }

    /// Open a saved pipeline into the cards (and remember it for Save).
    pub fn load_saved(&self, sp: &SavedPipeline) {
        self.mode_btn.set_active(false);
        self.set_pipeline(&sp.pipeline);
        *self.loaded.borrow_mut() = Some(sp.id);
        self.name_label.set_text(&sp.name);
        self.hide_results();
        self.root.grab_focus();
    }

    // ----- tools ------------------------------------------------------------

    /// `V`: the create-collection dialog, View kind, pipeline prefilled.
    pub fn create_view(&self) {
        let Some(app) = self.app() else { return };
        let text = match self.pipeline() {
            Ok(p) => p.to_text(),
            Err(e) => {
                app.toast(&e);
                return;
            }
        };
        crate::ui::manage::create_view_from(&app, self.conn, &self.ns, &text);
    }

    /// `Ctrl+Shift+X`: the pipeline as driver code.
    pub fn export_language(&self) {
        let Some(app) = self.app() else { return };
        let Some((docs, _)) = self.pipeline_for_run() else {
            return;
        };
        crate::ui::export_lang::show(
            &app,
            self.conn,
            self.ns.clone(),
            crate::export_to_language::Input::Pipeline(docs),
        );
    }

    /// `P`: the Explain page, pipeline source.
    pub fn explain(&self) {
        if let Some(app) = self.app() {
            app.explain_current("pipeline");
        }
    }

    pub fn toggle_options(&self) {
        let show = !self.options.reveals_child();
        self.options.set_reveal_child(show);
        if show {
            self.collation.grab_focus();
        }
    }

    pub fn focus(&self) {
        self.root.grab_focus();
    }

    /// Escape: cancel a run, else go back to the cards from the results.
    pub fn escape(&self) -> bool {
        if self.inflight.borrow().is_some() {
            self.cancel();
            true
        } else if self.results_focused.get() {
            self.results_focused.set(false);
            true
        } else {
            false
        }
    }
}
