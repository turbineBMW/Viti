//! Bulk operations on everything the query bar's filter matches: update
//! (with a before/after preview and a matched count) and delete (with the
//! count and a sample of what goes). Both go through `App::write_guard`.
use crate::app::App;
use crate::mongo::ejson::{self, Mode};
use crate::mongo::ops::{self, FindSpec, OpCtx, PreviewKind, UpdateSpec};
use crate::ui::documents::DocumentsPane;
use adw::prelude::*;
use bson::Document;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const PREVIEW_DOCS: u64 = 5;

fn plural(n: u64, word: &str) -> String {
    format!(
        "{} {word}{}",
        crate::ui::thousands(n),
        if n == 1 { "" } else { "s" }
    )
}

/// Filter + collation of the pane's current query, or the text of `entry` if given.
fn filter_of(text: &str, docs: &DocumentsPane) -> Result<(Document, Option<Document>), String> {
    let mut q = docs.current_query();
    q.filter = text.to_string();
    let spec = FindSpec::from_query(&q, "")?;
    Ok((spec.filter, spec.collation))
}

/// `u`: update every document matching the filter.
pub fn update_dialog(app: &Rc<App>, docs: &Rc<DocumentsPane>, initial: Option<String>) {
    if app.write_guard().is_err() {
        return;
    }
    let ns = docs.ns.clone();
    let dialog = adw::Dialog::builder()
        .title(format!("Update documents in {ns}"))
        .content_width((app.window.width() - 120).clamp(720, 1200))
        .content_height((app.window.height() - 100).clamp(560, 900))
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::builder()
        .label("Update")
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&run);
    toolbar.add_top_bar(&header);

    let filter = gtk::Entry::builder()
        .text(docs.current_query().filter)
        .placeholder_text("{ }  — every document")
        .hexpand(true)
        .css_classes(["viti-mono"])
        .build();
    filter.set_primary_icon_name(Some("edit-find-symbolic"));
    let count = gtk::Label::builder()
        .css_classes(["viti-count"])
        .xalign(1.0)
        .build();
    let filter_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    filter_row.set_margin_start(12);
    filter_row.set_margin_end(12);
    filter_row.set_margin_top(8);
    filter_row.append(&gtk::Label::new(Some("Filter")));
    filter_row.append(&filter);
    filter_row.append(&count);

    let update_view = crate::ui::json_view(
        &initial.unwrap_or_else(|| "{\n  $set: {\n    \n  }\n}".into()),
        true,
    );
    let update_scroller = gtk::ScrolledWindow::builder()
        .child(&update_view)
        .vexpand(true)
        .css_classes(["card"])
        .build();
    let update_label = gtk::Label::builder()
        .label("Update  ( { $set: … } / { $unset: … } / … or a pipeline [ { $set: … } ] )")
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption"])
        .build();
    let preview_btn = gtk::Button::builder()
        .label("Preview")
        .tooltip_text("Ctrl+Enter")
        .build();
    let upsert = gtk::CheckButton::with_label("Upsert (insert one document if nothing matches)");
    let left_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    left_head.append(&update_label);
    update_label.set_hexpand(true);
    left_head.append(&upsert);
    left_head.append(&preview_btn);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    left.append(&left_head);
    left.append(&update_scroller);

    let before = crate::ui::json_view("", false);
    let after = crate::ui::json_view("", false);
    let pane = gtk::Paned::new(gtk::Orientation::Horizontal);
    pane.set_start_child(Some(
        &gtk::ScrolledWindow::builder()
            .child(&before)
            .css_classes(["card"])
            .build(),
    ));
    pane.set_end_child(Some(
        &gtk::ScrolledWindow::builder()
            .child(&after)
            .css_classes(["card"])
            .build(),
    ));
    pane.set_position(((app.window.width() - 160) / 2).max(300));
    pane.set_vexpand(true);
    let preview_label = gtk::Label::builder()
        .label("Preview  (before → after)")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let prev = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["flat"])
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["flat"])
        .build();
    let which = gtk::Label::builder().css_classes(["viti-count"]).build();
    let right_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    right_head.append(&preview_label);
    right_head.append(&prev);
    right_head.append(&which);
    right_head.append(&next);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    right.append(&right_head);
    right.append(&pane);

    // Update editor above, before/after preview below: both get the full width.
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.set_start_child(Some(&left));
    split.set_end_child(Some(&right));
    split.set_shrink_start_child(false);
    split.set_shrink_end_child(false);
    split.set_position(190);
    split.set_vexpand(true);
    split.set_margin_start(12);
    split.set_margin_end(12);
    split.set_margin_top(8);

    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(8)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.append(&filter_row);
    body.append(&split);
    body.append(&status);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    struct State {
        pairs: Vec<(Document, Document)>,
        shown: usize,
        matched: Option<u64>,
        generation: u64,
    }
    let state = Rc::new(RefCell::new(State {
        pairs: Vec::new(),
        shown: 0,
        matched: None,
        generation: 0,
    }));
    let busy = Rc::new(Cell::new(false));

    let show_pair = {
        let state = state.clone();
        let (before, after, which) = (before.clone(), after.clone(), which.clone());
        Rc::new(move || {
            let s = state.borrow();
            match s.pairs.get(s.shown) {
                Some((b, a)) => {
                    before.buffer().set_text(&ejson::pretty(b, Mode::Relaxed));
                    after.buffer().set_text(&ejson::pretty(a, Mode::Relaxed));
                    which.set_text(&format!("{} / {}", s.shown + 1, s.pairs.len()));
                }
                None => {
                    before.buffer().set_text("");
                    after.buffer().set_text("");
                    which.set_text("");
                }
            }
        })
    };
    {
        let state = state.clone();
        let show = show_pair.clone();
        prev.connect_clicked(move |_| {
            let mut s = state.borrow_mut();
            if s.shown > 0 {
                s.shown -= 1;
            }
            drop(s);
            show();
        });
    }
    {
        let state = state.clone();
        let show = show_pair.clone();
        next.connect_clicked(move |_| {
            let mut s = state.borrow_mut();
            if s.shown + 1 < s.pairs.len() {
                s.shown += 1;
            }
            drop(s);
            show();
        });
    }

    // Count + preview: run on open and on Preview.
    let refresh: Rc<dyn Fn()> = {
        let app = app.clone();
        let docs = docs.clone();
        let state = state.clone();
        let busy = busy.clone();
        let (filter, update_view, count, status, run) = (
            filter.clone(),
            update_view.clone(),
            count.clone(),
            status.clone(),
            run.clone(),
        );
        let show = show_pair.clone();
        let ns = ns.clone();
        Rc::new(move || {
            let (f, collation) = match filter_of(&filter.text(), &docs) {
                Ok(x) => x,
                Err(e) => {
                    status.set_text(&e);
                    return;
                }
            };
            let update = match UpdateSpec::parse(&crate::ui::buffer_text(&update_view.buffer())) {
                Ok(u) => u,
                Err(e) => {
                    status.set_text(&format!("update: {e}"));
                    run.set_sensitive(false);
                    return;
                }
            };
            let Some(conn) = app.conn(docs.conn) else {
                status.set_text("Not connected");
                return;
            };
            let client = conn.client.clone();
            let ctx = OpCtx::new(app.max_time_ms());
            let generation = {
                let mut s = state.borrow_mut();
                s.generation += 1;
                s.generation
            };
            busy.set(true);
            status.set_text("Counting and previewing…");
            let (state, show, count, status, run, ns, busy) = (
                state.clone(),
                show.clone(),
                count.clone(),
                status.clone(),
                run.clone(),
                ns.clone(),
                busy.clone(),
            );
            glib::spawn_future_local(async move {
                let (f2, ns2, ctx2, client2, col2) = (
                    f.clone(),
                    ns.clone(),
                    ctx.clone(),
                    client.clone(),
                    collation.clone(),
                );
                let r = crate::rt::io(async move {
                    let (n, preview) = tokio::join!(
                        ops::count_filter(&client2, &ns2, f2.clone(), col2.as_ref(), &ctx2),
                        ops::preview_update(&client2, &ns2, f2, &update, PREVIEW_DOCS, &ctx2)
                    );
                    anyhow::Ok((n, preview))
                })
                .await;
                if state.borrow().generation != generation {
                    return;
                }
                busy.set(false);
                let Ok((n, preview)) = r else { return };
                match n {
                    Ok(n) => {
                        state.borrow_mut().matched = Some(n);
                        count.set_text(&format!("matches {}", plural(n, "document")));
                        run.set_label(&format!("Update {}", plural(n, "document")));
                        run.set_sensitive(n > 0 || true);
                    }
                    Err(e) => {
                        state.borrow_mut().matched = None;
                        count.set_text("count failed");
                        status.set_text(&format!("{e:#}"));
                        run.set_sensitive(true);
                    }
                }
                match preview {
                    Ok((kind, pairs)) => {
                        let mut s = state.borrow_mut();
                        s.shown = 0;
                        s.pairs = pairs;
                        let n = s.pairs.len();
                        drop(s);
                        show();
                        status.set_text(&match kind {
                            _ if n == 0 => "No document matches the filter.".to_string(),
                            PreviewKind::Transaction => format!(
                                "Exact preview of {n}: applied by the server in a transaction that was rolled back."
                            ),
                            PreviewKind::Aggregation => format!(
                                "Exact preview of {n}: the pipeline was run as an aggregation over the sample."
                            ),
                            PreviewKind::Approximate => format!(
                                "Approximate preview of {n}: emulated client-side (this server has no transactions)."
                            ),
                        });
                    }
                    Err(e) => {
                        state.borrow_mut().pairs.clear();
                        show();
                        status.set_text(&format!("{e:#}"));
                    }
                }
            });
        })
    };
    {
        let refresh = refresh.clone();
        preview_btn.connect_clicked(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        filter.connect_activate(move |_| refresh());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let app = app.clone();
        let docs = docs.clone();
        let dialog = dialog.clone();
        let state = state.clone();
        let (filter, update_view, upsert, status) = (
            filter.clone(),
            update_view.clone(),
            upsert.clone(),
            status.clone(),
        );
        let ns = ns.clone();
        run.connect_clicked(move |_| {
            let (f, collation) = match filter_of(&filter.text(), &docs) {
                Ok(x) => x,
                Err(e) => {
                    status.set_text(&e);
                    return;
                }
            };
            let update = match UpdateSpec::parse(&crate::ui::buffer_text(&update_view.buffer())) {
                Ok(u) => u,
                Err(e) => {
                    status.set_text(&format!("update: {e}"));
                    return;
                }
            };
            if app.write_guard().is_err() {
                return;
            }
            let matched = state.borrow().matched;
            let heading = match matched {
                Some(n) => format!("Update {}?", plural(n, "document")),
                None => "Update all matching documents?".to_string(),
            };
            let body = format!(
                "In {ns}, matching {}.{}",
                if f.is_empty() {
                    "every document".to_string()
                } else {
                    ejson::compact(&f, Mode::Relaxed)
                },
                if f.is_empty() {
                    " The filter is empty: this touches the whole collection."
                } else {
                    ""
                }
            );
            let app = app.clone();
            let docs = docs.clone();
            let dialog = dialog.clone();
            let ns = ns.clone();
            let do_upsert = upsert.is_active();
            crate::ui::confirm(
                &app.window.clone(),
                &heading,
                &body,
                "Update",
                true,
                move || {
                    let Some(conn) = app.conn(docs.conn) else {
                        return;
                    };
                    let client = conn.client.clone();
                    let ctx = OpCtx::new(app.max_time_ms());
                    let (f, update, collation, ns2) =
                        (f.clone(), update.clone(), collation.clone(), ns.clone());
                    let app = app.clone();
                    let docs = docs.clone();
                    let dialog = dialog.clone();
                    dialog.close();
                    app.banner
                        .set_title(&format!("Updating documents in {ns2}…"));
                    app.banner.set_revealed(true);
                    glib::spawn_future_local(async move {
                        let ns3 = ns2.clone();
                        let r = crate::rt::io(async move {
                            ops::update_many(
                                &client,
                                &ns3,
                                f,
                                &update,
                                collation.as_ref(),
                                do_upsert,
                                &ctx,
                            )
                            .await
                        })
                        .await;
                        app.banner.set_revealed(false);
                        match r {
                            Ok(o) => {
                                let msg = format!(
                                    "Matched {}, modified {}{}",
                                    crate::ui::thousands(o.matched),
                                    crate::ui::thousands(o.modified),
                                    if o.upserted { ", upserted 1" } else { "" }
                                );
                                app.toast(&msg);
                                app.notify_if_unfocused(
                                    "bulk",
                                    &format!("Bulk update on {ns2}"),
                                    &msg,
                                );
                                docs.reload_keep_cursor();
                            }
                            Err(e) => {
                                app.toast_error(&format!("bulk update on {ns2}"), &e);
                                app.notify_if_unfocused(
                                    "bulk",
                                    "Bulk update failed",
                                    &format!("{ns2}: {e:#}"),
                                );
                            }
                        }
                    });
                },
            );
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        let refresh = refresh.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if key == gtk::gdk::Key::Escape {
                dialog.close();
                return glib::Propagation::Stop;
            }
            if ctrl && (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter) {
                refresh();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
    update_view.grab_focus();
    refresh();
}

/// `Ctrl+Shift+d`: delete every document matching the filter.
pub fn delete_dialog(app: &Rc<App>, docs: &Rc<DocumentsPane>) {
    if app.write_guard().is_err() {
        return;
    }
    let ns = docs.ns.clone();
    let dialog = adw::Dialog::builder()
        .title(format!("Delete documents from {ns}"))
        .content_width(680)
        .content_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::builder()
        .label("Delete")
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&run);
    toolbar.add_top_bar(&header);

    let filter = gtk::Entry::builder()
        .text(docs.current_query().filter)
        .placeholder_text("{ }  — every document")
        .hexpand(true)
        .css_classes(["viti-mono"])
        .build();
    filter.set_primary_icon_name(Some("edit-find-symbolic"));
    let count = gtk::Label::builder()
        .css_classes(["viti-count"])
        .xalign(1.0)
        .build();
    let filter_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    filter_row.set_margin_start(12);
    filter_row.set_margin_end(12);
    filter_row.set_margin_top(8);
    filter_row.append(&gtk::Label::new(Some("Filter")));
    filter_row.append(&filter);
    filter_row.append(&count);

    let sample_label = gtk::Label::builder()
        .label("Some of the documents that would be deleted")
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .margin_start(12)
        .build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(8)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&filter_row);
    body.append(&sample_label);
    body.append(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    );
    body.append(&status);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let matched = Rc::new(Cell::new(None::<u64>));
    let generation = Rc::new(Cell::new(0u64));
    let refresh: Rc<dyn Fn()> = {
        let app = app.clone();
        let docs = docs.clone();
        let (filter, count, status, run, list) = (
            filter.clone(),
            count.clone(),
            status.clone(),
            run.clone(),
            list.clone(),
        );
        let matched = matched.clone();
        let generation = generation.clone();
        let ns = ns.clone();
        Rc::new(move || {
            let (f, collation) = match filter_of(&filter.text(), &docs) {
                Ok(x) => x,
                Err(e) => {
                    status.set_text(&e);
                    return;
                }
            };
            let Some(conn) = app.conn(docs.conn) else {
                status.set_text("Not connected");
                return;
            };
            let client = conn.client.clone();
            let ctx = OpCtx::new(app.max_time_ms());
            let generation_now = generation.get() + 1;
            generation.set(generation_now);
            status.set_text("Counting…");
            let (count, status, run, list, matched, generation, ns) = (
                count.clone(),
                status.clone(),
                run.clone(),
                list.clone(),
                matched.clone(),
                generation.clone(),
                ns.clone(),
            );
            glib::spawn_future_local(async move {
                let (f2, ns2, ctx2, client2) = (f.clone(), ns.clone(), ctx.clone(), client.clone());
                let r = crate::rt::io(async move {
                    let (n, sample) = tokio::join!(
                        ops::count_filter(&client2, &ns2, f2.clone(), collation.as_ref(), &ctx2),
                        ops::sample(&client2, &ns2, f2, PREVIEW_DOCS, &ctx2)
                    );
                    anyhow::Ok((n, sample))
                })
                .await;
                if generation.get() != generation_now {
                    return;
                }
                let Ok((n, sample)) = r else { return };
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                match n {
                    Ok(n) => {
                        matched.set(Some(n));
                        count.set_text(&format!("matches {}", plural(n, "document")));
                        run.set_label(&format!("Delete {}", plural(n, "document")));
                        run.set_sensitive(n > 0);
                        status.set_text(if n == 0 {
                            "Nothing matches."
                        } else {
                            "This cannot be undone."
                        });
                    }
                    Err(e) => {
                        matched.set(None);
                        count.set_text("count failed");
                        run.set_sensitive(true);
                        status.set_text(&format!("{e:#}"));
                    }
                }
                if let Ok(sample) = sample {
                    for d in sample {
                        let id = d.get("_id").map(ejson::id_display).unwrap_or_default();
                        let row = adw::ActionRow::builder()
                            .title(glib::markup_escape_text(&id))
                            .subtitle(glib::markup_escape_text(&ejson::summary(
                                &bson::Bson::Document(d.clone()),
                                140,
                            )))
                            .build();
                        row.add_css_class("viti-mono");
                        list.append(&row);
                    }
                }
            });
        })
    };
    {
        let refresh = refresh.clone();
        filter.connect_activate(move |_| refresh());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let app = app.clone();
        let docs = docs.clone();
        let dialog = dialog.clone();
        let (filter, status) = (filter.clone(), status.clone());
        let matched = matched.clone();
        let ns = ns.clone();
        run.connect_clicked(move |_| {
            let (f, collation) = match filter_of(&filter.text(), &docs) {
                Ok(x) => x,
                Err(e) => {
                    status.set_text(&e);
                    return;
                }
            };
            if app.write_guard().is_err() {
                return;
            }
            let go = {
                let app = app.clone();
                let docs = docs.clone();
                let dialog = dialog.clone();
                let ns = ns.clone();
                let f = f.clone();
                move || {
                    let Some(conn) = app.conn(docs.conn) else { return };
                    let client = conn.client.clone();
                    let ctx = OpCtx::new(app.max_time_ms());
                    let (f, collation, ns2) = (f.clone(), collation.clone(), ns.clone());
                    let app = app.clone();
                    let docs = docs.clone();
                    dialog.close();
                    app.banner.set_title(&format!("Deleting documents from {ns2}…"));
                    app.banner.set_revealed(true);
                    glib::spawn_future_local(async move {
                        let ns3 = ns2.clone();
                        let r = crate::rt::io(async move {
                            ops::delete_many(&client, &ns3, f, collation.as_ref(), &ctx).await
                        })
                        .await;
                        app.banner.set_revealed(false);
                        match r {
                            Ok(n) => {
                                let msg = format!("Deleted {}", plural(n, "document"));
                                app.toast(&msg);
                                app.notify_if_unfocused("bulk", &format!("Bulk delete on {ns2}"), &msg);
                                docs.page.set(0);
                                docs.load();
                            }
                            Err(e) => {
                                app.toast_error(&format!("bulk delete on {ns2}"), &e);
                                app.notify_if_unfocused(
                                    "bulk",
                                    "Bulk delete failed",
                                    &format!("{ns2}: {e:#}"),
                                );
                            }
                        }
                    });
                }
            };
            let n = matched.get();
            if f.is_empty() {
                // Emptying a collection gets the typed confirmation a drop does.
                crate::ui::confirm_typed(
                    &app.window.clone(),
                    "Delete every document?",
                    &format!(
                        "The filter is empty: all {} in {ns} will be deleted. Type the collection name to confirm.",
                        n.map(|n| plural(n, "document")).unwrap_or_else(|| "documents".into())
                    ),
                    &ns.coll,
                    "Delete all",
                    go,
                );
            } else {
                crate::ui::confirm(
                    &app.window.clone(),
                    &match n {
                        Some(n) => format!("Delete {}?", plural(n, "document")),
                        None => "Delete all matching documents?".into(),
                    },
                    &format!("From {ns}, matching {}. This cannot be undone.", ejson::compact(&f, Mode::Relaxed)),
                    "Delete",
                    true,
                    go,
                );
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                dialog.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
    refresh();
}
