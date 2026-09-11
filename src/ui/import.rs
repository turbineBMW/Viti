//! "Import" dialog (`I`, `:import [path]`): a JSON file (array, single
//! document or one per line) or a CSV file with a type per column (guessed
//! from a preview, editable), columns to skip, empty cells dropped or kept and
//! stop-on-error. Inserts run in batches on tokio with progress in the
//! banner; the report lists the first problems.
use crate::app::{App, LongOp};
use crate::events::Event;
use crate::mongo::ConnectionId;
use crate::mongo::export::DELIMITERS;
use crate::mongo::import::{self, Column, CsvOptions, FieldType, Format, ImportSpec, Preview};
use crate::mongo::ops::Namespace;
use crate::ui::documents::DocumentsPane;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const PREVIEW_ROWS: usize = 20;

struct Dialog {
    dialog: adw::Dialog,
    app: Rc<App>,
    conn: ConnectionId,
    ns: Namespace,
    docs: Option<Rc<DocumentsPane>>,
    file_row: adw::ActionRow,
    format: adw::ComboRow,
    csv_group: adw::PreferencesGroup,
    delimiter: adw::ComboRow,
    columns_status: gtk::Label,
    columns_list: gtk::ListBox,
    ignore_empty: adw::SwitchRow,
    stop_on_error: adw::SwitchRow,
    import_btn: gtk::Button,
    path: RefCell<Option<PathBuf>>,
    preview: RefCell<Option<Preview>>,
    /// (name, include, type) per column, in file order.
    column_widgets: RefCell<Vec<(String, gtk::CheckButton, gtk::DropDown)>>,
}

impl Dialog {
    fn effective_format(&self) -> Option<Format> {
        match self.format.selected() {
            1 => Some(Format::Json),
            2 => Some(Format::Csv),
            _ => self
                .path
                .borrow()
                .as_ref()
                .map(|p| import::format_for_path(p)),
        }
    }

    fn chosen_delimiter(&self) -> Option<u8> {
        let i = self.delimiter.selected();
        if i == 0 {
            None
        } else {
            DELIMITERS.get(i as usize - 1).map(|d| d.1)
        }
    }

    fn set_path(&self, p: PathBuf) {
        self.file_row
            .set_subtitle(&glib::markup_escape_text(&p.to_string_lossy()));
        *self.path.borrow_mut() = Some(p);
        self.update();
    }

    /// Show/hide the CSV section and (re)build the column rows.
    fn update(&self) {
        let format = self.effective_format();
        let has_path = self.path.borrow().is_some();
        self.import_btn.set_sensitive(has_path);
        let csv = format == Some(Format::Csv);
        self.csv_group.set_visible(csv);
        if !csv {
            return;
        }
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        match import::preview_csv(&path, self.chosen_delimiter(), PREVIEW_ROWS) {
            Ok(pv) => {
                let name = DELIMITERS
                    .iter()
                    .find(|d| d.1 == pv.delimiter)
                    .map(|d| d.0)
                    .unwrap_or("?");
                self.delimiter.set_subtitle(&format!("Detected: {name}"));
                self.columns_status.set_text(&format!(
                    "{} column{}; types guessed from the first {} row{}. Dotted names become nested fields.",
                    pv.headers.len(),
                    if pv.headers.len() == 1 { "" } else { "s" },
                    pv.rows.len(),
                    if pv.rows.len() == 1 { "" } else { "s" }
                ));
                self.set_columns(&pv);
                *self.preview.borrow_mut() = Some(pv);
            }
            Err(e) => {
                self.columns_status
                    .set_text(&format!("Could not read the file: {e:#}"));
                self.set_columns(&Preview::default());
                *self.preview.borrow_mut() = None;
            }
        }
    }

