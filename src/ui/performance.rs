//! The Performance page: one tab per connection with 1 Hz `serverStatus`
//! charts (operations, read & write, network, memory, connections), the
//! hottest collections (`top` deltas) and the slowest active operations
//! (`$currentOp`) with kill. Vi keys: `j` `k` move in the operations, `o` /
//! Enter details, `Ctrl+D` kill, `space` pause, `r` sample now.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops;
use crate::mongo::perf::{self, CurrentOp, HotCollection, Rates, Snapshot, TopEntry};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::glib::BoxedAnyObject;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

/// Points kept per chart (one per second).
const HISTORY: usize = 60;
const HOT_LIMIT: usize = 6;

/// Adwaita palette (blue, green, orange, purple, red, teal) as RGB.
const COLORS: [(f64, f64, f64); 6] = [
    (0.208, 0.518, 0.894),
    (0.180, 0.760, 0.494),
    (0.902, 0.522, 0.114),
    (0.569, 0.255, 0.925),
    (0.878, 0.106, 0.141),
    (0.220, 0.690, 0.690),
];

type Pick = fn(&Rates) -> Vec<f64>;
type Fmt = fn(f64) -> String;

struct ChartSpec {
    title: &'static str,
    series: &'static [&'static str],
    pick: Pick,
    fmt: Fmt,
}

const CHARTS: &[ChartSpec] = &[
    ChartSpec {
        title: "Operations / s",
        series: &["insert", "query", "update", "delete", "getmore", "command"],
        pick: |r| {
            vec![
                r.ops.insert,
                r.ops.query,
                r.ops.update,
                r.ops.delete,
                r.ops.getmore,
                r.ops.command,
            ]
        },
        fmt: perf::short_number,
    },
    ChartSpec {
        title: "Read & write clients",
        series: &[
            "active readers",
            "active writers",
            "queued readers",
            "queued writers",
        ],
        pick: |r| {
            vec![
                r.active_readers,
                r.active_writers,
                r.queued_readers,
                r.queued_writers,
            ]
        },
        fmt: perf::short_number,
    },
    ChartSpec {
        title: "Network / s",
        series: &["bytes in", "bytes out"],
        pick: |r| vec![r.net_in, r.net_out],
        fmt: perf::short_bytes,
    },
    ChartSpec {
        title: "Memory (MB)",
        series: &["resident", "virtual"],
        pick: |r| vec![r.mem_resident_mb, r.mem_virtual_mb],
        fmt: perf::short_number,
    },
    ChartSpec {
        title: "Connections",
        series: &["current"],
        pick: |r| vec![r.connections],
        fmt: perf::short_number,
    },
];

struct Chart {
    root: gtk::Box,
    area: gtk::DrawingArea,
    legend: Vec<gtk::Label>,
    spec: &'static ChartSpec,
}

fn card(title: &str) -> (gtk::Box, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class("viti-perf-card");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    head.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    root.append(&head);
    (root, head)
}

fn build_chart(spec: &'static ChartSpec, history: Rc<RefCell<VecDeque<Rates>>>) -> Chart {
    let (root, _) = card(spec.title);
    let legend_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(10)
        .row_spacing(2)
        .max_children_per_line(6)
        .build();
    let mut legend = Vec::new();
    for (i, name) in spec.series.iter().enumerate() {
        let (r, g, b) = COLORS[i % COLORS.len()];
        let swatch = gtk::DrawingArea::builder()
            .content_width(10)
            .content_height(10)
            .valign(gtk::Align::Center)
            .build();
        swatch.set_draw_func(move |_, cr, w, h| {
            cr.set_source_rgb(r, g, b);
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            let _ = cr.fill();
        });
        let label = gtk::Label::builder()
            .label(format!("{name} —"))
            .xalign(0.0)
            .css_classes(["caption", "viti-mono"])
            .build();
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        item.append(&swatch);
        item.append(&label);
        legend_box.insert(&item, -1);
        legend.push(label);
    }
    let area = gtk::DrawingArea::builder()
        .content_height(96)
        .hexpand(true)
        .build();
    {
        let history = history.clone();
        area.set_draw_func(move |a, cr, w, h| draw(spec, &history.borrow(), a, cr, w, h));
    }
    root.append(&legend_box);
    root.append(&area);
    Chart {
        root,
        area,
        legend,
        spec,
    }
}

