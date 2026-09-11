//! The Schema page of a collection tab: a random sample of the documents
//! matching the query bar is analysed into one row per field (name, types with
//! their share, a chart of the main type's values). Hovering a bar shows its
//! value and count; clicking it (or Enter on the highlighted bar) filters the
//! Documents page by that value or range. Vi keys: `j`/`k` rows, `h`/`l`
//! bars, `R` analyse, `o` details, `C` copy a generated `$jsonSchema`.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, Namespace, OpCtx};
use crate::mongo::schema::{self, Axis, Chart, Field, Schema};
use crate::ui::documents::DocumentsPane;
use adw::prelude::*;
use bson::{Bson, Document, doc};
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib::{self, BoxedAnyObject};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// How many documents the CSV field picker and the schema sample at most.
pub const MAX_SAMPLE: u32 = 100_000;

struct Row {
    field: Field,
    chart: Chart,
    /// The highlighted bar (hover or `h`/`l`).
    cursor: Cell<Option<usize>>,
    area: RefCell<Option<glib::WeakRef<gtk::DrawingArea>>>,
    caption: RefCell<Option<glib::WeakRef<gtk::Label>>>,
}

impl Row {
    fn bars(&self) -> usize {
        match &self.chart {
            Chart::Bars(b) => b.len(),
            Chart::Histogram { bins, .. } => bins.len(),
            Chart::None => 0,
        }
    }

    fn total(&self) -> usize {
        match &self.chart {
            Chart::Bars(b) => b.iter().map(|b| b.count).sum(),
            Chart::Histogram { bins, .. } => bins.iter().map(|b| b.count).sum(),
            Chart::None => 0,
        }
    }

    /// `label · count (share)` for a bar.
    fn caption_for(&self, i: usize) -> String {
        let total = self.total().max(1);
        let (label, count) = match &self.chart {
            Chart::Bars(b) => match b.get(i) {
                Some(bar) => (bar.label.clone(), bar.count),
                None => return String::new(),
            },
            Chart::Histogram { axis, bins } => match bins.get(i) {
                Some(bin) => (schema::bin_label(*axis, bin), bin.count),
                None => return String::new(),
            },
            Chart::None => return String::new(),
        };
        format!(
            "{label}  ·  {} ({:.0}%)",
            crate::ui::thousands(count as u64),
            count as f64 * 100.0 / total as f64
        )
    }

    fn filter_for(&self, i: usize) -> Option<Document> {
        match &self.chart {
            Chart::Bars(b) => b
                .get(i)
                .map(|bar| schema::value_filter(&self.field.path, &bar.value)),
            Chart::Histogram { axis, bins } => bins
                .get(i)
                .map(|bin| schema::range_filter(&self.field.path, *axis, bin, i + 1 == bins.len())),
            Chart::None => None,
        }
    }

    fn set_cursor(&self, c: Option<usize>) {
        self.cursor.set(c);
        if let Some(a) = self.area.borrow().as_ref().and_then(|w| w.upgrade()) {
            a.queue_draw();
        }
        if let Some(l) = self.caption.borrow().as_ref().and_then(|w| w.upgrade()) {
            l.set_text(&match c {
                Some(i) => self.caption_for(i),
                None => self.default_caption(),
            });
        }
    }

