//! "Export" dialog (`X`, `:export [json|csv]`): the whole collection, the
//! current query or the aggregation results, to JSON (three Extended JSON
//! flavours, array or one per line) or CSV (chosen fields, delimiter, formula
//! escaping). The export streams on tokio with progress in the banner, where
//! Cancel aborts it, kills the server-side cursor and removes the partial file.
use crate::app::{App, LongOp};
use crate::events::Event;
use crate::mongo::ConnectionId;
use crate::mongo::export::{self, DELIMITERS, ExportSpec, Format, JsonMode, Source};
use crate::mongo::ops::{Namespace, OpCtx};
use crate::ui::aggregation::AggregationPane;
use crate::ui::documents::DocumentsPane;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// Where the export was started from: decides the sources offered.
pub enum Context {
    Documents(Rc<DocumentsPane>),
    Aggregation(Rc<AggregationPane>),
}

/// Documents sampled to discover CSV fields.
const FIELD_SAMPLE: u64 = 50;
/// Exports are not subject to the per-query cap: a day.
const EXPORT_MAX_TIME_MS: u64 = 24 * 3600 * 1000;

struct Dialog {
    dialog: adw::Dialog,
    conn: ConnectionId,
    ns: Namespace,
    sources: Vec<Source>,
    source_buttons: Vec<gtk::CheckButton>,
    format: adw::ComboRow,
    json_group: adw::PreferencesGroup,
    json_mode: adw::ComboRow,
    ndjson: adw::SwitchRow,
    csv_group: adw::PreferencesGroup,
    delimiter: adw::ComboRow,
    escape: adw::SwitchRow,
    fields_group: adw::PreferencesGroup,
    fields_list: gtk::ListBox,
    fields_spinner: gtk::Spinner,
    fields_status: gtk::Label,
    field_checks: RefCell<Vec<(String, gtk::CheckButton)>>,
    export_btn: gtk::Button,
    app: Rc<App>,
}

impl Dialog {
    fn source(&self) -> Source {
        let i = self
            .source_buttons
            .iter()
            .position(|b| b.is_active())
            .unwrap_or(0);
        self.sources.get(i).cloned().unwrap_or(Source::Full)
    }

    fn is_csv(&self) -> bool {
        self.format.selected() == 1
    }

    fn update_format(self: &Rc<Self>) {
        let csv = self.is_csv();
        self.json_group.set_visible(!csv);
        self.csv_group.set_visible(csv);
        self.fields_group.set_visible(csv);
        if csv && self.field_checks.borrow().is_empty() {
            self.load_fields();
        }
    }

    /// Sample the source and list its flattened fields as check rows.
    fn load_fields(self: &Rc<Self>) {
        let Some(conn) = self.app.conn(self.conn) else {
            return;
        };
        let client = conn.client.clone();
        let ns = self.ns.clone();
        let source = self.source();
        let ctx = OpCtx::new(self.app.max_time_ms());
        self.fields_spinner.set_visible(true);
        self.fields_spinner.set_spinning(true);
        self.fields_status
            .set_text(&format!("Reading the first {FIELD_SAMPLE} documents…"));
        let me = self.clone();
        glib::spawn_future_local(async move {
            let ns2 = ns.clone();
            let r = crate::rt::io(async move {
                export::sample(&client, &ns2, &source, FIELD_SAMPLE, &ctx).await
            })
            .await;
            me.fields_spinner.set_visible(false);
            me.fields_spinner.set_spinning(false);
            match r {
                Ok(docs) => {
                    let fields = export::csv_fields(&docs);
                    me.fields_status.set_text(&format!(
                        "{} field{} found in the first {} document{}; fields only present in later documents are not listed. Nested fields are flattened with dots, array elements get an index.",
                        fields.len(),
                        if fields.len() == 1 { "" } else { "s" },
                        docs.len(),
                        if docs.len() == 1 { "" } else { "s" }
                    ));
                    me.set_fields(fields);
                }
                Err(e) => {
                    me.fields_status
                        .set_text(&format!("Could not sample: {e:#}"));
                    me.app.toast_error(&format!("export sample of {ns}"), &e);
                }
            }
        });
    }

