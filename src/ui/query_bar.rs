//! The query bar above the documents: the filter entry, an options revealer with
//! project/sort/collation/hint/skip/limit/maxTimeMS, the Find/Stop button and the
//! history popover. It knows nothing about the server; `on_run` gets a `Query`.
//!
//! `row` is the pane's single toolbar line: the documents pane prepends its view
//! switcher and appends its pager and menu, so there is only ever one row.
use crate::config::{Query, SavedQuery};
use adw::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

#[allow(clippy::type_complexity)]
pub struct QueryBar {
    pub root: gtk::Box,
    pub row: gtk::Box,
    pub filter: gtk::Entry,
    pub options: gtk::Revealer,
    pub project: gtk::Entry,
    pub sort: gtk::Entry,
    pub collation: gtk::Entry,
    pub hint: gtk::Entry,
    pub skip: gtk::SpinButton,
    pub limit: gtk::SpinButton,
    pub max_time: gtk::SpinButton,
    pub run: gtk::Button,
    /// Replaces `run` while a query is in flight.
    pub stop: gtk::Button,
    spinner: gtk::Spinner,
    pub history_btn: gtk::MenuButton,
    /// "Generate with AI": the pane wires it, the bar only shows it.
    pub ai_btn: gtk::Button,
    history_popover: gtk::Popover,
    history_list: gtk::ListBox,
    pub error: gtk::Label,
    on_run: RefCell<Option<Rc<dyn Fn(Query)>>>,
    on_history_pick: RefCell<Option<Rc<dyn Fn(SavedQuery)>>>,
    /// (id) -> toggle favourite / delete; the pane refreshes the popover.
    on_history_star: RefCell<Option<Rc<dyn Fn(uuid::Uuid)>>>,
    on_history_delete: RefCell<Option<Rc<dyn Fn(uuid::Uuid)>>>,
}

fn mono_entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .css_classes(["viti-mono"])
        .build()
}

