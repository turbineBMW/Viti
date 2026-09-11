//! Database and collection management: create (standard, capped, time-series,
//! clustered, view; optional collation), drop with a typed confirmation, and
//! rename. Reached from the sidebar (`A`, `Ctrl+d`, `R`, context menu) and
//! the `:mkdb` / `:mkcoll` / `:drop` / `:rename` commands.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ejson;
use crate::mongo::ops::{self, CreateCollSpec, Namespace, TimeSeriesSpec};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::rc::Rc;

const KINDS: &[&str] = &["Standard", "Capped", "Time series", "Clustered", "View"];
const GRANULARITIES: &[&str] = &["seconds", "minutes", "hours"];

fn mono(title: &str) -> adw::EntryRow {
    let e = adw::EntryRow::builder().title(title).build();
    e.add_css_class("viti-mono");
    e
}

/// Create a collection in `db`; with `db == None` the dialog also asks for a
/// database name (MongoDB creates the database with its first collection).
pub fn create_collection(
    app: &Rc<App>,
    conn: ConnectionId,
    db: Option<String>,
    preset: Option<&str>,
) {
    create_collection_with(app, conn, db, preset, None);
}

/// "Create view" from the aggregations page: the View kind preselected with
/// the source collection and pipeline filled in.
pub fn create_view_from(app: &Rc<App>, conn: ConnectionId, ns: &Namespace, pipeline: &str) {
    create_collection_with(
        app,
        conn,
        Some(ns.db.clone()),
        Some("View"),
        Some((ns.coll.clone(), pipeline.to_string())),
    );
}

