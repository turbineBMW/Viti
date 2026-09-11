//! My Queries: every favourite (and, on request, the whole history) across
//! namespaces, plus saved pipelines, with run / rename / copy / delete.
//! `Ctrl+Shift+Y`, `:queries`.
use crate::app::App;
use crate::config::{SavedPipeline, SavedQuery};
use crate::mongo::ops::Namespace;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::rc::Rc;

pub fn show(app: &Rc<App>) {
    let dialog = adw::Dialog::builder()
        .title("My Queries")
        .content_width(720)
        .content_height(640)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let history_toggle = gtk::ToggleButton::builder()
        .icon_name("document-open-recent-symbolic")
        .tooltip_text("Include history, not only favourites")
        .build();
    header.pack_end(&history_toggle);
    toolbar.add_top_bar(&header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search names, filters, namespaces")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .build();
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list-separate");
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);
    let empty = adw::StatusPage::builder()
        .icon_name("starred-symbolic")
        .title("No saved queries")
        .description("Ctrl+S in the query bar saves a favourite; Ctrl+S on the Aggregations page saves a pipeline.")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build(),
        Some("list"),
    );
    stack.add_named(&empty, Some("empty"));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.append(&search);
    body.append(&stack);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let rebuild: Rc<dyn Fn()> = {
        let app = app.clone();
        let dialog = dialog.clone();
        let (list, stack, history_toggle) = (list.clone(), stack.clone(), history_toggle.clone());
        // The rebuild closure hands itself to the row buttons through this cell.
        type Rebuild = Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>>;
        let me: Rebuild = Rc::new(std::cell::RefCell::new(None));
        let me2 = me.clone();
        let f: Rc<dyn Fn()> = Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let all = history_toggle.is_active();
            let mut queries: Vec<SavedQuery> = app
                .config
                .borrow()
                .queries
                .iter()
                .filter(|q| all || q.favourite)
                .cloned()
                .collect();
            queries.sort_by(|a, b| {
                a.ns.cmp(&b.ns)
                    .then(b.favourite.cmp(&a.favourite))
                    .then(b.last_run.cmp(&a.last_run))
            });
            let pipelines: Vec<SavedPipeline> = app.config.borrow().pipelines.clone();
            stack.set_visible_child_name(if queries.is_empty() && pipelines.is_empty() {
                "empty"
            } else {
                "list"
            });
            let mut last_ns = String::new();
            for q in queries {
                if q.ns != last_ns {
                    last_ns = q.ns.clone();
                    let title = gtk::Label::builder()
                        .label(&q.ns)
                        .xalign(0.0)
                        .css_classes(["heading", "viti-mono"])
                        .margin_top(12)
                        .margin_bottom(4)
                        .build();
                    let row = gtk::ListBoxRow::builder()
                        .child(&title)
                        .activatable(false)
                        .selectable(false)
                        .build();
                    row.set_widget_name(&q.ns.to_lowercase());
                    list.append(&row);
                }
                let title = q.name.clone().unwrap_or_else(|| q.query.summary());
                let when = q
                    .last_run
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                let subtitle = if q.name.is_some() {
                    format!("{}  ·  {when}", q.query.summary())
                } else {
                    when
                };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&title))
                    .subtitle(glib::markup_escape_text(&subtitle))
                    .activatable(true)
                    .build();
                row.add_css_class("viti-mono");
                row.set_widget_name(
                    &format!("{} {} {}", q.ns, title, q.query.summary()).to_lowercase(),
                );
                if q.favourite {
                    row.add_prefix(&gtk::Image::from_icon_name("starred-symbolic"));
                }
                let mk = |icon: &str, tip: &str| {
                    gtk::Button::builder()
                        .icon_name(icon)
                        .tooltip_text(tip)
                        .valign(gtk::Align::Center)
                        .css_classes(["flat"])
                        .build()
                };
                let run = mk("media-playback-start-symbolic", "Run");
                let star = mk(
                    if q.favourite {
                        "starred-symbolic"
                    } else {
                        "non-starred-symbolic"
                    },
                    if q.favourite {
                        "Remove from favourites"
                    } else {
                        "Add to favourites"
                    },
                );
                let rename = mk("document-edit-symbolic", "Rename");
                let copy = mk("edit-copy-symbolic", "Copy filter");
                let del = mk("user-trash-symbolic", "Delete");
                for b in [&run, &star, &rename, &copy, &del] {
                    row.add_suffix(b);
                }
                {
                    let app = app.clone();
                    let dialog = dialog.clone();
                    let q = q.clone();
                    let go = move || {
                        dialog.close();
                        app.run_saved_query(&q);
                    };
                    let go2 = go.clone();
                    run.connect_clicked(move |_| go());
                    row.connect_activated(move |_| go2());
                }
                {
                    let app = app.clone();
                    let id = q.id;
                    let me = me2.clone();
                    star.connect_clicked(move |_| {
                        app.toggle_favourite(id);
                        if let Some(f) = me.borrow().clone() {
                            f();
                        }
                    });
                }
                {
                    let app = app.clone();
                    let id = q.id;
                    let me = me2.clone();
                    let current = q.name.clone().unwrap_or_default();
                    rename.connect_clicked(move |_| {
                        let d = adw::AlertDialog::new(Some("Rename query"), None);
                        let entry = gtk::Entry::builder()
                            .text(&current)
                            .activates_default(true)
                            .build();
                        d.set_extra_child(Some(&entry));
                        d.add_responses(&[("cancel", "Cancel"), ("ok", "Rename")]);
                        d.set_default_response(Some("ok"));
                        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
                        let window = app.window.clone();
                        let app = app.clone();
                        let me = me.clone();
                        let entry2 = entry.clone();
                        d.connect_response(None, move |_, r| {
                            if r != "ok" {
                                return;
                            }
                            let name = entry2.text().trim().to_string();
                            if let Some(q) = app
                                .config
                                .borrow_mut()
                                .queries
                                .iter_mut()
                                .find(|q| q.id == id)
                            {
                                q.name = (!name.is_empty()).then_some(name);
                            }
                            app.schedule_save();
                            if let Some(f) = me.borrow().clone() {
                                f();
                            }
                        });
                        d.present(Some(&window));
                        entry.grab_focus();
                    });
                }
                {
                    let app = app.clone();
                    let filter = q.query.filter.clone();
                    copy.connect_clicked(move |_| {
                        crate::ui::copy_text(&filter);
                        app.toast("Filter copied");
                    });
                }
                {
                    let app = app.clone();
                    let id = q.id;
                    let me = me2.clone();
                    del.connect_clicked(move |_| {
                        app.config.borrow_mut().queries.retain(|q| q.id != id);
                        app.schedule_save();
                        if let Some(f) = me.borrow().clone() {
                            f();
                        }
                    });
                }
                list.append(&row);
            }
            if !pipelines.is_empty() {
                let title = gtk::Label::builder()
                    .label("Pipelines")
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .margin_top(12)
                    .margin_bottom(4)
                    .build();
                let row = gtk::ListBoxRow::builder()
                    .child(&title)
                    .activatable(false)
                    .selectable(false)
                    .build();
                row.set_widget_name("pipelines");
                list.append(&row);
            }
            let mut pipelines = pipelines;
            pipelines.sort_by(|a, b| a.ns.cmp(&b.ns).then(b.saved.cmp(&a.saved)));
            for sp in pipelines {
                let stages: Vec<&str> = sp
                    .pipeline
                    .stages
                    .iter()
                    .map(|s| s.operator.as_str())
                    .collect();
                let subtitle = format!(
                    "{}  ·  {}  ·  {}",
                    sp.ns,
                    stages.join(" → "),
                    sp.saved
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                );
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&sp.name))
                    .subtitle(glib::markup_escape_text(&subtitle))
                    .activatable(true)
                    .build();
                row.add_css_class("viti-mono");
                row.set_widget_name(&format!("{} {} pipeline", sp.ns, sp.name).to_lowercase());
                row.add_prefix(&gtk::Image::from_icon_name("system-run-symbolic"));
                let mk = |icon: &str, tip: &str| {
                    gtk::Button::builder()
                        .icon_name(icon)
                        .tooltip_text(tip)
                        .valign(gtk::Align::Center)
                        .css_classes(["flat"])
                        .build()
                };
                let run = mk("media-playback-start-symbolic", "Open and run");
                let copy = mk("edit-copy-symbolic", "Copy pipeline");
                let del = mk("user-trash-symbolic", "Delete");
                for b in [&run, &copy, &del] {
                    row.add_suffix(b);
                }
                {
                    let app = app.clone();
                    let dialog = dialog.clone();
                    let sp = sp.clone();
                    let go = move || {
                        dialog.close();
                        app.run_saved_pipeline(&sp);
                    };
                    let go2 = go.clone();
                    run.connect_clicked(move |_| go());
                    row.connect_activated(move |_| go2());
                }
                {
                    let app = app.clone();
                    let text = sp.pipeline.to_text();
                    copy.connect_clicked(move |_| {
                        crate::ui::copy_text(&text);
                        app.toast("Pipeline copied");
                    });
                }
                {
                    let app = app.clone();
                    let id = sp.id;
                    let me = me2.clone();
                    del.connect_clicked(move |_| {
                        app.config.borrow_mut().pipelines.retain(|p| p.id != id);
                        app.schedule_save();
                        if let Some(f) = me.borrow().clone() {
                            f();
                        }
                    });
                }
                list.append(&row);
            }
        });
        *me.borrow_mut() = Some(f.clone());
        f
    };
    rebuild();
    {
        let rebuild = rebuild.clone();
        history_toggle.connect_toggled(move |_| rebuild());
    }
    {
        let list = list.clone();
        search.connect_search_changed(move |e| {
            let needle = e.text().to_lowercase();
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let name = row.widget_name().to_string();
                row.set_visible(needle.is_empty() || name.contains(&needle));
                i += 1;
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        let search2 = search.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if search2.has_focus() && !search2.text().is_empty() {
                    search2.set_text("");
                } else {
                    dialog.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
    search.grab_focus();
}

impl App {
    /// Run a saved query: open its collection (on its own connection when
    /// that one is live, else the current one) and submit the query.
    pub fn run_saved_query(self: &Rc<Self>, q: &SavedQuery) {
        let Some((db, coll)) = q.ns.split_once('.') else {
            return;
        };
        let conn = q
            .conn
            .filter(|c| self.conn(*c).is_some())
            .or_else(|| self.current_conn.get())
            .filter(|c| self.conn(*c).is_some());
        let Some(conn) = conn else {
            self.toast("Connect first, then run the query");
            return;
        };
        let ns = Namespace::new(db, coll);
        self.open_collection(conn, ns.clone());
        if let Some(tab) = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.conn == conn && t.ns == ns)
            .cloned()
        {
            tab.docs.query_bar.set_query(&q.query);
            tab.docs.run_query(q.query.clone());
        }
    }

    /// Open a saved pipeline on its collection's Aggregations page and run it.
    pub fn run_saved_pipeline(self: &Rc<Self>, sp: &SavedPipeline) {
        let Some((db, coll)) = sp.ns.split_once('.') else {
            return;
        };
        let conn = sp
            .conn
            .filter(|c| self.conn(*c).is_some())
            .or_else(|| self.current_conn.get())
            .filter(|c| self.conn(*c).is_some());
        let Some(conn) = conn else {
            self.toast("Connect first, then open the pipeline");
            return;
        };
        let ns = Namespace::new(db, coll);
        self.open_collection(conn, ns.clone());
        if let Some(tab) = self
            .tabs
            .borrow()
            .iter()
            .find(|t| t.conn == conn && t.ns == ns)
            .cloned()
        {
            tab.show_page("aggregations");
            self.sync_page_picker();
            tab.agg.load_saved(sp);
            tab.agg.run();
        }
    }

    /// Star or unstar a history entry; starring asks for a name.
    pub fn toggle_favourite(self: &Rc<Self>, id: uuid::Uuid) {
        let now_fav = {
            let mut cfg = self.config.borrow_mut();
            let Some(q) = cfg.queries.iter_mut().find(|q| q.id == id) else {
                return;
            };
            q.favourite = !q.favourite;
            q.favourite
        };
        self.schedule_save();
        if !now_fav {
            self.toast("Removed from favourites");
            return;
        }
        let dialog = adw::AlertDialog::new(Some("Name this favourite"), None);
        let entry = gtk::Entry::builder()
            .placeholder_text("Optional name")
            .activates_default(true)
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("skip", "No name"), ("ok", "Save")]);
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let app = self.clone();
        let entry2 = entry.clone();
        dialog.connect_response(None, move |_, r| {
            if r == "ok" {
                let name = entry2.text().trim().to_string();
                if let Some(q) = app
                    .config
                    .borrow_mut()
                    .queries
                    .iter_mut()
                    .find(|q| q.id == id)
                {
                    q.name = (!name.is_empty()).then_some(name);
                }
                app.schedule_save();
            }
            app.toast("Saved as favourite");
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }
}
