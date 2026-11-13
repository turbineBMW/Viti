//! Apply the active Omarchy palette to GTK and follow theme switches live.

use crate::{accent, omarchy, style};
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
    /// `--viti-sidebar-opacity` as the loaded theme states it, if it does.
    sidebar_opacity: Cell<Option<f32>>,
    /// Run after every reload, once the provider holds the new CSS.
    on_reload: RefCell<Option<Box<dyn Fn()>>>,
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
            sidebar_opacity: Cell::new(None),
            on_reload: RefCell::new(None),
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

    /// The sidebar opacity the active theme asks for, or `None` when it
    /// states none or the theme isn't being followed.
    pub fn sidebar_opacity(&self) -> Option<f32> {
        self.inner.sidebar_opacity.get()
    }

    /// Call `f` after each reload, including the ones a theme switch makes.
    pub fn connect_reloaded(&self, f: impl Fn() + 'static) {
        self.inner.on_reload.replace(Some(Box::new(f)));
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
        self.sidebar_opacity.set(
            theme
                .as_ref()
                .and_then(|theme| style::opacity_in(&theme.css, style::SIDEBAR_OPACITY)),
        );

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
        if let Some(on_reload) = self.on_reload.borrow().as_ref() {
            on_reload();
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
