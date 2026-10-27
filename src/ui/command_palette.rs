//! `Ctrl+/`: a searchable list of everything that can be done right now —
//! every action of the Global scope, the sidebar and the current page, with
//! its key, plus the `:` commands. Enter runs the highlighted entry (a `:`
//! command that takes arguments opens the command line with its name typed).
use crate::app::App;
use crate::commands::{self, ArgSpec};
use crate::focus::Scope;
use crate::keybinds;
use adw::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy)]
enum Entry {
    Action { id: &'static str, scope: Scope },
    Command(&'static commands::Command),
}

struct Item {
    row: gtk::ListBoxRow,
    title: String,
    haystack: String,
    entry: Entry,
}

/// Motions and duplicates: not things to pick from a list.
fn hidden(id: &str) -> bool {
    matches!(
        id,
        "global.command-palette"
            | "global.escape"
            | "global.quit-key"
            | "global.leave-terminal"
            | "global.down"
            | "global.up"
            | "global.left"
            | "global.right"
            | "global.top"
            | "global.bottom"
    ) || id.ends_with("-enter")
}

pub fn show(app: &Rc<App>) {
    let origin = match app.focus.current() {
        Scope::Global | Scope::QueryBar | Scope::Palette => app.last_pane.get(),
        s => s,
    };
    // Scopes on screen: the focused pane first, then Global, then the rest.
    let mut scopes: Vec<Scope> = Vec::new();
    if app.split.shows_sidebar() {
        scopes.push(Scope::Sidebar);
    }
    if let Some((_, s)) = app.focus.content_root() {
        scopes.push(s);
    }
    for s in [Scope::Editor, Scope::Shell] {
        if app.focus.root_of(s).is_some() {
            scopes.push(s);
        }
    }
    scopes.sort_by_key(|s| *s != origin);
    scopes.insert(
        if scopes.first() == Some(&origin) {
            1
        } else {
            0
        },
        Scope::Global,
    );
    scopes.dedup();

    let dialog = adw::Dialog::builder()
        .title("Command palette")
        .content_width(560)
        .content_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Type to search actions and commands")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .build();
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);

    let settings = app.config.borrow().settings.clone();
    let mut items: Vec<Item> = Vec::new();
    for scope in &scopes {
        for (a, accel) in keybinds::merged(&settings) {
            if a.scope != *scope || hidden(a.id) {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(gtk::glib::markup_escape_text(a.title))
                .subtitle(scope.title())
                .activatable(true)
                .build();
            let key = gtk::Label::new(Some(&keybinds::pretty_accel(&accel)));
            key.add_css_class("dim-label");
            key.add_css_class("viti-mono");
            key.set_valign(gtk::Align::Center);
            row.add_suffix(&key);
            items.push(Item {
                row: row.upcast(),
                title: a.title.to_lowercase(),
                haystack: format!("{} {} {} {}", a.title, scope.title(), a.id, accel)
                    .to_lowercase(),
                entry: Entry::Action {
                    id: a.id,
                    scope: a.scope,
                },
            });
        }
    }
    for c in commands::COMMANDS {
        let mut names = vec![c.name.to_string()];
        names.extend(c.aliases.iter().map(|s| s.to_string()));
        let row = adw::ActionRow::builder()
            .title(format!(":{}", names.join("  :")))
            .subtitle(gtk::glib::markup_escape_text(c.help))
            .activatable(true)
            .build();
        let key = gtk::Label::new(Some("Command line"));
        key.add_css_class("dim-label");
        key.set_valign(gtk::Align::Center);
        row.add_suffix(&key);
        items.push(Item {
            row: row.upcast(),
            title: format!(":{}", c.name),
            haystack: format!("{} {} command line", names.join(" "), c.help).to_lowercase(),
            entry: Entry::Command(c),
        });
    }
    for it in &items {
        list.append(&it.row);
    }
    let items = Rc::new(items);
    let needle: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    // The chosen entry runs once the dialog has gone, so focus lands back on
    // a pane and the action sees the scope it belongs to.
    let pending: Rc<RefCell<Option<Entry>>> = Rc::new(RefCell::new(None));
    let choose = {
        let pending = pending.clone();
        let dialog = dialog.clone();
        Rc::new(move |e: Entry| {
            *pending.borrow_mut() = Some(e);
            dialog.close();
        })
    };
    {
        let app = app.clone();
        dialog.connect_closed(move |_| {
            if let Some(e) = pending.borrow_mut().take() {
                let app = app.clone();
                crate::ui::idle(move || perform(&app, e, origin));
            }
        });
    }
    {
        let items = items.clone();
        let choose = choose.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(it) = items.iter().find(|i| i.row == *row) {
                choose(it.entry);
            }
        });
    }
    {
        let items = items.clone();
        let list = list.clone();
        let choose = choose.clone();
        search.connect_activate(move |_| {
            if let Some(row) = list.selected_row()
                && let Some(it) = items.iter().find(|i| i.row == row)
            {
                choose(it.entry);
            }
        });
    }