    fn set_columns(&self, pv: &Preview) {
        while let Some(row) = self.columns_list.first_child() {
            self.columns_list.remove(&row);
        }
        let labels: Vec<&str> = FieldType::ALL.iter().map(|t| t.label()).collect();
        let mut widgets = Vec::new();
        for (i, h) in pv.headers.iter().enumerate() {
            let samples: Vec<String> = pv
                .rows
                .iter()
                .filter_map(|r| r.get(i))
                .filter(|v| !v.trim().is_empty())
                .take(3)
                .map(|v| crate::mongo::ejson::truncate(v, 30))
                .collect();
            let check = gtk::CheckButton::builder().active(true).build();
            let types = gtk::DropDown::from_strings(&labels);
            types.set_selected(pv.guessed.get(i).copied().unwrap_or_default().index());
            types.set_valign(gtk::Align::Center);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(if h.is_empty() {
                    "(unnamed)"
                } else {
                    h
                }))
                .subtitle(glib::markup_escape_text(&samples.join("  ·  ")))
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            row.add_suffix(&types);
            if h.is_empty() {
                check.set_active(false);
                row.set_sensitive(false);
            }
            self.columns_list.append(&row);
            widgets.push((h.clone(), check, types));
        }
        *self.column_widgets.borrow_mut() = widgets;
    }

    fn columns(&self) -> Vec<Column> {
        self.column_widgets
            .borrow()
            .iter()
            .map(|(name, check, types)| Column {
                name: name.clone(),
                field_type: FieldType::from_index(types.selected()),
                include: check.is_active(),
            })
            .collect()
    }

    fn set_all(&self, on: bool) {
        for (name, check, _) in self.column_widgets.borrow().iter() {
            if !name.is_empty() {
                check.set_active(on);
            }
        }
    }

    fn choose_file(self: &Rc<Self>) {
        let chooser = gtk::FileDialog::builder()
            .title("Import from")
            .modal(true)
            .build();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let all = gtk::FileFilter::new();
        all.set_name(Some("JSON and CSV"));
        for p in ["*.json", "*.ndjson", "*.jsonl", "*.csv", "*.tsv"] {
            all.add_pattern(p);
        }
        filters.append(&all);
        let any = gtk::FileFilter::new();
        any.set_name(Some("All files"));
        any.add_pattern("*");
        filters.append(&any);
        chooser.set_filters(Some(&filters));
        let me = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(f) = chooser.open_future(Some(&me.app.window)).await
                && let Some(p) = f.path()
            {
                me.set_path(p);
            }
        });
    }

    fn run(self: &Rc<Self>) {
        if self.app.write_guard().is_err() {
            return;
        }
        let Some(path) = self.path.borrow().clone() else {
            self.choose_file();
            return;
        };
        let Some(format) = self.effective_format() else {
            return;
        };
        let csv = if format == Format::Csv {
            let Some(pv) = self.preview.borrow().clone() else {
                self.app.toast("The CSV file could not be read");
                return;
            };
            let columns = self.columns();
            if !columns.iter().any(|c| c.include) {
                self.app.toast("Include at least one column");
                return;
            }
            Some(CsvOptions {
                delimiter: pv.delimiter,
                columns,
            })
        } else {
            None
        };
        let spec = ImportSpec {
            path,
            format,
            csv,
            ignore_empty: self.ignore_empty.is_active(),
            stop_on_error: self.stop_on_error.is_active(),
        };
        self.dialog.close();
        start(
            &self.app,
            self.conn,
            self.ns.clone(),
            spec,
            self.docs.clone(),
        );
    }
}

