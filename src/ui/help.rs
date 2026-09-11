//! `?`: every binding, grouped by pane, with the focused pane first, plus the
//! `:` commands. A search entry filters titles and keys.
use crate::app::App;
use crate::focus::Scope;
use crate::keybinds;
use adw::prelude::*;
use gtk4 as gtk;
use std::rc::Rc;

pub fn show(app: &Rc<App>) {
    let dialog = adw::Dialog::builder()
        .title("Keybindings")
        .content_width(640)
        .content_height(720)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search keys and actions")
        .build();
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(6);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list-separate");
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);

    let current = app.focus.current();
    let settings = app.config.borrow().settings.clone();
    let mut scopes: Vec<Scope> = Vec::new();
    for (a, _) in keybinds::merged(&settings) {
        if !scopes.contains(&a.scope) {
            scopes.push(a.scope);
        }
    }
    scopes.sort_by_key(|s| (*s != current, *s != Scope::Global));

    for scope in scopes {
        let title = gtk::Label::builder()
            .label(scope.title())
            .xalign(0.0)
            .css_classes(["heading"])
            .margin_top(12)
            .margin_bottom(4)
            .build();
        let header_row = gtk::ListBoxRow::builder()
            .child(&title)
            .activatable(false)
            .selectable(false)
            .build();
        header_row.set_widget_name("header");
        list.append(&header_row);
        for (a, accel) in keybinds::merged(&settings) {
            if a.scope != scope {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(a.title)
                .activatable(false)
                .build();
            let label = gtk::Label::new(Some(&keybinds::pretty_accel(&accel)));
            label.add_css_class("dim-label");
            label.add_css_class("viti-mono");
            label.set_valign(gtk::Align::Center);
            row.add_suffix(&label);
            row.set_widget_name(
                &format!("{} {} {}", a.title, accel, keybinds::pretty_accel(&accel)).to_lowercase(),
            );
            list.append(&row);
        }
    }
    let title = gtk::Label::builder()
        .label("Command line (:)")
        .xalign(0.0)
        .css_classes(["heading"])
        .margin_top(12)
        .margin_bottom(4)
        .build();
    let header_row = gtk::ListBoxRow::builder()
        .child(&title)
        .activatable(false)
        .selectable(false)
        .build();
    header_row.set_widget_name("header");
    list.append(&header_row);
    for c in crate::commands::COMMANDS {
        let mut names = vec![c.name.to_string()];
        names.extend(c.aliases.iter().map(|s| s.to_string()));
        let row = adw::ActionRow::builder()
            .title(format!(":{}", names.join("  :")))
            .subtitle(c.help)
            .activatable(false)
            .build();
        row.add_css_class("viti-mono");
        row.set_widget_name(&format!("{} {}", names.join(" "), c.help).to_lowercase());
        list.append(&row);
    }

    {
        let list = list.clone();
        search.connect_search_changed(move |e| {
            let needle = e.text().to_lowercase();
            list.set_filter_func(move |row| {
                if row.widget_name() == "header" {
                    return true;
                }
                needle.is_empty() || row.widget_name().contains(&needle)
            });
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
    {
        let dialog = dialog.clone();
        let search = search.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                dialog.close();
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::slash && !search.has_focus() {
                search.grab_focus();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
    search.grab_focus();
}
