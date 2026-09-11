//! The Validation page of a collection tab: the collection's validator as an
//! editable JSON document with its action (error / warn) and level (strict /
//! moderate / off), one sample document that passes the rules and one that
//! does not, and "generate from schema" which drafts a `$jsonSchema` from a
//! sample. Nothing is written until Save (`Ctrl+S`). Vi keys: `e` edit,
//! `Ctrl+E` external editor, `G` generate, `r` reload, `Ctrl+S` save.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, Namespace, OpCtx};
use crate::mongo::validation::{self, ACTIONS, LEVELS, Validation};
use crate::ui::schema::SchemaPane;
use adw::prelude::*;
use bson::{Document, doc};
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub struct ValidationPane {
    pub root: gtk::Box,
    status: gtk::Label,
    spinner: gtk::Spinner,
    save_btn: gtk::Button,
    editor: sourceview5::View,
    error: gtk::Label,
    action: gtk::DropDown,
    level: gtk::DropDown,
    valid_view: sourceview5::View,
    invalid_view: sourceview5::View,
    valid_title: gtk::Label,
    invalid_title: gtk::Label,
    pub conn: ConnectionId,
    pub ns: Namespace,
    app: Weak<App>,
    schema: RefCell<Weak<SchemaPane>>,
    current: RefCell<Validation>,
    dirty: Cell<bool>,
    /// Programmatic edits must not count as user changes.
    loading: Cell<bool>,
    loaded: Cell<bool>,
    me: RefCell<Weak<Self>>,
}

fn sample_card(title: &str) -> (gtk::Box, gtk::Label, sourceview5::View) {
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_start(8)
        .margin_top(6)
        .margin_bottom(4)
        .build();
    let view = crate::ui::json_view("", false);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .hexpand(true)
        .build();
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("viti-tile");
    card.set_hexpand(true);
    card.append(&label);
    card.append(&scroller);
    (card, label, view)
}

