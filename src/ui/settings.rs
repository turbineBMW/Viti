//! Ctrl+,: General, Editor & shell, Appearance, Keybindings, Notifications.
use crate::app::App;
use crate::config::{DocView, EditorMode, SecretStore, Sound, Theme};
use crate::focus::Scope;
use crate::keybinds;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::RefCell;
use std::rc::Rc;

pub fn show(app: &Rc<App>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Settings");
    dialog.set_search_enabled(true);

    // --- General ---
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    let group = adw::PreferencesGroup::builder().title("Behaviour").build();
    let cfg = app.config.borrow().settings.clone();

    let ro = adw::SwitchRow::builder()
        .title("Read-only mode")
        .subtitle("Block inserts, updates and deletes")
        .active(cfg.read_only)
        .build();
    {
        let app = app.clone();
        ro.connect_active_notify(move |r| {
            app.config.borrow_mut().settings.read_only = r.is_active();
            app.schedule_save();
            app.update_title();
        });
    }
    group.add(&ro);

    let adj = gtk::Adjustment::new(
        cfg.max_time_ms as f64,
        100.0,
        3_600_000.0,
        1000.0,
        10_000.0,
        0.0,
    );
    let mt = adw::SpinRow::builder()
        .title("Max time per operation (ms)")
        .subtitle("Upper limit for every query's maxTimeMS")
        .adjustment(&adj)
        .digits(0)
        .build();
    {
        let app = app.clone();
        adj.connect_value_changed(move |a| {
            app.config.borrow_mut().settings.max_time_ms = a.value() as u64;
            app.schedule_save();
        });
    }
    group.add(&mt);

    let sizes = gtk::StringList::new(&["25", "50", "75", "100"]);
    let ps = adw::ComboRow::builder()
        .title("Documents per page")
        .model(&sizes)
        .build();
    ps.set_selected(match cfg.page_size {
        50 => 1,
        75 => 2,
        100 => 3,
        _ => 0,
    });
    {
        let app = app.clone();
        ps.connect_selected_notify(move |r| {
            app.config.borrow_mut().settings.page_size = [25, 50, 75, 100][r.selected() as usize];
            app.schedule_save();
        });
    }
    group.add(&ps);

    let sadj = gtk::Adjustment::new(
        cfg.schema_sample_size as f64,
        10.0,
        100_000.0,
        100.0,
        1000.0,
        0.0,
    );
    let ss = adw::SpinRow::builder()
        .title("Schema sample size")
        .subtitle("Documents sampled by the Schema page and “generate from schema”")
        .adjustment(&sadj)
        .digits(0)
        .build();
    {
        let app = app.clone();
        sadj.connect_value_changed(move |a| {
            app.config.borrow_mut().settings.schema_sample_size = a.value() as u32;
            app.schedule_save();
        });
    }
    group.add(&ss);

    let views = gtk::StringList::new(&["List", "JSON", "Table"]);
    let dv = adw::ComboRow::builder()
        .title("Default documents view")
        .model(&views)
        .build();
    dv.set_selected(match cfg.default_view {
        DocView::List => 0,
        DocView::Json => 1,
        DocView::Table => 2,
    });
    {
        let app = app.clone();
        dv.connect_selected_notify(move |r| {
            app.config.borrow_mut().settings.default_view = match r.selected() {
                1 => DocView::Json,
                2 => DocView::Table,
                _ => DocView::List,
            };
            app.schedule_save();
        });
    }
    group.add(&dv);

    let sort = adw::EntryRow::builder()
        .title("Default sort (e.g. { _id: -1 })")
        .text(&cfg.default_sort)
        .show_apply_button(true)
        .build();
    {
        let app = app.clone();
        sort.connect_apply(move |r| {
            app.config.borrow_mut().settings.default_sort = r.text().trim().to_string();
            app.schedule_save();
        });
    }
    group.add(&sort);

    let protect = adw::SwitchRow::builder()
        .title("Protect connection strings")
        .subtitle("Hide credentials when showing or copying URIs")
        .active(cfg.protect_connection_strings)
        .build();
    {
        let app = app.clone();
        protect.connect_active_notify(move |r| {
            app.config.borrow_mut().settings.protect_connection_strings = r.is_active();
            app.schedule_save();
        });
    }
    group.add(&protect);

    let stores = gtk::StringList::new(&[
        "Keyring (secret service)",
        "File (~/.config/viti/secrets.json, 0600)",
        "Never store",
    ]);
    let store_row = adw::ComboRow::builder()
        .title("Password storage")
        .model(&stores)
        .build();
    store_row.set_selected(match cfg.secret_store {
        SecretStore::Keyring => 0,
        SecretStore::File => 1,
        SecretStore::None => 2,
    });
    {
        let app = app.clone();
        store_row.connect_selected_notify(move |r| {
            app.config.borrow_mut().settings.secret_store = match r.selected() {
                0 => SecretStore::Keyring,
                1 => SecretStore::File,
                _ => SecretStore::None,
            };
            app.schedule_save();
        });
    }
    group.add(&store_row);
    general.add(&group);

    let theme_group = adw::PreferencesGroup::builder().title("Theme").build();
    let themes = gtk::StringList::new(&["Follow system", "Light", "Dark"]);
    let theme_row = adw::ComboRow::builder()
        .title("Colour scheme")
        .model(&themes)
        .build();
    theme_row.set_selected(match cfg.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    {
        let app = app.clone();
        theme_row.connect_selected_notify(move |r| {
            app.config.borrow_mut().settings.theme = match r.selected() {
                1 => Theme::Light,
                2 => Theme::Dark,
                _ => Theme::System,
            };
            app.apply_theme();
            app.schedule_save();
        });
    }
    theme_group.add(&theme_row);
    general.add(&theme_group);
    dialog.add(&general);

    // --- Editor & shell ---
    let editor = adw::PreferencesPage::builder()
        .title("Editor")
        .icon_name("text-editor-symbolic")
        .build();
    let eg = adw::PreferencesGroup::builder()
        .title("Document editor")
        .description(
            "What e and the pencil open. The external editor runs in the embedded terminal on a temporary Extended JSON file (empty command uses $EDITOR); setting a command always uses it. Shift+E is always external.",
        )
        .build();
    let modes = gtk::StringList::new(&["In-app editor", "External editor"]);
    let mode_row = adw::ComboRow::builder()
        .title("Edit with")
        .model(&modes)
        .build();
    mode_row.set_selected(match cfg.editor_mode {
        EditorMode::InApp => 0,
        EditorMode::External => 1,
    });
    {
        let app = app.clone();
        mode_row.connect_selected_notify(move |r| {
            app.config.borrow_mut().settings.editor_mode = match r.selected() {
                1 => EditorMode::External,
                _ => EditorMode::InApp,
            };
            app.schedule_save();
        });
    }
    eg.add(&mode_row);
    let ed = adw::EntryRow::builder()
        .title("Editor command")
        .text(&cfg.editor_command)
        .show_apply_button(true)
        .build();
    {
        let app = app.clone();
        ed.connect_apply(move |r| {
            app.config.borrow_mut().settings.editor_command = r.text().trim().to_string();
            app.schedule_save();
        });
    }
    eg.add(&ed);
    let sh = adw::EntryRow::builder()
        .title("mongosh command")
        .text(&cfg.mongosh_command)
        .show_apply_button(true)
        .build();
    {
        let app = app.clone();
        sh.connect_apply(move |r| {
            app.config.borrow_mut().settings.mongosh_command = r.text().trim().to_string();
            app.schedule_save();
        });
    }
    eg.add(&sh);
    editor.add(&eg);

    let ai = adw::PreferencesGroup::builder().title("AI backend").description("Natural-language query and pipeline generation shell out to a local CLI. Results are shown for review, never run automatically.").build();
    let backends = gtk::StringList::new(&["claude -p", "codex exec", "Custom command"]);
    let be = adw::ComboRow::builder()
        .title("Backend")
        .model(&backends)
        .build();
    be.set_selected(match cfg.ai.backend.as_str() {
        "codex" => 1,
        "custom" => 2,
        _ => 0,
    });
    let custom = adw::EntryRow::builder()
        .title("Custom command (reads the prompt on stdin)")
        .text(shell_words::join(&cfg.ai.custom_argv))
        .show_apply_button(true)
        .build();
    custom.set_visible(cfg.ai.backend == "custom");
    {
        let app = app.clone();
        let custom = custom.clone();
        be.connect_selected_notify(move |r| {
            let b = match r.selected() {
                1 => "codex",
                2 => "custom",
                _ => "claude",
            };
            custom.set_visible(b == "custom");
            app.config.borrow_mut().settings.ai.backend = b.into();
            app.schedule_save();
        });
    }
    {
        let app = app.clone();
        custom.connect_apply(move |r| {
            app.config.borrow_mut().settings.ai.custom_argv =
                shell_words::split(&r.text()).unwrap_or_default();
            app.schedule_save();
        });
    }
    let custom_json = adw::SwitchRow::builder()
        .title("Custom command prints claude-style JSON")
        .subtitle("The reply is read from the `result` field instead of the whole output")
        .active(cfg.ai.custom_json_result)
        .visible(cfg.ai.backend == "custom")
        .build();
    {
        let app = app.clone();
        custom_json.connect_active_notify(move |r| {
            app.config.borrow_mut().settings.ai.custom_json_result = r.is_active();
            app.schedule_save();
        });
    }
    custom
        .bind_property("visible", &custom_json, "visible")
        .sync_create()
        .build();
    let samples = adw::SwitchRow::builder()
        .title("Send sample field values")
        .subtitle("Otherwise only field names and types are sent")
        .active(cfg.ai.include_sample_values)
        .build();
    {
        let app = app.clone();
        samples.connect_active_notify(move |r| {
            app.config.borrow_mut().settings.ai.include_sample_values = r.is_active();
            app.schedule_save();
        });
    }
    ai.add(&be);
    ai.add(&custom);
    ai.add(&custom_json);
    ai.add(&samples);
    editor.add(&ai);
    dialog.add(&editor);

    // --- Appearance ---
    let appearance = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name("applications-graphics-symbolic")
        .build();
    let ag = adw::PreferencesGroup::builder()
        .title("Stylesheet")
        .description(
            "Rules in style.css beat the theme and Viti's own CSS, and apply live when saved.",
        )
        .build();
    let open = adw::ActionRow::builder()
        .title("Open style.css")
        .subtitle(crate::style::path().to_string_lossy())
        .activatable(true)
        .build();
    open.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let app = app.clone();
        open.connect_activated(move |_| {
            crate::style::load();
            let file = gtk::gio::File::for_path(crate::style::path());
            let launcher = gtk::FileLauncher::new(Some(&file));
            launcher.launch(Some(&app.window), gtk::gio::Cancellable::NONE, |r| {
                if let Err(e) = r {
                    tracing::warn!("open style.css: {e}");
                }
            });
        });
    }
    ag.add(&open);
    let edit_in = adw::ActionRow::builder()
        .title("Edit style.css in the embedded editor")
        .activatable(true)
        .build();
    edit_in.add_suffix(&gtk::Image::from_icon_name("text-editor-symbolic"));
    {
        let app = app.clone();
        let dialog = dialog.clone();
        edit_in.connect_activated(move |_| {
            dialog.close();
            app.edit_file_in_pane(crate::style::path(), "style.css");
        });
    }
    ag.add(&edit_in);
    appearance.add(&ag);
    dialog.add(&appearance);

    // --- Keybindings ---
    let keys_page = adw::PreferencesPage::builder()
        .title("Keys")
        .icon_name("input-keyboard-symbolic")
        .build();
    let labels: Rc<RefCell<Vec<(&'static str, gtk::Label)>>> = Rc::new(RefCell::new(Vec::new()));
    let mut scopes: Vec<Scope> = Vec::new();
    for a in keybinds::ACTIONS {
        if !scopes.contains(&a.scope) {
            scopes.push(a.scope);
        }
    }
    let intro = adw::PreferencesGroup::builder()
        .description("Click a row, then press the new key. Backspace unbinds, Esc cancels. Single letters are allowed for pane keys (they only fire outside text fields). Also editable in keybindings.json.")
        .build();
    let open_kb = adw::ActionRow::builder()
        .title("Open keybindings.json")
        .subtitle(crate::config::keybindings_path().to_string_lossy())
        .activatable(true)
        .build();
    open_kb.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let app = app.clone();
        let dialog = dialog.clone();
        open_kb.connect_activated(move |_| {
            dialog.close();
            app.save_now();
            app.edit_file_in_pane(crate::config::keybindings_path(), "keybindings.json");
        });
    }
    intro.add(&open_kb);
    keys_page.add(&intro);
    for scope in scopes {
        let g = adw::PreferencesGroup::builder()
            .title(scope.title())
            .build();
        for (action, accel) in keybinds::merged(&app.config.borrow().settings) {
            if action.scope != scope {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(action.title)
                .subtitle(action.id)
                .activatable(true)
                .build();
            let label = gtk::Label::new(Some(&keybinds::pretty_accel(&accel)));
            label.add_css_class("dim-label");
            label.add_css_class("viti-mono");
            label.set_valign(gtk::Align::Center);
            row.add_suffix(&label);
            labels.borrow_mut().push((action.id, label));
            let app = app.clone();
            let labels = labels.clone();
            row.connect_activated(move |_| capture_binding(&app, action.id, action.title, &labels));
            g.add(&row);
        }
        keys_page.add(&g);
    }
    dialog.add(&keys_page);

    // --- Notifications ---
    let notif = adw::PreferencesPage::builder()
        .title("Notifications")
        .icon_name("preferences-system-notifications-symbolic")
        .build();
    let ng = adw::PreferencesGroup::builder()
        .title("Desktop notifications")
        .description("Sent when a long operation finishes while the window is not focused.")
        .build();
    let nr = adw::SwitchRow::builder()
        .title("Enable")
        .active(cfg.desktop_notifications)
        .build();
    {
        let app = app.clone();
        nr.connect_active_notify(move |r| {
            app.config.borrow_mut().settings.desktop_notifications = r.is_active();
            app.schedule_save();
        });
    }
    ng.add(&nr);
    let (sound_row, file_row) = sound_rows(app);
    ng.add(&sound_row);
    ng.add(&file_row);
    let test = adw::ButtonRow::builder()
        .title("Send a test notification")
        .build();
    {
        let app = app.clone();
        test.connect_activated(move |_| app.notify("test", "Viti", "Notifications work."));
    }
    ng.add(&test);
    notif.add(&ng);
    dialog.add(&notif);

    dialog.present(Some(&app.window));
}

fn refresh_labels(app: &Rc<App>, labels: &Rc<RefCell<Vec<(&'static str, gtk::Label)>>>) {
    let cfg = app.config.borrow();
    for (id, label) in labels.borrow().iter() {
        label.set_text(&keybinds::pretty_accel(&keybinds::accel_for(
            &cfg.settings,
            id,
        )));
    }
}

fn capture_binding(
    app: &Rc<App>,
    action_id: &'static str,
    title: &str,
    labels: &Rc<RefCell<Vec<(&'static str, gtk::Label)>>>,
) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Shortcut for “{title}”")),
        Some("Press a key combination.\nBackspace unbinds · Esc cancels"),
    );
    dialog.add_responses(&[("cancel", "Cancel")]);
    dialog.set_close_response("cancel");
    let global = keybinds::find(action_id)
        .map(|a| a.scope == Scope::Global && a.text_safe)
        .unwrap_or(false);
    let ctl = gtk::EventControllerKey::new();
    ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let app = app.clone();
        let labels = labels.clone();
        let dialog = dialog.clone();
        ctl.connect_key_pressed(move |_, keyval, _code, state| {
            let mods = state & gtk::accelerator_get_default_mod_mask();
            if mods.is_empty() {
                if keyval == gtk::gdk::Key::Escape {
                    dialog.close();
                    return glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::BackSpace {
                    apply(&app, action_id, "", &labels);
                    dialog.close();
                    return glib::Propagation::Stop;
                }
            }
            let keyval = keyval.to_lower();
            if !gtk::accelerator_valid(keyval, mods) {
                return glib::Propagation::Stop;
            }
            // Global chords must keep a modifier (they fire inside text fields).
            let chord = mods.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SUPER_MASK,
            );
            let fkey = (gtk::gdk::Key::F1..=gtk::gdk::Key::F12).contains(&keyval);
            if global && !chord && !fkey {
                return glib::Propagation::Stop;
            }
            let accel = gtk::accelerator_name(keyval, mods).to_string();
            apply(&app, action_id, &accel, &labels);
            dialog.close();
            glib::Propagation::Stop
        });
    }
    dialog.add_controller(ctl);
    dialog.present(Some(&app.window));
}