fn create_collection_with(
    app: &Rc<App>,
    conn: ConnectionId,
    db: Option<String>,
    preset: Option<&str>,
    view_seed: Option<(String, String)>,
) {
    let creating_db = db.is_none();
    let dialog = adw::Dialog::builder()
        .title(if creating_db {
            "New database"
        } else {
            "New collection"
        })
        .content_width(620)
        .content_height(640)
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
    let g = adw::PreferencesGroup::new();
    let db_row = mono("Database name");
    if let Some(d) = &db {
        db_row.set_text(d);
        db_row.set_editable(false);
    }
    let name = mono("Collection name");
    let kind = adw::ComboRow::builder().title("Type").build();
    kind.set_model(Some(&gtk::StringList::new(KINDS)));
    if let Some(p) = preset {
        kind.set_selected(KINDS.iter().position(|k| *k == p).unwrap_or(0) as u32);
    }
    g.add(&db_row);
    g.add(&name);
    g.add(&kind);
    page.add(&g);

    // Per-kind options; only the chosen kind's group is visible.
    let capped = adw::PreferencesGroup::builder().title("Capped").build();
    let cap_size = adw::SpinRow::with_range(1.0, 1e15, 1024.0);
    cap_size.set_title("Maximum size (bytes)");
    cap_size.set_value(1_048_576.0);
    let cap_max = adw::SpinRow::with_range(0.0, 1e12, 1.0);
    cap_max.set_title("Maximum documents (0 = unlimited)");
    capped.add(&cap_size);
    capped.add(&cap_max);
    page.add(&capped);

    let ts = adw::PreferencesGroup::builder()
        .title("Time series")
        .build();
    let ts_time = mono("Time field");
    ts_time.set_text("timestamp");
    let ts_meta = mono("Meta field (optional)");
    let ts_gran = adw::ComboRow::builder().title("Granularity").build();
    ts_gran.set_model(Some(&gtk::StringList::new(GRANULARITIES)));
    let ts_expire = adw::SpinRow::with_range(0.0, 1e12, 3600.0);
    ts_expire.set_title("Expire documents after (seconds, 0 = never)");
    ts.add(&ts_time);
    ts.add(&ts_meta);
    ts.add(&ts_gran);
    ts.add(&ts_expire);
    page.add(&ts);

    let clustered = adw::PreferencesGroup::builder()
        .title("Clustered")
        .description("Documents are stored ordered by _id (a unique clustered index).")
        .build();
    let cl_expire = adw::SpinRow::with_range(0.0, 1e12, 3600.0);
    cl_expire.set_title("Expire documents after (seconds, 0 = never)");
    clustered.add(&cl_expire);
    page.add(&clustered);

    let view = adw::PreferencesGroup::builder().title("View").build();
    let view_on = mono("Source collection");
    if let Some((on, _)) = &view_seed {
        view_on.set_text(on);
    }
    view.add(&view_on);
    let pipeline_view = crate::ui::json_view(
        view_seed
            .as_ref()
            .map(|(_, p)| p.as_str())
            .unwrap_or("[\n  { $match: { } }\n]"),
        true,
    );
    let pipeline_scroller = gtk::ScrolledWindow::builder()
        .child(&pipeline_view)
        .min_content_height(160)
        .css_classes(["card"])
        .build();
    let pipeline_label = gtk::Label::builder()
        .label("Pipeline")
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .margin_top(6)
        .build();
    view.add(&pipeline_label);
    view.add(&pipeline_scroller);
    page.add(&view);

    let coll_group = adw::PreferencesGroup::builder().title("Collation").build();
    let collation = adw::ExpanderRow::builder()
        .title("Custom collation")
        .show_enable_switch(true)
        .enable_expansion(false)
        .build();
    let collation_doc = mono("Collation document");
    collation_doc.set_text("{ locale: 'en', strength: 2 }");
    collation.add_row(&collation_doc);
    coll_group.add(&collation);
    page.add(&coll_group);

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
    let scroller = gtk::ScrolledWindow::builder()
        .child(&page)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    body.append(&scroller);
    body.append(&error);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let sync_kind = {
        let (capped, ts, clustered, view, coll_group) = (
            capped.clone(),
            ts.clone(),
            clustered.clone(),
            view.clone(),
            coll_group.clone(),
        );
        move |k: u32| {
            capped.set_visible(k == 1);
            ts.set_visible(k == 2);
            clustered.set_visible(k == 3);
            view.set_visible(k == 4);
            // Time-series and clustered collections take no collation.
            coll_group.set_visible(k != 2 && k != 3);
        }
    };
    sync_kind(kind.selected());
    kind.connect_selected_notify(move |k| sync_kind(k.selected()));

    let commit: Rc<dyn Fn()> = {
        let app = app.clone();
        let dialog = dialog.clone();
        let error = error.clone();
        let (db_row, name, kind) = (db_row.clone(), name.clone(), kind.clone());
        let (cap_size, cap_max) = (cap_size.clone(), cap_max.clone());
        let (ts_time, ts_meta, ts_gran, ts_expire) = (
            ts_time.clone(),
            ts_meta.clone(),
            ts_gran.clone(),
            ts_expire.clone(),
        );
        let cl_expire = cl_expire.clone();
        let (view_on, pipeline_view) = (view_on.clone(), pipeline_view.clone());
        let (collation, collation_doc) = (collation.clone(), collation_doc.clone());
        Rc::new(move || {
            let fail = |msg: &str| {
                error.set_text(msg);
                error.set_visible(true);
            };
            let db = db_row.text().trim().to_string();
            let coll = name.text().trim().to_string();
            if db.is_empty() || db.contains(['/', '\\', '.', ' ', '"', '$']) {
                return fail("Database names cannot be empty or contain / \\ . \" $ or spaces");
            }
            if coll.is_empty() || coll.starts_with("system.") || coll.contains('$') {
                return fail("Collection names cannot be empty, start with system. or contain $");
            }
            let mut spec = CreateCollSpec::default();
            match kind.selected() {
                1 => {
                    let max = cap_max.value() as u64;
                    spec.capped = Some((cap_size.value() as u64, (max > 0).then_some(max)));
                }
                2 => {
                    let tf = ts_time.text().trim().to_string();
                    if tf.is_empty() {
                        return fail("A time-series collection needs a time field");
                    }
                    let e = ts_expire.value() as u64;
                    spec.time_series = Some(TimeSeriesSpec {
                        time_field: tf,
                        meta_field: Some(ts_meta.text().trim().to_string())
                            .filter(|m| !m.is_empty()),
                        granularity: GRANULARITIES
                            .get(ts_gran.selected() as usize)
                            .map(|g| g.to_string()),
                        expire_after_seconds: (e > 0).then_some(e),
                    });
                }
                3 => {
                    spec.clustered = true;
                    let e = cl_expire.value() as u64;
                    spec.expire_after_seconds = (e > 0).then_some(e);
                }
                4 => {
                    let on = view_on.text().trim().to_string();
                    if on.is_empty() {
                        return fail("A view needs a source collection");
                    }
                    let text = crate::ui::buffer_text(&pipeline_view.buffer());
                    match ejson::parse_documents(&text) {
                        Ok(p) => spec.view = Some((on, p)),
                        Err(e) => return fail(&format!("pipeline: {e}")),
                    }
                }
                _ => {}
            }
            if collation.enables_expansion() && collation.is_visible() {
                match ejson::parse_document(&collation_doc.text()) {
                    Ok(c) => spec.collation = Some(c),
                    Err(e) => return fail(&format!("collation: {e}")),
                }
            }
            if app.write_guard().is_err() {
                return;
            }
            let Some(c) = app.conn(conn) else {
                return fail("Not connected");
            };
            let client = c.client.clone();
            let ns = Namespace::new(&db, &coll);
            let app = app.clone();
            let dialog = dialog.clone();
            glib::spawn_future_local(async move {
                let ns2 = ns.clone();
                let r = crate::rt::io(async move {
                    ops::create_collection(&client, &ns2.db, &ns2.coll, &spec).await
                })
                .await;
                match r {
                    Ok(()) => {
                        dialog.close();
                        app.toast(&format!("Created {ns}"));
                        app.after_namespace_change(conn, &ns.db);
                        app.open_collection(conn, ns);
                    }
                    Err(e) => app.toast_error(&format!("create {ns}"), &e),
                }
            });
        })
    };
    {
        let commit = commit.clone();
        create.connect_clicked(move |_| commit());
    }
    for e in [&db_row, &name, &view_on, &collation_doc] {
        let commit = commit.clone();
        e.connect_entry_activated(move |_| commit());
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
    if creating_db {
        db_row.grab_focus();
    } else {
        name.grab_focus();
    }
}

pub fn drop_database(app: &Rc<App>, conn: ConnectionId, db: &str) {
    if app.write_guard().is_err() {
        return;
    }
    let app = app.clone();
    let db = db.to_string();
    let db2 = db.clone();
    crate::ui::confirm_typed(
        &app.window.clone(),
        "Drop database?",
        &format!("Every collection in “{db}” will be deleted. Type the database name to confirm."),
        &db,
        "Drop database",
        move || {
            let Some(c) = app.conn(conn) else { return };
            let client = c.client.clone();
            let db = db2.clone();
            let app = app.clone();
            glib::spawn_future_local(async move {
                let db3 = db.clone();
                match crate::rt::io(async move { ops::drop_database(&client, &db3).await }).await {
                    Ok(()) => {
                        app.close_tabs(conn, Some(&db), None);
                        app.toast(&format!("Dropped database {db}"));
                        app.load_databases(conn);
                    }
                    Err(e) => app.toast_error(&format!("drop database {db}"), &e),
                }
            });
        },
    );
}

pub fn drop_collection(app: &Rc<App>, conn: ConnectionId, ns: Namespace) {
    if app.write_guard().is_err() {
        return;
    }
    let app = app.clone();
    let ns2 = ns.clone();
    crate::ui::confirm_typed(
        &app.window.clone(),
        "Drop collection?",
        &format!(
            "All documents and indexes of {ns} will be deleted. Type the collection name to confirm."
        ),
        &ns.coll,
        "Drop collection",
        move || {
            let Some(c) = app.conn(conn) else { return };
            let client = c.client.clone();
            let ns = ns2.clone();
            let app = app.clone();
            glib::spawn_future_local(async move {
                let ns3 = ns.clone();
                match crate::rt::io(async move { ops::drop_collection(&client, &ns3).await }).await
                {
                    Ok(()) => {
                        app.close_tabs(conn, Some(&ns.db), Some(&ns.coll));
                        app.toast(&format!("Dropped {ns}"));
                        app.after_namespace_change(conn, &ns.db);
                    }
                    Err(e) => app.toast_error(&format!("drop {ns}"), &e),
                }
            });
        },
    );
}

pub fn rename_collection(app: &Rc<App>, conn: ConnectionId, ns: Namespace) {
    if app.write_guard().is_err() {
        return;
    }
    let dialog = adw::AlertDialog::new(
        Some(&format!("Rename {}", ns)),
        Some("Open tabs on this collection are closed; views and indexes follow the collection."),
    );
    let entry = gtk::Entry::builder()
        .text(&ns.coll)
        .activates_default(true)
        .css_classes(["viti-mono"])
        .build();
    let drop_target = gtk::CheckButton::with_label("Replace an existing collection with that name");
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 8);
    extra.append(&entry);
    extra.append(&drop_target);
    dialog.set_extra_child(Some(&extra));
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");
    let window = app.window.clone();
    let app = app.clone();
    let entry2 = entry.clone();
    dialog.connect_response(None, move |_, r| {
        if r != "rename" {
            return;
        }
        let new_name = entry2.text().trim().to_string();
        if new_name.is_empty() || new_name == ns.coll {
            return;
        }
        let Some(c) = app.conn(conn) else { return };
        let client = c.client.clone();
        let drop = drop_target.is_active();
        let ns = ns.clone();
        let app = app.clone();
        glib::spawn_future_local(async move {
            let ns2 = ns.clone();
            let nn = new_name.clone();
            match crate::rt::io(
                async move { ops::rename_collection(&client, &ns2, &nn, drop).await },
            )
            .await
            {
                Ok(()) => {
                    app.close_tabs(conn, Some(&ns.db), Some(&ns.coll));
                    app.toast(&format!("Renamed {ns} to {new_name}"));
                    app.after_namespace_change(conn, &ns.db);
                    app.open_collection(conn, Namespace::new(&ns.db, &new_name));
                }
                Err(e) => app.toast_error(&format!("rename {ns}"), &e),
            }
        });
    });
    dialog.present(Some(&window));
    entry.grab_focus();
}