fn draw(
    spec: &ChartSpec,
    history: &VecDeque<Rates>,
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    w: i32,
    h: i32,
) {
    let w = w as f64;
    let h = h as f64;
    let fg = area.color();
    let fgc = |a: f64| cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, a);
    // Grid: baseline and three faint lines.
    for i in 0..4 {
        let y = h - 0.5 - (h - 1.0) * i as f64 / 3.0;
        fgc(if i == 0 { 0.25 } else { 0.07 });
        cr.rectangle(0.0, y, w, 1.0);
        let _ = cr.fill();
    }
    if history.is_empty() {
        return;
    }
    let points: Vec<Vec<f64>> = history.iter().map(spec.pick).collect();
    let max = points
        .iter()
        .flatten()
        .cloned()
        .fold(0.0_f64, f64::max)
        .max(1.0)
        * 1.1;
    let step = w / (HISTORY.max(2) - 1) as f64;
    let n = points.len();
    let x0 = w - step * (n - 1) as f64;
    let y_of = |v: f64| h - 1.0 - (v / max) * (h - 4.0);
    for s in (0..spec.series.len()).rev() {
        let (r, g, b) = COLORS[s % COLORS.len()];
        cr.set_source_rgba(r, g, b, 0.12);
        cr.move_to(x0, h);
        for (i, p) in points.iter().enumerate() {
            cr.line_to(x0 + step * i as f64, y_of(p[s]));
        }
        cr.line_to(x0 + step * (n - 1) as f64, h);
        cr.close_path();
        let _ = cr.fill();
        cr.set_source_rgb(r, g, b);
        cr.set_line_width(1.5);
        for (i, p) in points.iter().enumerate() {
            let (x, y) = (x0 + step * i as f64, y_of(p[s]));
            if i == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        let _ = cr.stroke();
    }
    // Scale label, top left.
    fgc(0.6);
    cr.set_font_size(10.0);
    cr.move_to(3.0, 11.0);
    let _ = cr.show_text(&(spec.fmt)(max / 1.1));
}

pub struct PerformancePane {
    pub root: gtk::Box,
    pub conn: ConnectionId,
    pub page: RefCell<Option<adw::TabPage>>,
    app: Weak<App>,
    me: RefCell<Weak<Self>>,
    pause_btn: gtk::ToggleButton,
    status: gtk::Label,
    error: gtk::Label,
    charts: Vec<Chart>,
    hot_list: gtk::ListBox,
    hot_empty: gtk::Label,
    ops_store: gio::ListStore,
    ops_selection: gtk::SingleSelection,
    ops_view: gtk::ColumnView,
    ops_empty: gtk::Label,
    history: Rc<RefCell<VecDeque<Rates>>>,
    prev: RefCell<Option<(Instant, Snapshot, Vec<TopEntry>)>>,
    ops: RefCell<Vec<CurrentOp>>,
    inflight: Cell<bool>,
    top_supported: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
}

fn label_column<F>(view: &gtk::ColumnView, title: &str, expand: bool, f: F)
where
    F: Fn(&CurrentOp, &gtk::Label) + 'static,
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
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
            return;
        };
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        f(&obj.borrow::<CurrentOp>(), &label);
    });
    let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    col.set_expand(expand);
    col.set_resizable(true);
    view.append_column(&col);
}

impl PerformancePane {
    pub fn new(app: &Rc<App>, conn: ConnectionId) -> Rc<Self> {
        let settings = app.config.borrow().settings.clone();
        let tip = |base: &str, id: &str| {
            let accel = crate::keybinds::accel_for(&settings, id);
            if accel.is_empty() {
                base.to_string()
            } else {
                format!("{base} ({})", crate::keybinds::pretty_accel(&accel))
            }
        };
        let pause_btn = gtk::ToggleButton::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text(tip("Pause sampling", "perf.pause"))
            .focus_on_click(false)
            .build();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(tip("Sample now", "perf.refresh"))
            .focus_on_click(false)
            .build();
        let kill = gtk::Button::builder()
            .label("Kill operation")
            .tooltip_text(tip("Kill the selected operation", "perf.kill"))
            .focus_on_click(false)
            .css_classes(["destructive-action"])
            .build();
        let status = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["viti-count"])
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.append(&status);
        bar.append(&refresh);
        bar.append(&pause_btn);
        bar.append(&kill);
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();