impl ValidationPane {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Reload from the server (r)")
            .focus_on_click(false)
            .build();
        let generate = gtk::Button::builder()
            .label("Generate from schema")
            .tooltip_text("Draft a $jsonSchema from a sample of the documents (G)")
            .focus_on_click(false)
            .build();
        let external = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit in the external editor (Ctrl+E)")
            .focus_on_click(false)
            .build();
        let save_btn = gtk::Button::builder()
            .label("Save")
            .tooltip_text("Apply with collMod (Ctrl+S)")
            .focus_on_click(false)
            .sensitive(false)
            .css_classes(["suggested-action"])
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&status);
        bar.append(&spinner);
        bar.append(&refresh);
        bar.append(&external);
        bar.append(&generate);
        bar.append(&save_btn);

        let editor = crate::ui::json_view("", true);
        editor.add_css_class("viti-validation-editor");
        let editor_scroller = gtk::ScrolledWindow::builder()
            .child(&editor)
            .vexpand(true)
            .min_content_height(140)
            .build();
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(8)
            .margin_end(8)
            .build();

        let action = gtk::DropDown::from_strings(&ACTIONS);
        action.set_tooltip_text(Some(
            "error: reject invalid writes  ·  warn: allow them and log a warning",
        ));
        let level = gtk::DropDown::from_strings(&LEVELS);
        level.set_tooltip_text(Some(
            "strict: validate every insert and update  ·  moderate: skip existing invalid documents  ·  off: never validate",
        ));
        let options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        options.set_margin_start(8);
        options.set_margin_end(8);
        options.set_margin_top(4);
        options.set_margin_bottom(6);
        options.append(&gtk::Label::new(Some("Action")));
        options.append(&action);
        options.append(
            &gtk::Label::builder()
                .label("Level")
                .margin_start(12)
                .build(),
        );
        options.append(&level);
        let hint = gtk::Label::builder()
            .label("An empty document removes validation.")
            .css_classes(["dim-label", "caption"])
            .hexpand(true)
            .xalign(1.0)
            .build();
        options.append(&hint);

        let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
        top.append(&editor_scroller);
        top.append(&error);
        top.append(&options);

        let (valid_card, valid_title, valid_view) = sample_card("Sample document that passes");
        let (invalid_card, invalid_title, invalid_view) = sample_card("Sample document that fails");
        let samples = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        samples.set_margin_start(8);
        samples.set_margin_end(8);
        samples.set_margin_bottom(8);
        samples.set_homogeneous(true);
        samples.append(&valid_card);
        samples.append(&invalid_card);

        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_start_child(Some(&top));
        paned.set_end_child(Some(&samples));
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(false);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_position(320);
        paned.set_vexpand(true);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-validation");
        root.append(&bar);
        root.append(&paned);

        let pane = Rc::new(Self {
            root,
            status,
            spinner,
            save_btn: save_btn.clone(),
            editor: editor.clone(),
            error,
            action: action.clone(),
            level: level.clone(),
            valid_view,
            invalid_view,
            valid_title,
            invalid_title,
            conn,
            ns,
            app: Rc::downgrade(app),
            schema: RefCell::new(Weak::new()),
            current: RefCell::new(Validation::default()),
            dirty: Cell::new(false),
            loading: Cell::new(false),
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
            generate.connect_clicked(move |_| p.generate());
        }
        {
            let p = pane.clone();
            external.connect_clicked(move |_| p.edit_external());
        }
        {
            let p = pane.clone();
            save_btn.connect_clicked(move |_| p.save());
        }
        {
            let p = pane.clone();
            editor.buffer().connect_changed(move |_| p.on_user_change());
        }
        {
            let p = pane.clone();
            action.connect_selected_notify(move |_| p.on_user_change());
        }
        {
            let p = pane.clone();
            level.connect_selected_notify(move |_| p.on_user_change());
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    fn me(&self) -> Option<Rc<Self>> {
        self.me.borrow().upgrade()
    }

    /// The Schema page, whose last analysis "generate" reuses.
    pub fn set_schema(&self, schema: &Rc<SchemaPane>) {
        *self.schema.borrow_mut() = Rc::downgrade(schema);
    }

    pub fn ensure_loaded(&self) {
        if !self.loaded.replace(true) {
            self.load();
        }
    }

    fn set_busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
    }

    fn on_user_change(&self) {
        if self.loading.get() {
            return;
        }
        self.dirty.set(true);
        self.save_btn.set_sensitive(true);
        self.status.set_text("Unsaved changes");
        self.error.set_visible(false);
    }

    fn selected(dd: &gtk::DropDown, items: &[&str]) -> String {
        items
            .get(dd.selected() as usize)
            .unwrap_or(&items[0])
            .to_string()
    }

    fn select(dd: &gtk::DropDown, items: &[&str], value: &str) {
        let i = items
            .iter()
            .position(|v| v.eq_ignore_ascii_case(value))
            .unwrap_or(0);
        dd.set_selected(i as u32);
    }

    fn show_status(&self, v: &Validation) {
        self.status.set_text(&if v.validator.is_empty() {
            "No validation rules".to_string()
        } else {
            format!(
                "Validation on  ·  action {}  ·  level {}",
                v.action, v.level
            )
        });
    }

    /// Put `v` in the widgets without marking them dirty.
    fn show(&self, v: &Validation) {
        self.loading.set(true);
        let text = if v.validator.is_empty() {
            "{}".to_string()
        } else {
            ejson::pretty(&v.validator, Mode::Relaxed)
        };
        self.editor.buffer().set_text(&text);
        Self::select(&self.action, &ACTIONS, &v.action);
        Self::select(&self.level, &LEVELS, &v.level);
        self.loading.set(false);
        self.dirty.set(false);
        self.save_btn.set_sensitive(false);
        self.error.set_visible(false);
        self.show_status(v);
    }

    /// `r`: fetch the rules and the samples.
    pub fn load(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        if self.dirty.get() {
            let me2 = me.clone();
            crate::ui::confirm(
                &app.window.clone(),
                "Discard changes?",
                "The rules were edited but not saved.",
                "Discard",
                true,
                move || {
                    me2.dirty.set(false);
                    me2.load();
                },
            );
            return;
        }
        let client = conn.client.clone();
        let ns = self.ns.clone();
        self.set_busy(true);
        glib::spawn_future_local(async move {
            let ns2 = ns.clone();
            let r = crate::rt::io(async move { validation::fetch(&client, &ns2).await }).await;
            me.set_busy(false);
            match r {
                Ok(v) => {
                    me.show(&v);
                    *me.current.borrow_mut() = v;
                    me.load_samples();
                }
                Err(e) => {
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("validation rules of {ns}"), &e);
                    }
                    me.status.set_text("Could not load the rules");
                }
            }
        });
    }

    /// The editor's document, or `None` after showing the parse error.
    fn editor_validator(&self) -> Option<Document> {
        let text = crate::ui::buffer_text(&self.editor.buffer());
        match ejson::parse_document_or_empty(&text) {
            Ok(d) => {
                self.error.set_visible(false);
                Some(d)
            }
            Err(e) => {
                self.error.set_text(&format!("Rules: {e}"));
                self.error.set_visible(true);
                None
            }
        }
    }

    /// One document matching the editor's rules and one that does not.
    pub fn load_samples(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let Some(validator) = self.editor_validator() else {
            return;
        };
        if validator.is_empty() {
            self.valid_view.buffer().set_text("");
            self.invalid_view.buffer().set_text("");
            self.valid_title.set_text("Sample document that passes");
            self.invalid_title.set_text("Sample document that fails");
            return;
        }
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let ctx = OpCtx::new(app.max_time_ms().min(10_000));
        let failing = doc! { "$nor": [validator.clone()] };
        glib::spawn_future_local(async move {
            let (c1, n1, ctx1, v1) = (client.clone(), ns.clone(), ctx.clone(), validator);
            let ok = crate::rt::io(async move { ops::sample(&c1, &n1, v1, 1, &ctx1).await }).await;
            let (c2, n2, ctx2) = (client, ns.clone(), ctx);
            let bad =
                crate::rt::io(async move { ops::sample(&c2, &n2, failing, 1, &ctx2).await }).await;
            let show = |view: &sourceview5::View,
                        title: &gtk::Label,
                        base: &str,
                        r: anyhow::Result<Vec<Document>>| match r {
                Ok(docs) => match docs.first() {
                    Some(d) => {
                        view.buffer().set_text(&ejson::pretty(d, Mode::Relaxed));
                        title.set_text(base);
                    }
                    None => {
                        view.buffer().set_text("");
                        title.set_text(&format!("{base}: none found"));
                    }
                },
                Err(e) => {
                    view.buffer().set_text("");
                    title.set_text(&format!("{base}: {e:#}"));
                    tracing::warn!("validation sample on {ns}: {e:#}");
                }
            };
            show(
                &me.valid_view,
                &me.valid_title,
                "Sample document that passes",
                ok,
            );
            show(
                &me.invalid_view,
                &me.invalid_title,
                "Sample document that fails",
                bad,
            );
        });
    }

    /// `Ctrl+S`: `collMod` with the editor's rules.
    pub fn save(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        if app.write_guard().is_err() {
            return;
        }
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let Some(validator) = self.editor_validator() else {
            self.editor.grab_focus();
            return;
        };
        let v = Validation {
            validator,
            level: Self::selected(&self.level, &LEVELS),
            action: Self::selected(&self.action, &ACTIONS),
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        self.set_busy(true);
        glib::spawn_future_local(async move {
            let (ns2, v2) = (ns.clone(), v.clone());
            let r = crate::rt::io(async move { validation::set(&client, &ns2, &v2).await }).await;
            me.set_busy(false);
            match r {
                Ok(()) => {
                    app.toast(&if v.validator.is_empty() {
                        format!("Removed validation from {ns}")
                    } else {
                        format!("Saved validation rules for {ns}")
                    });
                    me.dirty.set(false);
                    me.save_btn.set_sensitive(false);
                    me.show_status(&v);
                    *me.current.borrow_mut() = v;
                    me.load_samples();
                }
                Err(e) => app.toast_error(&format!("collMod on {ns}"), &e),
            }
        });
    }

    /// `G`: draft a `$jsonSchema` from the Schema page's analysis, sampling
    /// the collection first if it has none.
    pub fn generate(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        if let Some(schema) = self.schema.borrow().upgrade().and_then(|s| s.schema()) {
            self.set_generated(&schema);
            return;
        }
        let Some(conn) = app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let n = app.config.borrow().settings.schema_sample_size as u64;
        let ctx = OpCtx::new(app.max_time_ms());
        self.set_busy(true);
        self.status.set_text("Sampling documents…");
        glib::spawn_future_local(async move {
            let ns2 = ns.clone();
            let r = crate::rt::io(async move {
                let docs = match ops::sample_random(&client, &ns2, Document::new(), n, &ctx).await {
                    Ok(d) => d,
                    Err(_) => ops::sample(&client, &ns2, Document::new(), n, &ctx).await?,
                };
                Ok::<_, anyhow::Error>(crate::mongo::schema::analyze(&docs))
            })
            .await;
            me.set_busy(false);
            match r {
                Ok(schema) => me.set_generated(&schema),
                Err(e) => app.toast_error(&format!("sample of {ns}"), &e),
            }
        });
    }

    fn set_generated(&self, schema: &crate::mongo::schema::Schema) {
        let js = validation::json_schema(schema);
        self.editor
            .buffer()
            .set_text(&ejson::pretty(&js, Mode::Relaxed));
        self.on_user_change();
        self.status.set_text(&format!(
            "Drafted from {} sampled documents — review, then Save",
            crate::ui::thousands(schema.sampled as u64)
        ));
        self.load_samples();
        self.editor.grab_focus();
    }

    /// `e`: focus the rules editor.
    pub fn focus_editor(&self) {
        self.editor.grab_focus();
    }

    /// `Ctrl+E`: the rules in the external editor; they come back through
    /// `set_text` and stay unsaved.
    pub fn edit_external(&self) {
        let Some(app) = self.app() else { return };
        let text = crate::ui::buffer_text(&self.editor.buffer());
        let settings = app.config.borrow().settings.clone();
        if let Err(e) = app.editor_pane.open(
            &settings,
            crate::ui::editor_pane::JobKind::Validation {
                conn: self.conn,
                ns: self.ns.clone(),
            },
            text,
            &format!("{} — validation rules", self.ns),
        ) {
            app.toast_error("open editor", &e);
        }
    }

    /// Back from the external editor: replace the rules, unsaved.
    pub fn set_text(&self, text: &str) {
        self.editor.buffer().set_text(text);
        self.on_user_change();
        self.load_samples();
    }

    /// Whether the editor has focus (Escape then blurs, not cancels).
    pub fn editor_focused(&self) -> bool {
        self.editor.has_focus()
    }
}
