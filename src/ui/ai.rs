//! AI entry points (`Ctrl+I`, `:ai`, the AI buttons): the request dialog,
//! gathering context (a schema sample, existing indexes, the explain output),
//! running the backend as a cancellable long op in the banner, and handing the
//! result back for review — the query bar is filled, the pipeline replaced, the
//! explanation shown, the index suggestions listed. Nothing runs by itself.
use crate::ai::{self, Backend, IndexSuggestion, Request, Response, Task};
use crate::app::{App, LongOp};
use crate::mongo::ops::{self, OpCtx};
use crate::mongo::schema;
use crate::ui::collection::CollectionTab;
use adw::prelude::*;
use anyhow::anyhow;
use gtk4 as gtk;
use gtk4::glib;
use std::rc::Rc;

/// Longest explain output sent along (characters).
const MAX_EXPLAIN_CHARS: usize = 40_000;

/// The task follows the tab's current page unless given. With `text` the
/// request runs straight away (an empty text is fine for the tasks that have a
/// default request); without it a dialog asks.
pub fn ask(app: &Rc<App>, task: Option<Task>, text: Option<String>) {
    let Some(tab) = app.current_tab() else {
        app.toast("Open a collection first");
        return;
    };
    let task = task.unwrap_or_else(|| match tab.stack.visible_child_name().as_deref() {
        Some("aggregations") => Task::Pipeline,
        Some("explain") => Task::ExplainPlan,
        Some("indexes") => Task::IndexSuggest,
        _ => Task::Query,
    });
    match text {
        Some(t) if !t.trim().is_empty() || !task.needs_text() => start(app, &tab, task, t),
        _ => prompt_dialog(app, tab, task),
    }
}

fn prompt_dialog(app: &Rc<App>, tab: Rc<CollectionTab>, task: Task) {
    let settings = app.config.borrow().settings.clone();
    let backend_name = Backend::from_settings(&settings.ai)
        .map(|b| b.name())
        .unwrap_or_else(|_| "AI".into());
    let dialog = adw::Dialog::builder()
        .title(task.title())
        .content_width(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let go = gtk::Button::builder()
        .label("Generate")
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel);
    header.pack_end(&go);
    toolbar.add_top_bar(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(8);
    body.set_margin_bottom(16);
    let (placeholder, hint) = match task {
        Task::Query => (
            "e.g. orders over 100 dollars from last month, newest first",
            "Describe the documents you want. The filter, projection and sort land in the query bar for you to review and run.",
        ),
        Task::Pipeline => (
            "e.g. count orders per customer, top 10 by total",
            "Describe the result you want. The stages replace the current pipeline for you to review and run.",
        ),
        Task::ExplainPlan => (
            "optional — e.g. why is this slow?",
            "The last explain output is sent along; leave the question empty for a general explanation.",
        ),
        Task::IndexSuggest => (
            "optional — describe the workload, e.g. lookups by email and recent orders per user",
            "The field list, existing indexes and the current query are sent along; describe the workload for better suggestions.",
        ),
    };
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .activates_default(true)
        .build();
    let hint = gtk::Label::builder()
        .label(hint)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let footer = gtk::Label::builder()
        .label(format!(
            "Sent to `{backend_name}` with {}'s field names and types{}. Nothing runs until you do.",
            tab.ns,
            if settings.ai.include_sample_values {
                " and a few sample values"
            } else {
                ""
            }
        ))
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    body.append(&entry);
    body.append(&hint);
    body.append(&footer);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&go));
    dialog.set_focus(Some(&entry));

    let submit: Rc<dyn Fn()> = {
        let app = app.clone();
        let dialog = dialog.clone();
        let entry = entry.clone();
        Rc::new(move || {
            let text = entry.text().trim().to_string();
            if text.is_empty() && task.needs_text() {
                entry.add_css_class("error");
                entry.grab_focus();
                return;
            }
            dialog.close();
            start(&app, &tab, task, text);
        })
    };
    {
        let submit = submit.clone();
        go.connect_clicked(move |_| submit());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    entry.connect_changed(|e| e.remove_css_class("error"));
    dialog.present(Some(&app.window));
}