        let history: Rc<RefCell<VecDeque<Rates>>> = Rc::new(RefCell::new(VecDeque::new()));
        let charts: Vec<Chart> = CHARTS
            .iter()
            .map(|spec| build_chart(spec, history.clone()))
            .collect();
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(8)
            .column_homogeneous(true)
            .margin_start(8)
            .margin_end(8)
            .build();
        for (i, c) in charts.iter().enumerate() {
            grid.attach(&c.root, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        // Hottest collections take the sixth cell.
        let (hot_card, _) = card("Hottest collections");
        let hot_list = gtk::ListBox::new();
        hot_list.set_selection_mode(gtk::SelectionMode::None);
        hot_list.add_css_class("viti-perf-hot");
        let hot_empty = gtk::Label::builder()
            .label("No activity yet")
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        hot_card.append(&hot_empty);
        hot_card.append(&hot_list);
        grid.attach(&hot_card, 2, 1, 1, 1);

        // Slowest operations.
        let ops_store = gio::ListStore::new::<BoxedAnyObject>();
        let ops_selection = gtk::SingleSelection::new(Some(ops_store.clone()));
        ops_selection.set_autoselect(false);
        ops_selection.set_can_unselect(false);
        let ops_view = gtk::ColumnView::new(Some(ops_selection.clone()));
        ops_view.add_css_class("viti-table");
        ops_view.add_css_class("data-table");
        ops_view.set_can_focus(false);
        label_column(&ops_view, "Operation", true, |o, l| {
            l.set_text(&o.label());
            l.add_css_class("viti-mono");
        });
        label_column(&ops_view, "Running", false, |o, l| {
            l.set_text(&format!("{:.1} s", o.secs_running));
            if o.secs_running >= 10.0 {
                l.add_css_class("error");
            } else {
                l.remove_css_class("error");
            }
        });
        label_column(&ops_view, "Plan", false, |o, l| l.set_text(&o.plan_summary));
        label_column(&ops_view, "Client", false, |o, l| {
            l.set_text(&if o.app_name.is_empty() {
                o.client.clone()
            } else {
                format!("{} ({})", o.client, o.app_name)
            })
        });
        label_column(&ops_view, "Waiting", false, |o, l| {
            l.set_text(if o.waiting_for_lock { "lock" } else { "" })
        });
        let ops_empty = gtk::Label::builder()
            .label("No active operations")
            .xalign(0.0)
            .css_classes(["dim-label"])
            .margin_start(8)
            .margin_bottom(8)
            .build();
        let (ops_card, _) = card("Slowest active operations");
        ops_card.set_margin_start(8);
        ops_card.set_margin_end(8);
        ops_card.set_margin_bottom(8);
        ops_card.append(&ops_empty);
        ops_card.append(
            &gtk::ScrolledWindow::builder()
                .child(&ops_view)
                .min_content_height(140)
                .vexpand(true)
                .build(),
        );

        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&grid);
        body.append(&ops_card);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&body)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("viti-performance");
        root.append(&bar);
        root.append(&error);
        root.append(&scroller);