    fn default_caption(&self) -> String {
        match &self.chart {
            Chart::Bars(b) => {
                let distinct = match self.field.main_type().map(|t| &t.values) {
                    Some(schema::Values::Strings(map)) => map.len(),
                    _ => b.len(),
                };
                if distinct > b.len() {
                    format!(
                        "top {} of {} distinct values in the sample",
                        b.len(),
                        crate::ui::thousands(distinct as u64)
                    )
                } else {
                    format!(
                        "{distinct} distinct value{} in the sample",
                        if distinct == 1 { "" } else { "s" }
                    )
                }
            }
            Chart::Histogram { axis, bins } => match (bins.first(), bins.last()) {
                (Some(lo), Some(hi)) => match axis {
                    Axis::Number => format!(
                        "{} – {}",
                        schema::number_label(lo.lo),
                        schema::number_label(hi.hi)
                    ),
                    Axis::Date => format!(
                        "{} – {}",
                        schema::date_label(lo.lo as i64),
                        schema::date_label(hi.hi as i64)
                    ),
                },
                _ => String::new(),
            },
            Chart::None => match self.field.main_type() {
                Some(t) if t.name == "Array" => {
                    let n = t.lengths.len().max(1);
                    let avg = t.lengths.iter().sum::<usize>() as f64 / n as f64;
                    let elems = t
                        .elements
                        .iter()
                        .map(|e| {
                            format!(
                                "{} ({:.0}%)",
                                e.name,
                                e.count as f64 * 100.0
                                    / t.elements.iter().map(|x| x.count).sum::<usize>().max(1)
                                        as f64
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("average length {avg:.1}  ·  elements: {elems}")
                }
                Some(t) if t.name == "Object" => format!(
                    "{} nested field{}",
                    self.field.children.len(),
                    if self.field.children.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                Some(t) => format!("{} values, no chart", t.name),
                None => String::new(),
            },
        }
    }
}

/// Which bar is under `x`, if any.
fn bar_at(row: &Row, x: f64, width: f64) -> Option<usize> {
    let n = row.bars();
    if n == 0 || width <= 0.0 {
        return None;
    }
    let i = (x / width * n as f64).floor();
    if i < 0.0 {
        None
    } else {
        Some((i as usize).min(n - 1))
    }
}

fn draw_chart(row: &Row, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32) {
    let w = w as f64;
    let h = h as f64;
    let fg = area.color();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let counts: Vec<usize> = match &row.chart {
        Chart::Bars(b) => b.iter().map(|b| b.count).collect(),
        Chart::Histogram { bins, .. } => bins.iter().map(|b| b.count).collect(),
        Chart::None => Vec::new(),
    };
    if counts.is_empty() {
        return;
    }
    let n = counts.len();
    let max = *counts.iter().max().unwrap_or(&1) as f64;
    let label_h = 14.0;
    let chart_h = (h - label_h).max(4.0);
    let slot = w / n as f64;
    let gap = if slot > 6.0 { 2.0 } else { 0.0 };
    let bar_w = (slot - gap).max(1.0);
    let cursor = row.cursor.get();
    // Baseline.
    cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.15);
    cr.rectangle(0.0, chart_h - 0.5, w, 1.0);
    let _ = cr.fill();
    for (i, c) in counts.iter().enumerate() {
        let bh = if max > 0.0 {
            (*c as f64 / max * (chart_h - 2.0)).max(if *c > 0 { 1.5 } else { 0.0 })
        } else {
            0.0
        };
        let x = i as f64 * slot;
        let alpha = if cursor == Some(i) { 1.0 } else { 0.55 };
        cr.set_source_rgba(
            accent.red() as f64,
            accent.green() as f64,
            accent.blue() as f64,
            alpha,
        );
        cr.rectangle(x, chart_h - bh, bar_w, bh);
        let _ = cr.fill();
    }
    // Labels.
    cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.7);
    cr.set_font_size(10.0);
    let fits = |s: &str, max_w: f64| -> String {
        let chars = (max_w / 6.0).floor() as usize;
        if s.chars().count() <= chars {
            s.to_string()
        } else if chars > 2 {
            ejson::truncate(s, chars)
        } else {
            String::new()
        }
    };
    match &row.chart {
        Chart::Bars(bars) => {
            if bar_w >= 18.0 {
                for (i, b) in bars.iter().enumerate() {
                    let text = fits(&b.label, bar_w);
                    if text.is_empty() {
                        continue;
                    }
                    let ext = cr.text_extents(&text).ok();
                    let tw = ext.map(|e| e.width()).unwrap_or(0.0);
                    cr.move_to(i as f64 * slot + (bar_w - tw) / 2.0, h - 3.0);
                    let _ = cr.show_text(&text);
                }
            }
        }
        Chart::Histogram { axis, bins } => {
            if let (Some(lo), Some(hi)) = (bins.first(), bins.last()) {
                let (a, b) = match axis {
                    Axis::Number => (schema::number_label(lo.lo), schema::number_label(hi.hi)),
                    Axis::Date => (
                        schema::date_label(lo.lo as i64),
                        schema::date_label(hi.hi as i64),
                    ),
                };
                cr.move_to(1.0, h - 3.0);
                let _ = cr.show_text(&a);
                let tw = cr.text_extents(&b).map(|e| e.width()).unwrap_or(0.0);
                cr.move_to(w - tw - 1.0, h - 3.0);
                let _ = cr.show_text(&b);
            }
        }
        Chart::None => {}
    }
}

pub struct SchemaPane {
    pub root: gtk::Box,
    status: gtk::Label,
    spinner: gtk::Spinner,
    sample_size: gtk::SpinButton,
    run_btn: gtk::Button,
    stop_btn: gtk::Button,
    view: gtk::ListView,
    model: gio::ListStore,
    selection: gtk::SingleSelection,
    empty: adw::StatusPage,
    stack: gtk::Stack,
    pub conn: ConnectionId,
    pub ns: Namespace,
    app: Weak<App>,
    docs: RefCell<Weak<DocumentsPane>>,
    rows: RefCell<Vec<Rc<Row>>>,
    schema: RefCell<Option<Schema>>,
    loaded: Cell<bool>,
    inflight: RefCell<Option<(OpCtx, tokio::task::AbortHandle)>>,
    generation: Cell<u64>,
    me: RefCell<Weak<Self>>,
}

impl SchemaPane {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let model = gio::ListStore::new::<BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(false);
        let factory = gtk::SignalListItemFactory::new();
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory.clone()));
        view.add_css_class("viti-schema-list");
        view.set_can_focus(false);
        view.set_single_click_activate(false);

        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let sample_size = gtk::SpinButton::with_range(10.0, MAX_SAMPLE as f64, 100.0);
        sample_size.set_value(app.config.borrow().settings.schema_sample_size as f64);
        sample_size.set_tooltip_text(Some("Documents to sample (random, matching the query bar)"));
        sample_size.set_width_chars(7);
        let copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy a $jsonSchema built from the sample (C)")
            .focus_on_click(false)
            .build();
        let run_btn = gtk::Button::builder()
            .label("Analyze")
            .tooltip_text("Sample and analyse (R)")
            .focus_on_click(false)
            .css_classes(["suggested-action"])
            .build();
        let stop_btn = gtk::Button::builder()
            .label("Stop")
            .tooltip_text("Cancel (Esc)")
            .focus_on_click(false)
            .visible(false)
            .css_classes(["destructive-action"])
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&status);
        bar.append(&spinner);
        bar.append(&gtk::Label::new(Some("Sample")));
        bar.append(&sample_size);
        bar.append(&copy_btn);
        bar.append(&stop_btn);
        bar.append(&run_btn);

        let empty = adw::StatusPage::builder()
            .icon_name("view-grid-symbolic")
            .title("Schema")
            .description("Analyse a random sample of the documents matching the query bar: field names, types and value distributions. Press R or click Analyze.")
            .vexpand(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("list"));
        stack.set_visible_child_name("empty");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-schema");
        root.append(&bar);
        root.append(&stack);

        let pane = Rc::new(Self {
            root,
            status,
            spinner,
            sample_size,
            run_btn: run_btn.clone(),
            stop_btn: stop_btn.clone(),
            view: view.clone(),
            model,
            selection: selection.clone(),
            empty,
            stack,
            conn,
            ns,
            app: Rc::downgrade(app),
            docs: RefCell::new(Weak::new()),
            rows: RefCell::new(Vec::new()),
            schema: RefCell::new(None),
            loaded: Cell::new(false),
            inflight: RefCell::new(None),
            generation: Cell::new(0),
            me: RefCell::new(Weak::new()),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);
        pane.setup_factory(&factory);
        {
            let p = pane.clone();
            run_btn.connect_clicked(move |_| p.analyze());
        }
        {
            let p = pane.clone();
            stop_btn.connect_clicked(move |_| p.cancel());
        }
        {
            let p = pane.clone();
            copy_btn.connect_clicked(move |_| p.copy_json_schema());
        }
        {
            let p = pane.clone();
            pane.sample_size.connect_value_changed(move |s| {
                if let Some(app) = p.app() {
                    app.config.borrow_mut().settings.schema_sample_size = s.value() as u32;
                    app.schedule_save();
                }
            });
        }
        {
            let p = pane.clone();
            view.connect_activate(move |_, _| p.apply_cursor());
        }
        {
            // Clicking anywhere in the list focuses the pane so vi keys apply.
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

    /// The Documents pane whose filter the sample uses and that clicks filter.
    pub fn set_docs(&self, docs: &Rc<DocumentsPane>) {
        *self.docs.borrow_mut() = Rc::downgrade(docs);
    }

    /// The last analysis, for the Validation page's "generate from schema".
    pub fn schema(&self) -> Option<Schema> {
        self.schema.borrow().clone()
    }

    fn setup_factory(self: &Rc<Self>, factory: &gtk::SignalListItemFactory) {
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let name = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .css_classes(["viti-key", "viti-schema-name"])
                .build();
            let types = gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["dim-label", "caption", "viti-schema-types"])
                .build();
            let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
            left.set_width_request(240);
            left.set_valign(gtk::Align::Start);
            left.append(&name);
            left.append(&types);
            let area = gtk::DrawingArea::builder()
                .hexpand(true)
                .content_height(58)
                .build();
            let caption = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build();
            let right = gtk::Box::new(gtk::Orientation::Vertical, 2);
            right.set_hexpand(true);
            right.append(&area);
            right.append(&caption);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("viti-schema-row");
            row.set_margin_start(8);
            row.set_margin_end(8);
            row.set_margin_top(4);
            row.set_margin_bottom(4);
            row.append(&left);
            row.append(&right);
            item.set_child(Some(&row));
        });
        let me = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            let row: Rc<Row> = obj.borrow::<Rc<Row>>().clone();
            let root = item.child().and_downcast::<gtk::Box>().unwrap();
            let left = root.first_child().and_downcast::<gtk::Box>().unwrap();
            let name = left.first_child().and_downcast::<gtk::Label>().unwrap();
            let types = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let right = left.next_sibling().and_downcast::<gtk::Box>().unwrap();
            let area = right
                .first_child()
                .and_downcast::<gtk::DrawingArea>()
                .unwrap();
            let caption = area.next_sibling().and_downcast::<gtk::Label>().unwrap();

            left.set_margin_start(row.field.depth as i32 * 18);
            name.set_text(&row.field.name);
            name.set_tooltip_text(Some(&row.field.path));
            types.set_text(&row.field.types_text());
            *row.area.borrow_mut() = Some(area.downgrade());
            *row.caption.borrow_mut() = Some(caption.downgrade());
            caption.set_text(&match row.cursor.get() {
                Some(i) => row.caption_for(i),
                None => row.default_caption(),
            });
            {
                let row = row.clone();
                area.set_draw_func(move |a, cr, w, h| draw_chart(&row, a, cr, w, h));
            }
            area.set_visible(row.bars() > 0);
            area.set_cursor_from_name(if row.bars() > 0 {
                Some("pointer")
            } else {
                None
            });
            // Fresh controllers per bind (removed again on unbind).
            let motion = gtk::EventControllerMotion::new();
            {
                let row = row.clone();
                let area2 = area.clone();
                motion.connect_motion(move |_, x, _| {
                    let hit = bar_at(&row, x, area2.width() as f64);
                    if hit != row.cursor.get() {
                        row.set_cursor(hit);
                    }
                });
            }
            {
                let row = row.clone();
                motion.connect_leave(move |_| row.set_cursor(None));
            }
            area.add_controller(motion);
            let click = gtk::GestureClick::new();
            {
                let row = row.clone();
                let area2 = area.clone();
                let me = me.clone();
                click.connect_released(move |_, _, x, _| {
                    if let (Some(i), Some(p)) =
                        (bar_at(&row, x, area2.width() as f64), me.upgrade())
                    {
                        p.apply_bar(&row, i);
                    }
                });
            }
            area.add_controller(click);
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(root) = item.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let Some(area) = root
                .first_child()
                .and_then(|l| l.next_sibling())
                .and_then(|r| r.first_child())
                .and_downcast::<gtk::DrawingArea>()
            else {
                return;
            };
            let ctls = area.observe_controllers();
            let mut to_remove = Vec::new();
            for i in 0..ctls.n_items() {
                if let Some(c) = ctls.item(i).and_downcast::<gtk::EventController>() {
                    to_remove.push(c);
                }
            }
            for c in to_remove {
                area.remove_controller(&c);
            }
        });
    }

    /// Analyse once, the first time the page is shown.
    pub fn ensure_loaded(&self) {
        if !self.loaded.replace(true) {
            self.analyze();
        }
    }

    fn set_busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.run_btn.set_visible(!busy);
        self.stop_btn.set_visible(busy);
    }

    /// The query bar's filter as last run on the Documents page.
    fn filter(&self) -> Document {
        self.docs
            .borrow()
            .upgrade()
            .map(|d| d.current_spec().filter)
            .unwrap_or_default()
    }

    /// `R`: sample and analyse.
    pub fn analyze(&self) {
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            app.toast("Not connected");
            return;
        };
        self.cancel_silent();
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let filter = self.filter();
        let n = self.sample_size.value() as u64;
        let ctx = OpCtx::new(app.max_time_ms());
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.set_busy(true);
        self.status
            .set_text(&format!("Sampling {} documents…", crate::ui::thousands(n)));
        let ctx2 = ctx.clone();
        let filter2 = filter.clone();
        let handle = crate::rt::spawn(async move {
            let r = ops::sample_random(&client, &ns, filter2.clone(), n, &ctx2).await;
            let docs = match r {
                Ok(d) => d,
                // `$sample` is refused on some namespaces: take the first N instead.
                Err(e) => {
                    tracing::debug!("$sample on {ns}: {e:#}; falling back to find");
                    ops::sample(&client, &ns, filter2, n, &ctx2).await?
                }
            };
            Ok::<Schema, anyhow::Error>(schema::analyze(&docs))
        });
        *self.inflight.borrow_mut() = Some((ctx, handle.abort_handle()));
        glib::spawn_future_local(async move {
            let r = handle.await;
            if me.generation.get() != generation {
                return;
            }
            me.inflight.borrow_mut().take();
            me.set_busy(false);
            match r {
                Ok(Ok(schema)) => me.set_schema(schema, &filter),
                Ok(Err(e)) => {
                    if let Some(app) = me.app() {
                        app.toast_error(&format!("schema sample of {}", me.ns), &e);
                    }
                    me.status.set_text("Sampling failed");
                }
                Err(_) => {}
            }
        });
    }

    fn cancel_silent(&self) {
        if let Some((ctx, handle)) = self.inflight.borrow_mut().take() {
            handle.abort();
            self.generation.set(self.generation.get() + 1);
            if let Some(app) = self.app()
                && let Some(conn) = app.conn(self.conn)
            {
                let client = conn.client.clone();
                crate::rt::spawn(async move {
                    if let Err(e) = ops::kill_by_comment(&client, &ctx.comment).await {
                        tracing::warn!("killOp: {e:#}");
                    }
                });
            }
            self.set_busy(false);
        }
    }

    /// Escape / Stop.
    pub fn cancel(&self) {
        if self.inflight.borrow().is_some() {
            self.cancel_silent();
            self.status.set_text("Cancelled");
            if let Some(app) = self.app() {
                app.toast("Sampling cancelled");
            }
        }
    }

    fn set_schema(&self, schema: Schema, filter: &Document) {
        let keep = self.cursor().unwrap_or(0);
        let total = self.docs.borrow().upgrade().and_then(|d| d.total.get());
        let mut status = format!(
            "Sampled {} document{}",
            crate::ui::thousands(schema.sampled as u64),
            if schema.sampled == 1 { "" } else { "s" }
        );
        if let Some(t) = total {
            status.push_str(&format!(" of {}", crate::ui::thousands(t)));
        }
        let fields = schema.flatten().len();
        status.push_str(&format!(
            "  ·  {fields} field{}",
            if fields == 1 { "" } else { "s" }
        ));
        if !filter.is_empty() {
            status.push_str(&format!(
                "  ·  filter {}",
                ejson::truncate(&ejson::compact(filter, Mode::Relaxed), 60)
            ));
        }
        self.status.set_text(&status);
        let rows: Vec<Rc<Row>> = schema
            .flatten()
            .into_iter()
            .map(|f| {
                let chart = f.main_type().map(schema::chart).unwrap_or(Chart::None);
                Rc::new(Row {
                    field: f.clone(),
                    chart,
                    cursor: Cell::new(None),
                    area: RefCell::new(None),
                    caption: RefCell::new(None),
                })
            })
            .collect();
        *self.schema.borrow_mut() = Some(schema);
        *self.rows.borrow_mut() = rows.clone();
        self.model.remove_all();
        for r in rows {
            self.model.append(&BoxedAnyObject::new(r));
        }
        if self.model.n_items() > 0 {
            self.stack.set_visible_child_name("list");
            self.set_cursor(keep);
        } else {
            self.empty.set_description(Some(
                "No documents matched the query bar, so there is nothing to analyse.",
            ));
            self.stack.set_visible_child_name("empty");
        }
    }

    // ----- navigation -----------------------------------------------------

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
            .scroll_to(i as u32, gtk::ListScrollFlags::NONE, None);
    }

    fn current_row(&self) -> Option<Rc<Row>> {
        let i = self.cursor()?;
        self.rows.borrow().get(i).cloned()
    }

    pub fn move_cursor(&self, delta: i64) {
        let n = self.model.n_items() as i64;
        if n == 0 {
            return;
        }
        if let Some(r) = self.current_row() {
            r.set_cursor(None);
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

    /// `h`/`l`: highlight the previous/next bar of the current row.
    pub fn move_bar(&self, delta: i64) {
        let Some(row) = self.current_row() else {
            return;
        };
        let n = row.bars() as i64;
        if n == 0 {
            return;
        }
        let next = match row.cursor.get() {
            None => {
                if delta > 0 {
                    0
                } else {
                    n - 1
                }
            }
            Some(c) => (c as i64 + delta).clamp(0, n - 1),
        };
        row.set_cursor(Some(next as usize));
    }

    /// Enter: filter the Documents page by the highlighted bar.
    pub fn apply_cursor(&self) {
        let Some(row) = self.current_row() else {
            return;
        };
        match row.cursor.get() {
            Some(i) => self.apply_bar(&row, i),
            None => {
                if row.bars() > 0 {
                    row.set_cursor(Some(0));
                    if let Some(app) = self.app() {
                        app.toast("h / l pick a bar, Enter filters by it");
                    }
                } else {
                    self.peek();
                }
            }
        }
    }

    fn apply_bar(&self, row: &Row, i: usize) {
        let Some(filter) = row.filter_for(i) else {
            return;
        };
        self.apply_filter(filter);
    }

    /// Run `filter` on the Documents page and switch to it.
    pub fn apply_filter(&self, filter: Document) {
        let Some(app) = self.app() else { return };
        let Some(docs) = self.docs.borrow().upgrade() else {
            return;
        };
        let mut q = docs.current_query();
        q.filter = ejson::compact(&filter, Mode::Relaxed);
        docs.query_bar.set_query(&q);
        docs.run_query(q);
        if let Some(t) = app.current_tab() {
            t.show_page("documents");
            app.sync_page_picker();
        }
    }

    /// `o`: the field's statistics as JSON.
    pub fn peek(&self) {
        let Some(row) = self.current_row() else {
            return;
        };
        let Some(app) = self.app() else { return };
        let f = &row.field;
        let types: Vec<Bson> = f
            .types
            .iter()
            .map(|t| {
                let mut d = doc! {
                    "type": t.name,
                    "count": t.count as i64,
                    "percent": (f.type_pct(t) * 10.0).round() / 10.0,
                };
                if t.name == "Array" {
                    d.insert(
                        "elements",
                        Bson::Array(
                            t.elements
                                .iter()
                                .map(|e| Bson::Document(doc! { "type": e.name, "count": e.count as i64 }))
                                .collect(),
                        ),
                    );
                    if !t.lengths.is_empty() {
                        d.insert(
                            "lengths",
                            doc! {
                                "min": *t.lengths.iter().min().unwrap_or(&0) as i64,
                                "max": *t.lengths.iter().max().unwrap_or(&0) as i64,
                                "average": t.lengths.iter().sum::<usize>() as f64 / t.lengths.len() as f64,
                            },
                        );
                    }
                }
                Bson::Document(d)
            })
            .collect();
        let mut d = doc! {
            "path": f.path.clone(),
            "present": f.count as i64,
            "of": f.parent_count as i64,
            "presentPercent": (f.presence_pct() * 10.0).round() / 10.0,
            "types": types,
        };
        match &row.chart {
            Chart::Bars(bars) => {
                d.insert(
                    "values",
                    Bson::Array(
                        bars.iter()
                            .map(|b| {
                                Bson::Document(
                                    doc! { "value": b.value.clone(), "count": b.count as i64 },
                                )
                            })
                            .collect(),
                    ),
                );
            }
            Chart::Histogram { axis, bins } => {
                d.insert(
                    "histogram",
                    Bson::Array(
                        bins.iter()
                            .map(|b| {
                                Bson::Document(
                                    doc! { "range": schema::bin_label(*axis, b), "count": b.count as i64 },
                                )
                            })
                            .collect(),
                    ),
                );
            }
            Chart::None => {}
        }
        crate::ui::peek_document(&app, &f.path, &ejson::pretty(&d, Mode::Relaxed));
    }

    /// `C`: a `$jsonSchema` validator built from the sample, to the clipboard.
    pub fn copy_json_schema(&self) {
        let Some(app) = self.app() else { return };
        let Some(schema) = self.schema() else {
            app.toast("Analyse the collection first (R)");
            return;
        };
        let js = crate::mongo::validation::json_schema(&schema);
        crate::ui::copy_text(&ejson::pretty(&js, Mode::Relaxed));
        app.toast("Copied $jsonSchema");
    }
}