impl QueryBar {
    pub fn new() -> Rc<Self> {
        let filter = mono_entry("{ field: 'value' }   — Enter to run, Ctrl+Y for history");
        filter.set_primary_icon_name(Some("edit-find-symbolic"));
        // Secondary icon = Reset; only shown once the query differs from the default.
        filter.set_secondary_icon_activatable(true);
        filter.set_secondary_icon_tooltip_text(Some("Reset query"));
        let run = gtk::Button::builder()
            .label("Find")
            .focus_on_click(false)
            .css_classes(["suggested-action"])
            .build();
        let stop = gtk::Button::builder()
            .label("Stop")
            .focus_on_click(false)
            .tooltip_text("Cancel (Esc)")
            .css_classes(["destructive-action"])
            .visible(false)
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        let options_btn = gtk::ToggleButton::builder()
            .icon_name("pan-down-symbolic")
            .focus_on_click(false)
            .tooltip_text("Query options (Alt+O)")
            .css_classes(["flat"])
            .build();
        let history_popover = gtk::Popover::new();
        let history_list = gtk::ListBox::new();
        history_list.add_css_class("boxed-list");
        history_list.set_selection_mode(gtk::SelectionMode::None);
        let history_scroller = gtk::ScrolledWindow::builder()
            .child(&history_list)
            .propagate_natural_height(true)
            .max_content_height(400)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        // A hard floor: `min-content-width` alone leaves the popover one glyph
        // wide when the list is empty.
        history_scroller.set_size_request(420, -1);
        history_popover.set_child(Some(&history_scroller));
        let history_btn = gtk::MenuButton::builder()
            .icon_name("document-open-recent-symbolic")
            .tooltip_text("History & favourites (Ctrl+Y)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .popover(&history_popover)
            .build();

        // One line: [ filter ] [history] [options] [spinner] [Find/Stop], with the
        // pane's own controls added around it.
        let ai_btn = gtk::Button::builder()
            .label("AI")
            .tooltip_text("Generate a query from a description (Ctrl+I)")
            .focus_on_click(false)
            .css_classes(["flat"])
            .build();
        let query_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        query_group.add_css_class("linked");
        query_group.append(&filter);
        query_group.append(&history_btn);
        query_group.append(&ai_btn);
        query_group.append(&options_btn);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_margin_start(8);
        row.set_margin_end(8);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.append(&query_group);
        row.append(&spinner);
        row.append(&run);
        row.append(&stop);

        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(6)
            .build();
        grid.set_margin_start(8);
        grid.set_margin_end(8);
        grid.set_margin_top(6);
        let project = mono_entry("{ field: 1 }");
        let sort = mono_entry("{ field: -1 }");
        let collation = mono_entry("{ locale: 'en' }");
        let hint = mono_entry("index name or { field: 1 }");
        let skip = gtk::SpinButton::with_range(0.0, 1e12, 1.0);
        let limit = gtk::SpinButton::with_range(0.0, 1e12, 1.0);
        let max_time = gtk::SpinButton::with_range(0.0, 1e9, 1000.0);
        let lbl = |t: &str| {
            gtk::Label::builder()
                .label(t)
                .xalign(1.0)
                .css_classes(["dim-label"])
                .build()
        };
        grid.attach(&lbl("Project"), 0, 0, 1, 1);
        grid.attach(&project, 1, 0, 1, 1);
        grid.attach(&lbl("Sort"), 2, 0, 1, 1);
        grid.attach(&sort, 3, 0, 1, 1);
        grid.attach(&lbl("Collation"), 0, 1, 1, 1);
        grid.attach(&collation, 1, 1, 1, 1);
        grid.attach(&lbl("Hint"), 2, 1, 1, 1);
        grid.attach(&hint, 3, 1, 1, 1);
        let nums = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        nums.append(&lbl("Skip"));
        nums.append(&skip);
        nums.append(&lbl("Limit"));
        nums.append(&limit);
        nums.append(&lbl("Max time (ms)"));
        nums.append(&max_time);
        nums.append(
            &gtk::Label::builder()
                .label("0 = default")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        grid.attach(&nums, 0, 2, 4, 1);
        let options = gtk::Revealer::builder()
            .child(&grid)
            .reveal_child(false)
            .build();
        options_btn
            .bind_property("active", &options, "reveal-child")
            .bidirectional()
            .sync_create()
            .build();

        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error", "caption"])
            .margin_start(12)
            .margin_end(12)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&row);
        root.append(&options);
        root.append(&error);

        let bar = Rc::new(Self {
            root,
            row,
            filter,
            options,
            project,
            sort,
            collation,
            hint,
            skip,
            limit,
            max_time,
            run,
            stop,
            spinner,
            history_btn,
            ai_btn,
            history_popover,
            history_list,
            error,
            on_run: RefCell::new(None),
            on_history_pick: RefCell::new(None),
            on_history_star: RefCell::new(None),
            on_history_delete: RefCell::new(None),
        });
        for entry in [
            &bar.filter,
            &bar.project,
            &bar.sort,
            &bar.collation,
            &bar.hint,
        ] {
            let b = bar.clone();
            entry.connect_activate(move |_| b.submit());
            let b = bar.clone();
            entry.connect_changed(move |_| b.sync_reset());
        }
        for spin in [&bar.skip, &bar.limit, &bar.max_time] {
            let b = bar.clone();
            spin.connect_activate(move |_| b.submit());
            let b = bar.clone();
            spin.connect_value_changed(move |_| b.sync_reset());
        }
        {
            let b = bar.clone();
            bar.run.connect_clicked(move |_| b.submit());
        }
        {
            let b = bar.clone();
            bar.filter.connect_icon_release(move |_, pos| {
                if pos == gtk::EntryIconPosition::Secondary {
                    b.reset();
                }
            });
        }
        options_btn.connect_toggled(|b| {
            b.set_icon_name(if b.is_active() {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            });
        });
        bar
    }

    /// Clear the filter and every option, then re-run.
    pub fn reset(&self) {
        self.set_query(&Query::default());
        self.submit();
    }

    /// Show the reset icon only when there is something to reset.
    fn sync_reset(&self) {
        let icon = (!self.query().is_default()).then_some("edit-clear-symbolic");
        self.filter.set_secondary_icon_name(icon);
    }

    /// Find becomes Stop while a query is in flight.
    pub fn set_busy(&self, busy: bool) {
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
        self.run.set_visible(!busy);
        self.stop.set_visible(busy);
    }

    pub fn set_on_run(&self, f: impl Fn(Query) + 'static) {
        *self.on_run.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_on_history_pick(&self, f: impl Fn(SavedQuery) + 'static) {
        *self.on_history_pick.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_on_history_star(&self, f: impl Fn(uuid::Uuid) + 'static) {
        *self.on_history_star.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_on_history_delete(&self, f: impl Fn(uuid::Uuid) + 'static) {
        *self.on_history_delete.borrow_mut() = Some(Rc::new(f));
    }

    /// Called every time the popover is about to open, however it was opened:
    /// the pane answers with `show_history`.
    pub fn set_on_history_open(&self, f: impl Fn() + 'static) {
        self.history_btn.set_create_popup_func(move |_| f());
    }

    pub fn open_history(&self) {
        self.history_btn.popup();
    }

    pub fn submit(&self) {
        let cb = self.on_run.borrow().clone();
        if let Some(cb) = cb {
            cb(self.query());
        }
    }

    pub fn query(&self) -> Query {
        let mt = self.max_time.value() as u64;
        Query {
            filter: self.filter.text().to_string(),
            project: self.project.text().to_string(),
            sort: self.sort.text().to_string(),
            collation: self.collation.text().to_string(),
            skip: self.skip.value() as u64,
            limit: self.limit.value() as u64,
            max_time_ms: (mt > 0).then_some(mt),
            hint: self.hint.text().to_string(),
        }
    }

    pub fn set_query(&self, q: &Query) {
        self.filter.set_text(&q.filter);
        self.project.set_text(&q.project);
        self.sort.set_text(&q.sort);
        self.collation.set_text(&q.collation);
        self.hint.set_text(&q.hint);
        self.skip.set_value(q.skip as f64);
        self.limit.set_value(q.limit as f64);
        self.max_time.set_value(q.max_time_ms.unwrap_or(0) as f64);
        let has_options = !q.project.is_empty()
            || !q.sort.is_empty()
            || !q.collation.is_empty()
            || !q.hint.is_empty()
            || q.skip > 0
            || q.limit > 0
            || q.max_time_ms.is_some();
        if has_options {
            self.options.set_reveal_child(true);
        }
    }

    pub fn set_error(&self, msg: Option<&str>) {
        match msg {
            Some(m) => {
                self.error.set_text(m);
                self.error.set_visible(true);
            }
            None => self.error.set_visible(false),
        }
    }

    pub fn toggle_options(&self) {
        let show = !self.options.reveals_child();
        self.options.set_reveal_child(show);
        if show {
            self.project.grab_focus();
        }
    }

    pub fn focus_filter(&self) {
        self.filter.grab_focus();
        self.filter.select_region(0, -1);
    }

    pub fn focus_sort(&self) {
        self.options.set_reveal_child(true);
        self.sort.grab_focus();
    }

    /// Fill the popover: favourites first, then history, newest first.
    pub fn show_history(&self, entries: Vec<SavedQuery>) {
        while let Some(child) = self.history_list.first_child() {
            self.history_list.remove(&child);
        }
        if entries.is_empty() {
            let row = adw::ActionRow::builder().title("No queries yet").build();
            row.add_css_class("dim-label");
            self.history_list.append(&row);
        }
        let mut sorted = entries;
        sorted.sort_by_key(|q| (!q.favourite, std::cmp::Reverse(q.last_run)));
        for q in sorted {
            let title = q.name.clone().unwrap_or_else(|| q.query.summary());
            let row = adw::ActionRow::builder()
                .title(gtk::glib::markup_escape_text(&title))
                .subtitle(
                    q.last_run
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                )
                .activatable(true)
                .build();
            row.add_css_class("viti-mono");
            let star = gtk::Button::builder()
                .icon_name(if q.favourite {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if q.favourite {
                    "Remove from favourites"
                } else {
                    "Save as favourite"
                })
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Forget")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&star);
            row.add_suffix(&del);
            {
                let f = self.on_history_star.borrow().clone();
                let id = q.id;
                let popover = self.history_popover.clone();
                star.connect_clicked(move |_| {
                    popover.popdown();
                    if let Some(f) = &f {
                        f(id);
                    }
                });
            }
            {
                let f = self.on_history_delete.borrow().clone();
                let id = q.id;
                let popover = self.history_popover.clone();
                del.connect_clicked(move |_| {
                    popover.popdown();
                    if let Some(f) = &f {
                        f(id);
                    }
                });
            }
            let pick = self.on_history_pick.borrow().clone();
            let popover = self.history_popover.clone();
            row.connect_activated(move |_| {
                popover.popdown();
                if let Some(f) = &pick {
                    f(q.clone());
                }
            });
            self.history_list.append(&row);
        }
    }

    pub fn clear_focused(&self) {
        for e in [
            &self.filter,
            &self.project,
            &self.sort,
            &self.collation,
            &self.hint,
        ] {
            if e.has_focus() {
                e.set_text("");
            }
        }
    }
}