fn apply(
    app: &Rc<App>,
    action_id: &str,
    accel: &str,
    labels: &Rc<RefCell<Vec<(&'static str, gtk::Label)>>>,
) {
    keybinds::apply_binding(&mut app.config.borrow_mut().settings, action_id, accel);
    app.save_now();
    app.reinstall_shortcuts();
    refresh_labels(app, labels);
}

fn sound_rows(app: &Rc<App>) -> (adw::ComboRow, adw::ActionRow) {
    let choices = gtk::StringList::new(&["System default", "Custom file", "None"]);
    let sound_row = adw::ComboRow::builder()
        .title("Sound")
        .subtitle("Requested from the notification daemon, so do-not-disturb still applies")
        .model(&choices)
        .build();
    let file_row = adw::ActionRow::builder()
        .title("Sound file")
        .activatable(true)
        .build();
    file_row.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    let current = app.config.borrow().settings.notification_sound.clone();
    sound_row.set_selected(match &current {
        Sound::SystemDefault => 0,
        Sound::File(_) => 1,
        Sound::None => 2,
    });
    if let Sound::File(p) = &current {
        file_row.set_subtitle(&p.to_string_lossy());
    }
    file_row.set_visible(matches!(current, Sound::File(_)));
    {
        let app = app.clone();
        let file_row = file_row.clone();
        sound_row.connect_selected_notify(move |row| {
            {
                let mut cfg = app.config.borrow_mut();
                cfg.settings.notification_sound = match row.selected() {
                    0 => Sound::SystemDefault,
                    1 => match &cfg.settings.notification_sound {
                        Sound::File(p) => Sound::File(p.clone()),
                        _ => Sound::File(std::path::PathBuf::new()),
                    },
                    _ => Sound::None,
                };
            }
            file_row.set_visible(row.selected() == 1);
            app.schedule_save();
        });
    }
    {
        let app = app.clone();
        file_row.connect_activated(move |row| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Audio"));
            filter.add_mime_type("audio/*");
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let chooser = gtk::FileDialog::builder()
                .title("Choose a notification sound")
                .default_filter(&filter)
                .filters(&filters)
                .modal(true)
                .build();
            let window = app.window.clone();
            let app = app.clone();
            let row = row.clone();
            chooser.open(Some(&window), gtk::gio::Cancellable::NONE, move |res| {
                let Some(path) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                row.set_subtitle(&path.to_string_lossy());
                app.config.borrow_mut().settings.notification_sound = Sound::File(path);
                app.schedule_save();
            });
        });
    }
    (sound_row, file_row)
}
