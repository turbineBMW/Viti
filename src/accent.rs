//! The system accent colour, for the parts of the UI that CSS can't reach
//! (the terminal palette, drawn charts).
//!
//! libadwaita learns the accent from the settings portal, which only carries
//! it under GNOME's portal backend. On other compositors the portal answers
//! "not found" and libadwaita silently falls back to blue, so when it reports
//! no system support we read `org.gnome.desktop.interface accent-color`
//! ourselves and override its `--accent-*` CSS variables.
//!
//! A followed Omarchy theme outranks both. Its CSS handles widgets, while
//! [`set_override`] exposes the same accent to custom-drawn UI.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::{gio, glib};

const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";

thread_local! {
    static FALLBACK: RefCell<Option<Fallback>> = const { RefCell::new(None) };
    static OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Each returns false once its widget is gone, and is then dropped.
    static WATCHERS: RefCell<Vec<Box<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
    static HOOKED: Cell<bool> = const { Cell::new(false) };
}

struct Fallback {
    settings: gio::Settings,
    provider: gtk::CssProvider,
}

/// The accent as a CSS hex colour, tracking the GNOME setting through
/// libadwaita's style manager, or through GSettings where the portal
/// doesn't relay it.
pub fn accent_hex() -> String {
    rgba_hex(&accent_rgba())
}

/// The current accent for custom drawing, including an Omarchy override.
pub fn accent_rgba() -> gtk::gdk::RGBA {
    if let Some(accent) = OVERRIDE.with(|cell| cell.borrow().clone())
        && let Ok(rgba) = gtk::gdk::RGBA::parse(&accent)
    {
        return rgba;
    }
    let manager = adw::StyleManager::default();
    match fallback_accent() {
        Some(accent) => accent.to_standalone_rgba(manager.is_dark()),
        None => manager.accent_color_rgba(),
    }
}

pub fn rgba_hex(color: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8
    )
}

/// Run `on_change` whenever the accent or the dark/light scheme flips, for
/// as long as `widget` is alive.
pub fn watch<W: IsA<glib::Object>>(widget: &W, on_change: impl Fn(&W) + 'static) {
    if !HOOKED.replace(true) {
        let manager = adw::StyleManager::default();
        manager.connect_accent_color_notify(|_| notify());
        manager.connect_dark_notify(|_| notify());
    }
    let widget = widget.downgrade();
    let watcher = move || match widget.upgrade() {
        Some(widget) => {
            on_change(&widget);
            true
        }
        None => false,
    };
    WATCHERS.with(|cell| cell.borrow_mut().push(Box::new(watcher)));
}

/// Replace the accent with a theme colour. The caller coordinates notifying
/// watchers with any simultaneous dark/light change.
pub fn set_override(accent: Option<String>) -> bool {
    OVERRIDE.with(|cell| {
        let changed = *cell.borrow() != accent;
        cell.replace(accent);
        changed
    })
}

/// Run every callback registered with [`watch`].
pub fn notify() {
    // Taken out while they run, so a callback may register another.
    let mut watchers = WATCHERS.with(|cell| cell.take());
    watchers.retain(|on_change| on_change());
    WATCHERS.with(|cell| {
        let mut cell = cell.borrow_mut();
        watchers.append(&mut cell);
        *cell = watchers;
    });
}

/// Install the GSettings fallback if libadwaita can't see the system accent.
/// Call once, after the display exists and before the first window.
pub fn install_fallback(display: &gtk::gdk::Display) {
    let manager = adw::StyleManager::default();
    if manager.is_system_supports_accent_colors() {
        return;
    }
    let source = gio::SettingsSchemaSource::default();
    if source.is_none_or(|s| s.lookup(INTERFACE_SCHEMA, true).is_none()) {
        tracing::info!("no {INTERFACE_SCHEMA} schema; keeping libadwaita's default accent");
        return;
    }
    let settings = gio::Settings::new(INTERFACE_SCHEMA);
    if !settings
        .settings_schema()
        .is_some_and(|s| s.has_key("accent-color"))
    {
        return;
    }
    tracing::info!("portal has no accent colour; following {INTERFACE_SCHEMA} accent-color");

    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let fallback = Fallback { settings, provider };
    apply(&fallback, manager.is_dark());
    FALLBACK.with(|cell| *cell.borrow_mut() = Some(fallback));

    let refresh = || {
        let dark = adw::StyleManager::default().is_dark();
        FALLBACK.with(|cell| {
            if let Some(fallback) = cell.borrow().as_ref() {
                apply(fallback, dark);
            }
        });
    };
    FALLBACK.with(|cell| {
        if let Some(fallback) = cell.borrow().as_ref() {
            fallback
                .settings
                .connect_changed(Some("accent-color"), move |_, _| {
                    refresh();
                    notify();
                });
        }
    });
    manager.connect_dark_notify(move |_| refresh());
}

fn fallback_accent() -> Option<adw::AccentColor> {
    FALLBACK.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|f| parse(&f.settings.string("accent-color")))
    })
}

fn apply(fallback: &Fallback, dark: bool) {
    let accent = parse(&fallback.settings.string("accent-color"));
    let css = format!(
        ":root {{ --accent-bg-color: {}; --accent-fg-color: #ffffff; --accent-color: {}; }}",
        rgba_hex(&accent.to_rgba()),
        rgba_hex(&accent.to_standalone_rgba(dark)),
    );
    fallback.provider.load_from_string(&css);
}

fn parse(name: &str) -> adw::AccentColor {
    use adw::AccentColor::*;
    match name {
        "teal" => Teal,
        "green" => Green,
        "yellow" => Yellow,
        "orange" => Orange,
        "red" => Red,
        "pink" => Pink,
        "purple" => Purple,
        "slate" => Slate,
        _ => Blue,
    }
}