    fn set_fields(&self, fields: Vec<String>) {
        while let Some(row) = self.fields_list.first_child() {
            self.fields_list.remove(&row);
        }
        let mut checks = Vec::new();
        for f in fields {
            let check = gtk::CheckButton::builder().active(true).build();
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&f))
                .activatable_widget(&check)
                .build();
            row.add_css_class("viti-mono");
            row.add_prefix(&check);
            self.fields_list.append(&row);
            checks.push((f, check));
        }
        *self.field_checks.borrow_mut() = checks;
    }

    fn set_all_fields(&self, on: bool) {
        for (_, c) in self.field_checks.borrow().iter() {
            c.set_active(on);
        }
    }

    fn selected_fields(&self) -> Vec<String> {
        self.field_checks
            .borrow()
            .iter()
            .filter(|(_, c)| c.is_active())
            .map(|(f, _)| f.clone())
            .collect()
    }

    fn format_spec(&self) -> Option<Format> {
        if self.is_csv() {
            let fields = self.selected_fields();
            if fields.is_empty() {
                self.app.toast("Pick at least one field");
                return None;
            }
            Some(Format::Csv {
                fields,
                delimiter: DELIMITERS
                    .get(self.delimiter.selected() as usize)
                    .map(|d| d.1)
                    .unwrap_or(b','),
                escape_formulas: self.escape.is_active(),
            })
        } else {
            Some(Format::Json {
                mode: JsonMode::from_index(self.json_mode.selected()),
                ndjson: self.ndjson.is_active(),
            })
        }
    }

    fn suggested_name(&self) -> String {
        let ext = if self.is_csv() {
            "csv"
        } else if self.ndjson.is_active() {
            "ndjson"
        } else {
            "json"
        };
        format!("{}.{}.{ext}", self.ns.db, self.ns.coll)
    }

    /// "Export…": pick a file, then run.
    fn pick_and_run(self: &Rc<Self>) {
        let Some(format) = self.format_spec() else {
            return;
        };
        let source = self.source();
        let chooser = gtk::FileDialog::builder()
            .title("Export to")
            .modal(true)
            .initial_name(self.suggested_name())
            .build();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let f = gtk::FileFilter::new();
        if self.is_csv() {
            f.set_name(Some("CSV"));
            f.add_pattern("*.csv");
        } else {
            f.set_name(Some("JSON"));
            f.add_pattern("*.json");
            f.add_pattern("*.ndjson");
            f.add_pattern("*.jsonl");
        }
        filters.append(&f);
        chooser.set_filters(Some(&filters));
        let me = self.clone();
        glib::spawn_future_local(async move {
            let Ok(file) = chooser.save_future(Some(&me.app.window)).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            me.dialog.close();
            start(
                &me.app,
                me.conn,
                me.ns.clone(),
                ExportSpec {
                    source,
                    format,
                    path,
                },
            );
        });
    }
}

