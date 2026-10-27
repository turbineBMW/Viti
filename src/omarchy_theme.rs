//! Apply the active Omarchy palette to GTK and follow theme switches live.

use crate::{accent, omarchy};
use adw::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// One theme switch is a burst of file events; reload once it settles.
const SETTLE: Duration = Duration::from_millis(120);

/// Owns the CSS provider and directory monitor for as long as the app lives.
pub struct OmarchyTheme {
    inner: Rc<Inner>,
}

struct Inner {
    enabled: Cell<bool>,
    fallback: Cell<adw::ColorScheme>,
    provider: gtk::CssProvider,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

impl OmarchyTheme {
    /// Install an initially empty provider above Viti's built-in CSS and the
    /// system-accent fallback. User style.css remains above this provider.
    pub fn install(display: &gtk::gdk::Display) -> Self {
        let inner = Rc::new(Inner {
            enabled: Cell::new(false),
            fallback: Cell::new(adw::ColorScheme::Default),
            provider: gtk::CssProvider::new(),
            monitor: RefCell::new(None),
        });
        gtk::style_context_add_provider_for_display(
            display,
            &inner.provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2,
        );
        inner.provider.connect_parsing_error(|_, section, error| {
            tracing::warn!(
                "Omarchy theme CSS line {}: {error}",
                section.start_location().lines() + 1
            );
        });
        inner.watch();
        Self { inner }
    }

    /// Apply the preference and the colour scheme Viti should restore when
    /// Omarchy is disabled or unavailable.
    pub fn apply(&self, enabled: bool, fallback: adw::ColorScheme) {
        self.inner.enabled.set(enabled);
        self.inner.fallback.set(fallback);
        self.inner.reload();
    }
}

impl Inner {
    fn reload(&self) {
        let theme = self
            .enabled
            .get()
            .then(|| omarchy::load(&glib::home_dir()))
            .flatten();
        self.provider
            .load_from_string(theme.as_ref().map_or("", |theme| theme.css.as_str()));

        let manager = adw::StyleManager::default();
        let was_dark = manager.is_dark();
        let accent_changed = accent::set_override(theme.as_ref().and_then(|t| t.accent.clone()));
        manager.set_color_scheme(match &theme {
            Some(theme) if theme.light => adw::ColorScheme::ForceLight,
            Some(_) => adw::ColorScheme::ForceDark,
            None => self.fallback.get(),
        });
        if accent_changed && manager.is_dark() == was_dark {
            accent::notify();
        }
    }

    /// `omarchy theme set` replaces `current/theme`, so monitor its stable
    /// parent rather than a file inside the directory that gets replaced.
    fn watch(self: &Rc<Self>) {
        if !omarchy::detected(&glib::home_dir()) {
            return;
        }
        let dir = omarchy::state_dir(&glib::home_dir());
        let monitor = match gio::File::for_path(&dir)
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            Ok(monitor) => monitor,
            Err(error) => {
                tracing::warn!("cannot watch {}: {error}", dir.display());
                return;
            }
        };
        let pending = Rc::new(Cell::new(false));
        monitor.connect_changed(glib::clone!(
            #[weak(rename_to = inner)]
            self,
            move |_, _, _, _| {
                if pending.replace(true) {
                    return;
                }
                let pending = pending.clone();
                glib::timeout_add_local_once(
                    SETTLE,
                    glib::clone!(
                        #[weak]
                        inner,
                        move || {
                            pending.set(false);
                            inner.reload();
                        }
                    ),
                );
            }
        ));
        self.monitor.replace(Some(monitor));
    }
}