/// Open the dialog; with `path` the file is preselected.
pub fn show(
    app: &Rc<App>,
    conn: ConnectionId,
    ns: Namespace,
    docs: Option<Rc<DocumentsPane>>,
    path: Option<PathBuf>,
) {
    if app.write_guard().is_err() {
        return;
    }
    let dialog = adw::Dialog::builder()
        .title(format!("Import into {ns}"))
        .content_width(600)
        .content_height(680)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let import_btn = gtk::Button::builder()
        .label("Import")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    header.pack_end(&import_btn);
    toolbar.add_top_bar(&header);
    let page = adw::PreferencesPage::new();

    let file_group = adw::PreferencesGroup::builder().title("File").build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    let file_row = adw::ActionRow::builder()
        .title("File")
        .subtitle("A JSON array, one document per line, or CSV with a header row")
        .activatable_widget(&choose)
        .build();
    file_row.add_suffix(&choose);
    file_group.add(&file_row);
    let format = adw::ComboRow::builder()
        .title("Format")
        .subtitle("Auto: by file extension")
        .model(&gtk::StringList::new(&["Auto", "JSON", "CSV"]))
        .build();
    file_group.add(&format);
    page.add(&file_group);

    let csv_group = adw::PreferencesGroup::builder()
        .title("CSV columns")
        .visible(false)
        .build();
    let mut delim_labels = vec!["Auto"];
    delim_labels.extend(DELIMITERS.iter().map(|d| d.0));
    let delimiter = adw::ComboRow::builder()
        .title("Delimiter")
        .model(&gtk::StringList::new(&delim_labels))
        .build();
    csv_group.add(&delimiter);
    let columns_status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .margin_top(6)
        .margin_bottom(6)
        .build();
    csv_group.add(&columns_status);
    let all_btn = gtk::Button::builder()
        .label("All")
        .css_classes(["flat"])
        .build();
    let none_btn = gtk::Button::builder()
        .label("None")
        .css_classes(["flat"])
        .build();
    let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    header_box.append(&all_btn);
    header_box.append(&none_btn);
    csv_group.set_header_suffix(Some(&header_box));
    let columns_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    csv_group.add(&columns_list);
    page.add(&csv_group);

    let options = adw::PreferencesGroup::builder().title("Options").build();
    let ignore_empty = adw::SwitchRow::builder()
        .title("Ignore empty cells")
        .subtitle("Leave the field out instead of importing an empty string (CSV)")
        .active(true)
        .build();
    options.add(&ignore_empty);
    let stop_on_error = adw::SwitchRow::builder()
        .title("Stop on errors")
        .subtitle("Abort at the first bad row or rejected insert; off skips them and reports")
        .active(true)
        .build();
    options.add(&stop_on_error);
    page.add(&options);

    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let d = Rc::new(Dialog {
        dialog: dialog.clone(),
        app: app.clone(),
        conn,
        ns,
        docs,
        file_row,
        format: format.clone(),
        csv_group,
        delimiter: delimiter.clone(),
        columns_status,
        columns_list,
        ignore_empty,
        stop_on_error,
        import_btn: import_btn.clone(),
        path: RefCell::new(None),
        preview: RefCell::new(None),
        column_widgets: RefCell::new(Vec::new()),
    });
    {
        let d = d.clone();
        choose.connect_clicked(move |_| d.choose_file());
    }
    {
        let d = d.clone();
        format.connect_selected_notify(move |_| d.update());
    }
    {
        let d = d.clone();
        delimiter.connect_selected_notify(move |_| d.update());
    }
    {
        let d = d.clone();
        all_btn.connect_clicked(move |_| d.set_all(true));
    }
    {
        let d = d.clone();
        none_btn.connect_clicked(move |_| d.set_all(false));
    }
    {
        let d = d.clone();
        import_btn.connect_clicked(move |_| d.run());
    }
    dialog.present(Some(&app.window));
    match path {
        Some(p) => d.set_path(p),
        None => d.choose_file(),
    }
}

/// Run an import in the background; the banner shows progress and the
/// Documents page reloads when it is done.
pub fn start(
    app: &Rc<App>,
    conn: ConnectionId,
    ns: Namespace,
    spec: ImportSpec,
    docs: Option<Rc<DocumentsPane>>,
) {
    let Some(c) = app.conn(conn) else {
        app.toast("Not connected");
        return;
    };
    let client = c.client.clone();
    let name = spec
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = format!("Importing {name} into {ns}");
    let op = uuid::Uuid::new_v4();
    let tx = app.events.clone();
    let ns2 = ns.clone();
    let label2 = label.clone();
    let handle = crate::rt::spawn(async move {
        import::run(&client, &ns2, spec, move |done, total| {
            let _ = tx.try_send(Event::Progress {
                op,
                done,
                total,
                label: label2.clone(),
            });
        })
        .await
    });
    app.add_long_op(
        op,
        LongOp {
            label,
            conn,
            ctx: None,
            handle: handle.abort_handle(),
            partial_file: None,
        },
    );
    let app = app.clone();
    glib::spawn_future_local(async move {
        let r = handle.await;
        if let Some(d) = &docs {
            d.load();
        }
        match r {
            Ok(Ok(report)) => {
                app.finish_long_op(op);
                let summary = format!(
                    "Imported {} document{} into {ns}{}",
                    crate::ui::thousands(report.inserted),
                    if report.inserted == 1 { "" } else { "s" },
                    if report.failed > 0 {
                        format!(", {} failed", crate::ui::thousands(report.failed))
                    } else {
                        String::new()
                    }
                );
                tracing::info!("{summary}");
                app.notify_if_unfocused("import", "Import finished", &summary);
                if report.failed == 0 && !report.stopped {
                    app.toast(&summary);
                } else {
                    let mut body = report.errors.join("\n");
                    let more = report.failed.saturating_sub(report.errors.len() as u64);
                    if more > 0 {
                        body.push_str(&format!("\n… and {more} more"));
                    }
                    if report.stopped {
                        body.push_str("\n\nStopped at the first error (Options › Stop on errors).");
                    }
                    let dialog = adw::AlertDialog::new(Some(&summary), Some(&body));
                    dialog.add_responses(&[("ok", "OK")]);
                    dialog.set_default_response(Some("ok"));
                    dialog.present(Some(&app.window));
                }
            }
            Ok(Err(e)) => {
                app.finish_long_op(op);
                app.toast_error(&format!("import into {ns}"), &e);
            }
            Err(_) => {}
        }
    });
}