/// Open the dialog. `format` preselects `"json"` or `"csv"`.
pub fn show(app: &Rc<App>, conn: ConnectionId, ns: Namespace, ctx: Context, format: Option<&str>) {
    let dialog = adw::Dialog::builder()
        .title(format!("Export {ns}"))
        .content_width(560)
        .content_height(640)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let export_btn = gtk::Button::builder()
        .label("Export…")
        .css_classes(["suggested-action"])
        .build();
    header.pack_end(&export_btn);
    toolbar.add_top_bar(&header);
    let page = adw::PreferencesPage::new();

    // --- what ---
    let what = adw::PreferencesGroup::builder().title("Documents").build();
    let mut sources = Vec::new();
    let mut source_buttons = Vec::new();
    match &ctx {
        Context::Documents(docs) => {
            let q = docs.current_query();
            let full = gtk::CheckButton::new();
            let full_row = adw::ActionRow::builder()
                .title("Full collection")
                .subtitle("Every document, natural order")
                .activatable_widget(&full)
                .build();
            full_row.add_prefix(&full);
            what.add(&full_row);
            sources.push(Source::Full);
            source_buttons.push(full.clone());
            let query = gtk::CheckButton::builder().group(&full).build();
            let query_row = adw::ActionRow::builder()
                .title("Current query")
                .subtitle(glib::markup_escape_text(&q.summary()))
                .activatable_widget(&query)
                .build();
            query_row.add_prefix(&query);
            what.add(&query_row);
            sources.push(Source::Find(Box::new(docs.current_spec())));
            source_buttons.push(query.clone());
            if q.is_default() {
                query_row.set_sensitive(false);
                query_row.set_subtitle("The query bar is empty");
                full.set_active(true);
            } else {
                query.set_active(true);
            }
        }
        Context::Aggregation(agg) => {
            let Some((pipeline, opts)) = agg.pipeline_for_run() else {
                return;
            };
            let check = gtk::CheckButton::builder().active(true).build();
            let row = adw::ActionRow::builder()
                .title("Aggregation results")
                .subtitle(format!(
                    "The pipeline is run again and streamed ({} stage{})",
                    pipeline.len(),
                    if pipeline.len() == 1 { "" } else { "s" }
                ))
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            what.add(&row);
            sources.push(Source::Aggregate(pipeline, opts));
            source_buttons.push(check);
        }
    }
    page.add(&what);

    // --- format ---
    let fmt_group = adw::PreferencesGroup::builder().title("Format").build();
    let format_row = adw::ComboRow::builder()
        .title("File type")
        .model(&gtk::StringList::new(&["JSON", "CSV"]))
        .build();
    format_row.set_selected(u32::from(format == Some("csv")));
    fmt_group.add(&format_row);
    page.add(&fmt_group);

    let json_group = adw::PreferencesGroup::builder().title("JSON").build();
    let labels: Vec<&str> = JsonMode::ALL.iter().map(|m| m.label()).collect();
    let json_mode = adw::ComboRow::builder()
        .title("Extended JSON")
        .subtitle(JsonMode::Default.description())
        .model(&gtk::StringList::new(&labels))
        .build();
    {
        let jm = json_mode.clone();
        json_mode.connect_selected_notify(move |r| {
            jm.set_subtitle(JsonMode::from_index(r.selected()).description());
        });
    }
    json_group.add(&json_mode);
    let ndjson = adw::SwitchRow::builder()
        .title("One document per line")
        .subtitle("Newline-delimited JSON instead of one array")
        .build();
    json_group.add(&ndjson);
    page.add(&json_group);

    let csv_group = adw::PreferencesGroup::builder().title("CSV").build();
    let delim_labels: Vec<&str> = DELIMITERS.iter().map(|d| d.0).collect();
    let delimiter = adw::ComboRow::builder()
        .title("Delimiter")
        .model(&gtk::StringList::new(&delim_labels))
        .build();
    csv_group.add(&delimiter);
    let escape = adw::SwitchRow::builder()
        .title("Escape spreadsheet formulas")
        .subtitle("Prefix values starting with =, +, -, @ so spreadsheets do not run them")
        .active(true)
        .build();
    csv_group.add(&escape);
    page.add(&csv_group);

    let fields_group = adw::PreferencesGroup::builder().title("Fields").build();
    let all_btn = gtk::Button::builder()
        .label("All")
        .css_classes(["flat"])
        .build();
    let none_btn = gtk::Button::builder()
        .label("None")
        .css_classes(["flat"])
        .build();
    let fields_spinner = gtk::Spinner::builder().visible(false).build();
    let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header_box.append(&fields_spinner);
    header_box.append(&all_btn);
    header_box.append(&none_btn);
    fields_group.set_header_suffix(Some(&header_box));
    let fields_status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .margin_bottom(6)
        .build();
    fields_group.add(&fields_status);
    let fields_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    fields_group.add(&fields_list);
    page.add(&fields_group);

    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let d = Rc::new(Dialog {
        dialog: dialog.clone(),
        conn,
        ns,
        sources,
        source_buttons,
        format: format_row.clone(),
        json_group,
        json_mode,
        ndjson,
        csv_group,
        delimiter,
        escape,
        fields_group,
        fields_list,
        fields_spinner,
        fields_status,
        field_checks: RefCell::new(Vec::new()),
        export_btn: export_btn.clone(),
        app: app.clone(),
    });
    d.update_format();
    {
        let d = d.clone();
        format_row.connect_selected_notify(move |_| d.update_format());
    }
    for b in &d.source_buttons {
        let d2 = d.clone();
        b.connect_toggled(move |b| {
            if b.is_active() && d2.is_csv() {
                d2.load_fields();
            }
        });
    }
    {
        let d = d.clone();
        all_btn.connect_clicked(move |_| d.set_all_fields(true));
    }
    {
        let d = d.clone();
        none_btn.connect_clicked(move |_| d.set_all_fields(false));
    }
    {
        let d = d.clone();
        export_btn.connect_clicked(move |_| d.pick_and_run());
    }
    dialog.present(Some(&app.window));
    d.export_btn.grab_focus();
}

/// Run an export in the background, with the banner showing progress.
pub fn start(app: &Rc<App>, conn: ConnectionId, ns: Namespace, spec: ExportSpec) {
    let Some(c) = app.conn(conn) else {
        app.toast("Not connected");
        return;
    };
    let client = c.client.clone();
    let path: PathBuf = spec.path.clone();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = format!("Exporting {} of {ns} to {name}", spec.source.label());
    let op = uuid::Uuid::new_v4();
    let ctx = OpCtx::new(EXPORT_MAX_TIME_MS);
    let tx = app.events.clone();
    let ns2 = ns.clone();
    let ctx2 = ctx.clone();
    let label2 = label.clone();
    let handle = crate::rt::spawn(async move {
        export::run(&client, &ns2, spec, &ctx2, move |n| {
            let _ = tx.try_send(Event::Progress {
                op,
                done: n,
                total: None,
                label: label2.clone(),
            });
        })
        .await
    });
    app.add_long_op(
        op,
        LongOp {
            label: label.clone(),
            conn,
            ctx: Some(ctx),
            handle: handle.abort_handle(),
            partial_file: Some(path.clone()),
        },
    );
    let app = app.clone();
    glib::spawn_future_local(async move {
        let r = handle.await;
        match r {
            Ok(Ok(n)) => {
                app.finish_long_op(op);
                let msg = format!(
                    "Exported {} document{} to {}",
                    crate::ui::thousands(n),
                    if n == 1 { "" } else { "s" },
                    path.display()
                );
                app.toast(&msg);
                app.notify_if_unfocused("export", "Export finished", &msg);
            }
            Ok(Err(e)) => {
                app.finish_long_op(op);
                let _ = std::fs::remove_file(&path);
                app.toast_error(&format!("export of {ns}"), &e);
            }
            // Aborted: `cancel_long_ops` already cleaned up.
            Err(_) => {}
        }
    });
}
