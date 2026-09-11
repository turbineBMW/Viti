//! Shared widget helpers. Everything here runs on the GTK thread.
pub mod aggregation;
pub mod bulk;
pub mod collection;
pub mod connections;
pub mod documents;
pub mod editor_pane;
pub mod explain;
pub mod export;
pub mod export_lang;
pub mod help;
pub mod import;
pub mod indexes;
pub mod manage;
pub mod my_queries;
pub mod palette;
pub mod query_bar;
pub mod schema;
pub mod settings;
pub mod sidebar;
pub mod validation;
pub mod window;

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use sourceview5::prelude::*;
use std::cell::RefCell;

thread_local! {
    /// Every source buffer we created, so the colour scheme can follow dark/light.
    static BUFFERS: RefCell<Vec<glib::WeakRef<sourceview5::Buffer>>> = const { RefCell::new(Vec::new()) };
}

fn scheme_name() -> &'static str {
    if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    }
}

/// Call once: keep every JSON buffer's scheme in step with the app theme.
pub fn watch_scheme() {
    adw::StyleManager::default().connect_dark_notify(|_| {
        let scheme = sourceview5::StyleSchemeManager::default().scheme(scheme_name());
        BUFFERS.with(|b| {
            b.borrow_mut().retain(|w| w.upgrade().is_some());
            for w in b.borrow().iter() {
                if let Some(buf) = w.upgrade() {
                    buf.set_style_scheme(scheme.as_ref());
                }
            }
        });
    });
}

pub fn json_buffer(text: &str) -> sourceview5::Buffer {
    let lang = sourceview5::LanguageManager::default().language("json");
    let buffer = match lang {
        Some(l) => sourceview5::Buffer::with_language(&l),
        None => sourceview5::Buffer::new(None),
    };
    buffer.set_highlight_syntax(true);
    buffer.set_highlight_matching_brackets(true);
    buffer.set_style_scheme(
        sourceview5::StyleSchemeManager::default()
            .scheme(scheme_name())
            .as_ref(),
    );
    buffer.set_text(text);
    BUFFERS.with(|b| b.borrow_mut().push(buffer.downgrade()));
    buffer
}

/// A monospace JSON view. Read-only views still allow selection/copy.
pub fn json_view(text: &str, editable: bool) -> sourceview5::View {
    let view = sourceview5::View::with_buffer(&json_buffer(text));
    view.set_editable(editable);
    view.set_monospace(true);
    view.set_show_line_numbers(editable);
    view.set_tab_width(2);
    view.set_indent_width(2);
    view.set_insert_spaces_instead_of_tabs(true);
    view.set_auto_indent(editable);
    view.set_highlight_current_line(editable);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(6);
    view.set_bottom_margin(6);
    view.set_cursor_visible(editable);
    view
}

pub fn buffer_text(buffer: &impl IsA<gtk::TextBuffer>) -> String {
    let b = buffer.as_ref();
    b.text(&b.start_iter(), &b.end_iter(), true).to_string()
}

/// Yes/no dialog. `destructive` styles the confirm button red.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    ok_label: &str,
    destructive: bool,
    on_ok: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", ok_label)]);
    dialog.set_response_appearance(
        "ok",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, r| {
        if r == "ok" {
            on_ok();
        }
    });
    dialog.present(Some(parent));
}

/// Confirm by typing a name, for irreversible drops.
pub fn confirm_typed(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    expected: &str,
    ok_label: &str,
    on_ok: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    let entry = gtk::Entry::builder()
        .placeholder_text(expected)
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", ok_label)]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    dialog.set_response_enabled("ok", false);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    {
        let dialog = dialog.clone();
        let expected = expected.to_string();
        entry.connect_changed(move |e| dialog.set_response_enabled("ok", e.text() == expected));
    }
    dialog.connect_response(None, move |_, r| {
        if r == "ok" {
            on_ok();
        }
    });
    dialog.present(Some(parent));
    entry.grab_focus();
}

/// A read-only document dialog: `C` copies, `j`/`k`/`g`/`G` scroll, Esc
/// closes. Used wherever a document is shown outside the Documents pane.
pub fn peek_document(app: &std::rc::Rc<crate::app::App>, title: &str, text: &str) {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(760)
        .content_height(620)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy (C)")
        .build();
    header.pack_end(&copy);
    toolbar.add_top_bar(&header);
    let view = json_view(text, false);
    view.set_can_focus(true);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));
    {
        let text = text.to_string();
        let app = app.clone();
        copy.connect_clicked(move |_| {
            copy_text(&text);
            app.toast("Copied");
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        let text = text.to_string();
        let app = app.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            match key {
                Key::Escape | Key::q | Key::o | Key::O => {
                    dialog.close();
                    glib::Propagation::Stop
                }
                Key::C => {
                    copy_text(&text);
                    app.toast("Copied");
                    glib::Propagation::Stop
                }
                Key::j | Key::k | Key::g | Key::G => {
                    let adj = scroller.vadjustment();
                    let step = adj.step_increment().max(40.0);
                    adj.set_value(match key {
                        Key::j => adj.value() + step,
                        Key::k => adj.value() - step,
                        Key::g => adj.lower(),
                        _ => adj.upper(),
                    });
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
}

/// A source view for a language other than JSON (export to language).
pub fn source_view(text: &str, language: &str) -> sourceview5::View {
    let buffer = json_buffer(text);
    if let Some(l) = sourceview5::LanguageManager::default().language(language) {
        buffer.set_language(Some(&l));
    }
    let view = sourceview5::View::with_buffer(&buffer);
    view.set_editable(false);
    view.set_monospace(true);
    view.set_show_line_numbers(true);
    view.set_tab_width(4);
    view.set_wrap_mode(gtk::WrapMode::None);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(6);
    view.set_bottom_margin(6);
    view.set_cursor_visible(false);
    view
}

pub fn copy_text(text: &str) {
    if let Some(d) = gtk::gdk::Display::default() {
        d.clipboard().set_text(text);
    }
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Defer to the next main-loop iteration: the safe place to touch a widget
/// from inside one of its own callbacks.
pub fn idle(f: impl FnOnce() + 'static) {
    glib::idle_add_local_once(f);
}

/// The app icon as a paintable: the installed hicolor PNG, else the checkout's
/// `data/icons` (found next to the executable or in the working directory).
pub fn app_icon_paintable() -> Option<gtk::gdk::Texture> {
    let id = crate::notify::APP_ID;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(b) = directories::BaseDirs::new() {
        candidates.push(
            b.data_dir()
                .join(format!("icons/hicolor/512x512/apps/{id}.png")),
        );
    }
    candidates.push(format!("/usr/share/icons/hicolor/512x512/apps/{id}.png").into());
    if let Ok(exe) = std::env::current_exe() {
        for up in [2, 3] {
            let mut p = exe.clone();
            for _ in 0..up {
                p.pop();
            }
            candidates.push(p.join("data/icons/viti-512.png"));
        }
    }
    candidates.push("data/icons/viti-512.png".into());
    candidates
        .into_iter()
        .find(|p| p.exists())
        .and_then(|p| gtk::gdk::Texture::from_filename(p).ok())
}