    // Filter: every word must appear; sort: title matches first, then the
    // original order.
    {
        let items = items.clone();
        let needle = needle.clone();
        list.set_filter_func(move |row| {
            let n = needle.borrow();
            if n.is_empty() {
                return true;
            }
            items
                .iter()
                .find(|i| i.row == *row)
                .is_some_and(|it| n.split_whitespace().all(|w| it.haystack.contains(w)))
        });
    }
    {
        let items = items.clone();
        let needle = needle.clone();
        list.set_sort_func(move |a, b| {
            let n = needle.borrow();
            let rank = |row: &gtk::ListBoxRow| -> (u8, usize) {
                let Some(pos) = items.iter().position(|i| i.row == *row) else {
                    return (3, 0);
                };
                let title = &items[pos].title;
                let r = if n.is_empty() || title.starts_with(n.as_str()) {
                    0
                } else if title.contains(n.as_str()) {
                    1
                } else {
                    2
                };
                (r, pos)
            };
            rank(a).cmp(&rank(b)).into()
        });
    }
    {
        let list = list.clone();
        let needle = needle.clone();
        search.connect_search_changed(move |e| {
            *needle.borrow_mut() = e.text().trim().to_lowercase();
            list.invalidate_filter();
            list.invalidate_sort();
            select_visible(&list, 0, 0);
        });
    }

    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.append(&search);
    body.append(&scroller);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        let list = list.clone();
        let scroller = scroller.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            match key {
                Key::Escape => {
                    dialog.close();
                }
                Key::Down | Key::Up => {
                    let cur = list.selected_row().map(|r| r.index()).unwrap_or(-1);
                    let dir = if key == Key::Down { 1 } else { -1 };
                    if let Some(row) = select_visible(&list, cur, dir) {
                        reveal(&scroller, &row);
                    }
                }
                _ => return gtk::glib::Propagation::Proceed,
            }
            gtk::glib::Propagation::Stop
        });
    }
    dialog.add_controller(keys);
    select_visible(&list, 0, 0);
    dialog.present(Some(&app.window));
    search.grab_focus();
}

/// Select the first row that survives the filter, walking from index `from`
/// in direction `dir` (`0`: `from` itself, then downwards).
fn select_visible(list: &gtk::ListBox, from: i32, dir: i32) -> Option<gtk::ListBoxRow> {
    let step = if dir == 0 { 1 } else { dir };
    let mut i = from + dir;
    loop {
        if i < 0 {
            return None;
        }
        let row = list.row_at_index(i)?;
        if row.is_child_visible() {
            list.select_row(Some(&row));
            return Some(row);
        }
        i += step;
    }
}

/// Scroll just enough to show `row`.
fn reveal(scroller: &gtk::ScrolledWindow, row: &gtk::ListBoxRow) {
    let Some(list) = row.parent() else { return };
    let Some(bounds) = row.compute_bounds(&list) else {
        return;
    };
    let (y, h) = (bounds.y() as f64, bounds.height() as f64);
    let adj = scroller.vadjustment();
    let (value, page) = (adj.value(), adj.page_size());
    if y < value {
        adj.set_value(y);
    } else if y + h > value + page {
        adj.set_value(y + h - page);
    }
}

fn perform(app: &Rc<App>, entry: Entry, origin: Scope) {
    match entry {
        Entry::Action { id, scope } => {
            // Put the vi focus where the action lives before running it.
            let pane = if scope == Scope::Global {
                origin
            } else {
                scope
            };
            if pane != Scope::Global {
                app.focus.focus(pane);
            }
            app.run_action(id);
        }
        Entry::Command(c) => {
            if matches!(c.args, ArgSpec::None) {
                app.run_command(c.name);
            } else {
                app.open_palette(&format!("{} ", c.name));
            }
        }
    }
}
