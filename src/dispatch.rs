//! Window-level key handling. Two capture-phase controllers:
//!
//! * a `ShortcutController` for modified chords (`<Control>o`…), which skips
//!   actions that are not `text_safe` while a text widget has focus, and
//!   everything except "leave terminal" while a terminal has focus;
//! * an `EventControllerKey` for normal-mode keys (`j`, `/`, `<Shift>a`…),
//!   which is inert while a text widget has focus (Escape excepted) and
//!   otherwise looks the key up in the focused pane's scope, then Global.
use crate::app::App;
use crate::focus::Scope;
use crate::keybinds;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::rc::Rc;

/// Whether the focused entry has a completion popover up (`ui::completer`).
fn completing(window: &gtk::Window) -> bool {
    let Some(w) = GtkWindowExt::focus(window) else {
        return false;
    };
    let class = crate::ui::completer::COMPLETING_CLASS;
    w.has_css_class(class) || w.parent().is_some_and(|p| p.has_css_class(class))
}

/// Whether the focused widget consumes typing.
pub fn text_has_focus(window: &gtk::Window) -> bool {
    let Some(w) = GtkWindowExt::focus(window) else {
        return false;
    };
    w.is::<gtk::Text>()
        || w.is::<gtk::Entry>()
        || w.is::<gtk::TextView>()
        || w.is::<vte4::Terminal>()
        || w.is::<gtk::SpinButton>()
        || w.is::<gtk::SearchEntry>()
        || w.is::<gtk::PasswordEntry>()
        || w.ancestor(gtk::Entry::static_type()).is_some()
        || w.ancestor(gtk::SpinButton::static_type()).is_some()
}

pub fn terminal_has_focus(window: &gtk::Window) -> bool {
    GtkWindowExt::focus(window).is_some_and(|w| w.is::<vte4::Terminal>())
}

pub fn install(app: &Rc<App>) {
    // Normal-mode keys.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let app = app.clone();
        keys.connect_key_pressed(move |_, keyval, _code, state| app.on_normal_key(keyval, state));
    }
    app.window.add_controller(keys);

    // Track the focused pane.
    {
        let app = app.clone();
        let window = app.window.clone();
        window.connect_focus_widget_notify(move |w| {
            let scope = app.focus.update(GtkWindowExt::focus(w));
            if scope != Scope::Global && !matches!(scope, Scope::Palette) {
                app.last_pane.set(scope);
            }
        });
    }
    app.reinstall_shortcuts();
}

impl App {
    /// (Re)build the chord controller from the current bindings.
    pub fn reinstall_shortcuts(self: &Rc<Self>) {
        if let Some(old) = self.shortcut_ctl.borrow_mut().take() {
            self.window.remove_controller(&old);
        }
        *self.keymap.borrow_mut() = keybinds::build_keymap(&self.config.borrow().settings);
        let ctl = gtk::ShortcutController::new();
        ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        for (action, accel) in keybinds::merged(&self.config.borrow().settings) {
            if accel.is_empty() || keybinds::is_bare(&accel) {
                continue;
            }
            let app = self.clone();
            let id = action.id;
            keybinds::add_sc(&ctl, &accel, move || app.on_chord(id));
        }
        self.window.add_controller(ctl.clone());
        *self.shortcut_ctl.borrow_mut() = Some(ctl);
    }

    fn on_chord(self: &Rc<Self>, id: &'static str) -> glib::Propagation {
        if self.window.visible_dialog().is_some() {
            return glib::Propagation::Proceed;
        }
        let Some(action) = keybinds::find(id) else {
            return glib::Propagation::Proceed;
        };
        let window = self.window.clone().upcast::<gtk::Window>();
        if terminal_has_focus(&window) && id != "global.leave-terminal" {
            return glib::Propagation::Proceed;
        }
        let scope = self.focus.current();
        let in_text = text_has_focus(&window);
        if in_text && !action.text_safe {
            // Pane-scoped chords still fire from that pane's own entries
            // (e.g. Ctrl+Y in the query bar), never from unrelated text.
            if !(action.scope == scope && scope != Scope::Global) {
                return glib::Propagation::Proceed;
            }
        }
        if action.scope != Scope::Global && action.scope != scope {
            return glib::Propagation::Proceed;
        }
        self.run_action(id)
    }

    fn on_normal_key(
        self: &Rc<Self>,
        keyval: gtk::gdk::Key,
        state: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        use gtk::gdk::Key;
        // Let modal dialogs own Tab, Enter and Escape as well as typing.
        if self.window.visible_dialog().is_some() {
            return glib::Propagation::Proceed;
        }
        let window = self.window.clone().upcast::<gtk::Window>();
        if terminal_has_focus(&window) {
            return glib::Propagation::Proceed;
        }
        let mods = state & gtk::accelerator_get_default_mod_mask();
        if text_has_focus(&window) {
            if keyval == Key::Escape && mods.is_empty() {
                // Let a palette entry close itself, and an entry with a
                // completion popover close that; otherwise blur to the pane.
                if self.focus.current() == Scope::Palette || completing(&window) {
                    return glib::Propagation::Proceed;
                }
                self.blur_to_pane();
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        // Only bare keys and Shift-combinations are normal-mode keys; chords
        // are the ShortcutController's business.
        let chord = mods.intersects(
            gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::META_MASK,
        );
        if chord {
            return glib::Propagation::Proceed;
        }
        if keyval == Key::Tab || keyval == Key::ISO_Left_Tab {
            self.focus
                .cycle(if keyval == Key::ISO_Left_Tab { -1 } else { 1 });
            return glib::Propagation::Stop;
        }
        let scope = self.focus.current();
        let lower = keyval.to_lower();
        // `?` arrives as keyval `question` with Shift held; bindings say plain
        // "question", so try with and without Shift.
        let candidates = [
            gtk::accelerator_name(lower, mods).to_string(),
            gtk::accelerator_name(lower, mods & !gtk::gdk::ModifierType::SHIFT_MASK).to_string(),
        ];
        let id = {
            let map = self.keymap.borrow();
            candidates
                .iter()
                .find_map(|c| {
                    map.get(&(scope, c.clone()))
                        .or_else(|| map.get(&(Scope::Global, c.clone())))
                })
                .copied()
        };
        match id {
            Some(id) => self.run_action(id),
            None => glib::Propagation::Proceed,
        }
    }

    /// Escape from a text entry: back to the pane that owns it.
    pub fn blur_to_pane(&self) {
        let target = self.last_pane.get();
        if !self.focus.focus(target) {
            GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
    }
}