/// Gather context, run the backend and apply the response.
pub fn start(app: &Rc<App>, tab: &Rc<CollectionTab>, task: Task, text: String) {
    let settings = app.config.borrow().settings.clone();
    let backend = match Backend::from_settings(&settings.ai) {
        Ok(b) => b,
        Err(e) => {
            app.toast(&e);
            return;
        }
    };
    let Some(conn) = app.conn(tab.conn) else {
        app.toast("Not connected");
        return;
    };
    let explain_raw = tab.explain.raw_text();
    if task == Task::ExplainPlan && explain_raw.is_empty() {
        app.toast("Run Explain first (R on the Explain page), then ask for an explanation");
        return;
    }
    let existing_schema = tab.schema.schema();
    let query = tab.docs.current_query();
    let client = conn.client.clone();
    let ns = tab.ns.clone();
    let include_values = settings.ai.include_sample_values;
    let sample_n = settings.schema_sample_size.clamp(20, 200) as u64;
    let ctx = OpCtx::new(app.max_time_ms());
    let ctx2 = ctx.clone();
    let backend2 = backend.clone();
    let (tx, rx) = async_channel::bounded(1);
    let handle = crate::rt::spawn(async move {
        let r: anyhow::Result<Response> = async {
            let schema = match existing_schema {
                Some(s) => s,
                None => {
                    let docs =
                        match ops::sample_random(&client, &ns, Default::default(), sample_n, &ctx2)
                            .await
                        {
                            Ok(d) => d,
                            Err(_) => {
                                ops::sample(&client, &ns, Default::default(), sample_n, &ctx2)
                                    .await?
                            }
                        };
                    schema::analyze(&docs)
                }
            };
            let mut req = Request {
                task: Some(task),
                namespace: ns.to_string(),
                schema: ai::schema_lines(&schema, include_values, ai::MAX_SCHEMA_LINES),
                context: Vec::new(),
                request: text,
            };
            let query_text = || {
                let mut lines = Vec::new();
                if !query.filter.trim().is_empty() {
                    lines.push(format!("filter: {}", query.filter.trim()));
                }
                if !query.sort.trim().is_empty() {
                    lines.push(format!("sort: {}", query.sort.trim()));
                }
                if !query.project.trim().is_empty() {
                    lines.push(format!("projection: {}", query.project.trim()));
                }
                lines.join("\n")
            };
            match task {
                Task::IndexSuggest => {
                    let indexes = ops::list_indexes(&client, &ns).await?;
                    let text: Vec<String> = indexes
                        .iter()
                        .map(|i| {
                            format!(
                                "{}: {}",
                                i.name,
                                crate::ui::indexes::keys_text(&i.keys, &i.options)
                            )
                        })
                        .collect();
                    req.context
                        .push(("Existing indexes".into(), text.join("\n")));
                    req.context.push(("Current query".into(), query_text()));
                }
                Task::ExplainPlan => {
                    req.context.push(("Current query".into(), query_text()));
                    let raw = if explain_raw.chars().count() > MAX_EXPLAIN_CHARS {
                        let cut: String = explain_raw.chars().take(MAX_EXPLAIN_CHARS).collect();
                        format!("{cut}\n… (truncated)")
                    } else {
                        explain_raw
                    };
                    req.context.push(("Explain output".into(), raw));
                }
                _ => {}
            }
            let prompt = ai::build_prompt(&req);
            tracing::debug!(
                "ai {task:?} via {}: {} chars of prompt",
                backend2.name(),
                prompt.len()
            );
            let stdout = ai::run(&backend2, prompt, ai::TIMEOUT).await?;
            tracing::debug!("ai reply: {}", crate::mongo::ejson::truncate(&stdout, 500));
            let reply = ai::extract_result(&stdout, backend2.json_result())?;
            ai::parse_response(task, &reply).map_err(|e| anyhow!(e))
        }
        .await;
        let _ = tx.send(r).await;
    });
    let id = uuid::Uuid::new_v4();
    app.add_long_op(
        id,
        LongOp {
            label: format!("{} on {}: asking {}…", task.title(), tab.ns, backend.name()),
            conn: tab.conn,
            ctx: Some(ctx),
            handle: handle.abort_handle(),
            partial_file: None,
        },
    );
    let app = app.clone();
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        // A cancelled task drops the sender: nothing to do, the banner is gone.
        let Ok(r) = rx.recv().await else { return };
        app.finish_long_op(id);
        match r {
            Ok(resp) => apply(&app, &tab, resp),
            Err(e) => app.toast_error(&format!("AI {} on {}", task.title(), tab.ns), &e),
        }
    });
}

fn apply(app: &Rc<App>, tab: &Rc<CollectionTab>, resp: Response) {
    match resp {
        Response::Query(q) => {
            tab.show_page("documents");
            app.sync_page_picker();
            let mut cur = tab.docs.current_query();
            cur.filter = q.filter;
            cur.project = q.project;
            cur.sort = q.sort;
            if q.limit > 0 {
                cur.limit = q.limit;
            }
            if q.skip > 0 {
                cur.skip = q.skip;
            }
            tab.docs.query_bar.set_query(&cur);
            tab.docs.query_bar.set_error(None);
            tab.docs.query_bar.focus_filter();
            app.toast("Query generated — review it, then Enter or Find runs it");
        }
        Response::Pipeline(text) => {
            tab.show_page("aggregations");
            app.sync_page_picker();
            match tab.agg.load_text(&text) {
                Ok(()) => {
                    tab.agg.focus();
                    app.toast("Pipeline generated — review the stages, then R or Run");
                }
                Err(e) => app.toast(&format!("AI pipeline did not load: {e}")),
            }
        }
        Response::Explanation(text) => {
            tab.show_page("explain");
            app.sync_page_picker();
            tab.explain.set_explanation(Some(&text));
        }
        Response::Indexes(list) => {
            tab.show_page("indexes");
            app.sync_page_picker();
            tab.indexes.ensure_loaded();
            if list.is_empty() {
                app.toast("No new indexes suggested");
            } else {
                suggestions_dialog(app, tab, list);
            }
        }
    }
}

fn suggestions_dialog(app: &Rc<App>, tab: &Rc<CollectionTab>, list: Vec<IndexSuggestion>) {
    let dialog = adw::Dialog::builder()
        .title(format!("Suggested indexes for {}", tab.ns))
        .content_width(640)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let group = adw::PreferencesGroup::builder()
        .description(
            "Suggestions only — review each one; Create… opens the index dialog filled in.",
        )
        .build();
    for s in list {
        let row = adw::ActionRow::builder()
            .title(crate::ui::indexes::keys_text(&s.keys, &s.options))
            .subtitle(s.reason.trim())
            .build();
        row.add_css_class("property");
        let create = gtk::Button::builder()
            .label("Create…")
            .valign(gtk::Align::Center)
            .build();
        {
            let dialog = dialog.clone();
            let indexes = tab.indexes.clone();
            create.connect_clicked(move |_| {
                dialog.close();
                indexes.add_prefilled(Some((s.keys.clone(), s.options.clone())));
            });
        }
        row.add_suffix(&create);
        group.add(&row);
    }
    let page = adw::PreferencesPage::new();
    page.add(&group);
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(&page)
            .propagate_natural_height(true)
            .max_content_height(600)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    ));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&app.window));
}