        let pane = Rc::new(Self {
            root,
            conn,
            page: RefCell::new(None),
            app: Rc::downgrade(app),
            me: RefCell::new(Weak::new()),
            pause_btn: pause_btn.clone(),
            status,
            error,
            charts,
            hot_list,
            hot_empty,
            ops_store,
            ops_selection,
            ops_view: ops_view.clone(),
            ops_empty,
            history,
            prev: RefCell::new(None),
            ops: RefCell::new(Vec::new()),
            inflight: Cell::new(false),
            top_supported: Cell::new(true),
            timer: RefCell::new(None),
        });
        *pane.me.borrow_mut() = Rc::downgrade(&pane);
        {
            let p = pane.clone();
            pause_btn.connect_toggled(move |b| {
                b.set_icon_name(if b.is_active() {
                    "media-playback-start-symbolic"
                } else {
                    "media-playback-pause-symbolic"
                });
                if !b.is_active() {
                    p.tick();
                }
            });
        }
        {
            let p = pane.clone();
            refresh.connect_clicked(move |_| p.tick());
        }
        {
            let p = pane.clone();
            kill.connect_clicked(move |_| p.kill_selected());
        }
        {
            let p = pane.clone();
            ops_view.connect_activate(move |_, _| p.peek());
        }
        {
            let root = pane.root.clone();
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.connect_pressed(move |_, _, _, _| {
                root.grab_focus();
            });
            ops_view.add_controller(click);
        }
        pane
    }

    fn app(&self) -> Option<Rc<App>> {
        self.app.upgrade()
    }

    fn me(&self) -> Option<Rc<Self>> {
        self.me.borrow().upgrade()
    }

    /// Start the 1 Hz sampling; stops by itself once the pane is dropped.
    pub fn start(&self) {
        if self.timer.borrow().is_some() {
            return;
        }
        self.tick();
        let me = self.me.borrow().clone();
        let id = glib::timeout_add_local(Duration::from_secs(1), move || match me.upgrade() {
            Some(p) => {
                if !p.pause_btn.is_active() {
                    p.tick();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *self.timer.borrow_mut() = Some(id);
    }

    pub fn stop(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }

    pub fn toggle_pause(&self) {
        self.pause_btn.set_active(!self.pause_btn.is_active());
    }

    /// One sample: serverStatus + $currentOp + top, in parallel on tokio.
    pub fn tick(&self) {
        if self.inflight.get() {
            return;
        }
        let Some(me) = self.me() else { return };
        let Some(app) = self.app() else { return };
        let Some(conn) = app.conn(self.conn) else {
            self.error.set_text("Disconnected");
            self.error.set_visible(true);
            self.stop();
            return;
        };
        self.inflight.set(true);
        let client = conn.client.clone();
        let want_top = self.top_supported.get();
        glib::spawn_future_local(async move {
            let r = crate::rt::io(async move {
                let at = Instant::now();
                let (status, ops, top) = tokio::join!(
                    ops::server_status(&client),
                    ops::current_ops(&client),
                    async {
                        if want_top {
                            ops::top(&client).await.map(Some)
                        } else {
                            Ok(None)
                        }
                    }
                );
                (at, status, ops, top)
            })
            .await;
            me.inflight.set(false);
            me.apply(r);
        });
    }

    #[allow(clippy::type_complexity)]
    fn apply(
        &self,
        (at, status, ops, top): (
            Instant,
            anyhow::Result<bson::Document>,
            anyhow::Result<Vec<bson::Document>>,
            anyhow::Result<Option<bson::Document>>,
        ),
    ) {
        let snap = match status {
            Ok(doc) => Snapshot::parse(&doc),
            Err(e) => {
                tracing::warn!("serverStatus: {e:#}");
                self.error.set_text(&format!("serverStatus: {e:#}"));
                self.error.set_visible(true);
                return;
            }
        };
        self.error.set_visible(false);
        let top_entries = match top {
            Ok(Some(doc)) => perf::parse_top(&doc),
            Ok(None) => Vec::new(),
            Err(e) => {
                // mongos and restricted users: stop asking.
                tracing::debug!("top: {e:#}; hottest collections disabled");
                self.top_supported.set(false);
                self.hot_empty
                    .set_text("Not available on this server (top refused)");
                Vec::new()
            }
        };
        let prev = self
            .prev
            .replace(Some((at, snap.clone(), top_entries.clone())));
        if let Some((pat, psnap, ptop)) = prev {
            let dt = at.duration_since(pat).as_secs_f64();
            let rates = Rates::between(&psnap, &snap, dt);
            for c in &self.charts {
                let vals = (c.spec.pick)(&rates);
                for (label, (name, v)) in c.legend.iter().zip(c.spec.series.iter().zip(vals)) {
                    label.set_text(&format!("{name} {}", (c.spec.fmt)(v)));
                }
                c.area.queue_draw();
            }
            {
                let mut h = self.history.borrow_mut();
                h.push_back(rates);
                while h.len() > HISTORY {
                    h.pop_front();
                }
            }
            if self.top_supported.get() {
                self.show_hot(perf::hottest(&ptop, &top_entries, dt, HOT_LIMIT));
            }
        }
        self.status.set_text(&format!(
            "{}  ·  {} {}  ·  up {}  ·  {} connection{} ({} available)",
            snap.host,
            snap.process,
            snap.version,
            perf::uptime_text(snap.uptime_s),
            snap.connections,
            if snap.connections == 1.0 { "" } else { "s" },
            perf::short_number(snap.connections_available),
        ));
        match ops {
            Ok(docs) => self.show_ops(perf::parse_current_ops(&docs)),
            Err(e) => {
                tracing::debug!("$currentOp: {e:#}");
                self.ops_empty
                    .set_text(&format!("Operations unavailable: {e:#}"));
                self.ops_empty.set_visible(true);
            }
        }
    }

    fn show_hot(&self, hot: Vec<HotCollection>) {
        while let Some(row) = self.hot_list.first_child() {
            self.hot_list.remove(&row);
        }
        self.hot_empty.set_visible(hot.is_empty());
        let max = hot.first().map(|h| h.ops_per_s).unwrap_or(1.0).max(1e-9);
        for h in hot {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(
                &gtk::Label::builder()
                    .label(&h.ns)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .css_classes(["viti-mono", "caption"])
                    .build(),
            );
            line.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{} op/s · {:.0}% read",
                        perf::short_number(h.ops_per_s),
                        h.read_share * 100.0
                    ))
                    .css_classes(["viti-count"])
                    .build(),
            );
            let bar = gtk::LevelBar::builder()
                .min_value(0.0)
                .max_value(1.0)
                .value((h.ops_per_s / max).clamp(0.0, 1.0))
                .build();
            bar.add_css_class("viti-perf-bar");
            row.append(&line);
            row.append(&bar);
            self.hot_list.append(&row);
        }
    }

    fn show_ops(&self, ops: Vec<CurrentOp>) {
        let selected = self
            .cursor()
            .and_then(|i| self.ops.borrow().get(i).map(|o| o.opid.clone()));
        self.ops_empty.set_visible(ops.is_empty());
        if ops.is_empty() {
            self.ops_empty.set_text("No active operations");
        }
        *self.ops.borrow_mut() = ops.clone();
        self.ops_store.remove_all();
        for o in &ops {
            self.ops_store.append(&BoxedAnyObject::new(o.clone()));
        }
        let keep = selected.and_then(|id| ops.iter().position(|o| o.opid == id));
        if let Some(i) = keep {
            self.ops_selection.set_selected(i as u32);
        }
    }

    // ----- vi keys ----------------------------------------------------------

    pub fn cursor(&self) -> Option<usize> {
        let i = self.ops_selection.selected();
        (i != gtk::INVALID_LIST_POSITION && (i as usize) < self.ops.borrow().len())
            .then_some(i as usize)
    }

    pub fn move_cursor(&self, delta: i64) {
        let n = self.ops_store.n_items() as i64;
        if n == 0 {
            return;
        }
        let cur = self.cursor().map(|c| c as i64).unwrap_or(-1);
        let next = (cur + delta).clamp(0, n - 1);
        self.ops_selection.set_selected(next as u32);
        self.ops_view
            .scroll_to(next as u32, None, gtk::ListScrollFlags::empty(), None);
    }

    pub fn top(&self) {
        if self.ops_store.n_items() > 0 {
            self.ops_selection.set_selected(0);
        }
    }

    pub fn bottom(&self) {
        let n = self.ops_store.n_items();
        if n > 0 {
            self.ops_selection.set_selected(n - 1);
        }
    }

    fn selected(&self) -> Option<CurrentOp> {
        self.cursor()
            .and_then(|i| self.ops.borrow().get(i).cloned())
    }

    /// `o` / Enter: the operation's command as JSON.
    pub fn peek(&self) {
        let Some(app) = self.app() else { return };
        let Some(op) = self.selected() else {
            app.toast("No operation selected");
            return;
        };
        let mut doc = bson::doc! {
            "opid": op.opid.clone(),
            "op": &op.op,
            "ns": &op.ns,
            "secs_running": op.secs_running,
            "active": op.active,
            "waitingForLock": op.waiting_for_lock,
            "client": &op.client,
        };
        if !op.app_name.is_empty() {
            doc.insert("appName", &op.app_name);
        }
        if !op.plan_summary.is_empty() {
            doc.insert("planSummary", &op.plan_summary);
        }
        doc.insert("command", op.command.clone());
        crate::ui::peek_document(&app, &op.label(), &ejson::pretty(&doc, Mode::Relaxed));
    }

    /// `Ctrl+D`: killOp after confirmation.
    pub fn kill_selected(&self) {
        let Some(app) = self.app() else { return };
        let Some(me) = self.me() else { return };
        let Some(op) = self.selected() else {
            app.toast("Select an operation to kill (j / k)");
            return;
        };
        let Some(conn) = app.conn(self.conn) else {
            app.toast("Not connected");
            return;
        };
        let client = conn.client.clone();
        let label = op.label();
        let opid = op.opid.clone();
        let window = app.window.clone();
        crate::ui::confirm(
            &window,
            &format!("Kill {label}?"),
            &format!(
                "Operation {} has been running for {:.1} s. killOp interrupts it; the client sees an error.",
                ejson::summary(&opid, 40),
                op.secs_running
            ),
            "Kill",
            true,
            move || {
                let client = client.clone();
                let opid = opid.clone();
                let app = app.clone();
                let me = me.clone();
                let label = label.clone();
                glib::spawn_future_local(async move {
                    let r = crate::rt::io(async move { ops::kill_op(&client, opid).await }).await;
                    match r {
                        Ok(()) => {
                            app.toast(&format!("Killed {label}"));
                            me.tick();
                        }
                        Err(e) => app.toast_error(&format!("killOp for {label}"), &e),
                    }
                });
            },
        );
    }
}
