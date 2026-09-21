//! A completion popover under a `gtk::Entry`: field names, operators and
//! constructors from `query_complete`, refreshed as the user types. Tab accepts
//! the highlighted row, Up/Down (Ctrl+N/P) move, Enter accepts only after the
//! user moved the highlight (so a bare Enter still runs the query), Escape
//! dismisses until the text changes again, Ctrl+Space reopens.
//!
//! While the popover is up the entry carries `.viti-completing`, which the
//! window's Escape handler checks so it lets the entry close the popover
//! instead of blurring.
use crate::query_complete::{self, Completion, Field, Kind};
use adw::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const COMPLETING_CLASS: &str = "viti-completing";
const MAX_ROWS: usize = 60;

pub struct Completer {
    entry: gtk::Entry,
    popover: gtk::Popover,
    list: gtk::ListBox,
    kind: Kind,
    fields: RefCell<Rc<Vec<Field>>>,
    items: RefCell<Vec<Completion>>,
    selected: Cell<usize>,
    /// The user moved the highlight: Enter now means "accept".
    navigated: Cell<bool>,
    /// Escape was pressed: stay hidden until the text changes.
    dismissed: Cell<bool>,
    /// Set while we rewrite the entry ourselves.
    suppress: Cell<bool>,
}

impl Completer {
    pub fn attach(entry: &gtk::Entry, kind: Kind) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .can_focus(false)
            .focusable(false)
            .css_classes(["viti-complete-list"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .max_content_height(320)
            .hscrollbar_policy(gtk::PolicyType::Never)
            // No scrollbar: its minimum length would pad a one-row list.
            .vscrollbar_policy(gtk::PolicyType::External)
            .can_focus(false)
            .build();
        let popover = gtk::Popover::builder()
            .child(&scroller)
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .can_focus(false)
            .css_classes(["viti-complete"])
            .build();
        popover.set_parent(entry);

        let me = Rc::new(Self {
            entry: entry.clone(),
            popover,
            list,
            kind,
            fields: RefCell::new(Rc::new(Vec::new())),
            items: RefCell::new(Vec::new()),
            selected: Cell::new(0),
            navigated: Cell::new(false),
            dismissed: Cell::new(false),
            suppress: Cell::new(false),
        });
        {
            let c = me.clone();
            entry.connect_changed(move |_| {
                if c.suppress.get() {
                    return;
                }
                c.dismissed.set(false);
                if c.entry_focused() {
                    c.refresh();
                } else {
                    c.hide();
                }
            });
        }
        {
            let c = me.clone();
            me.list.connect_row_activated(move |_, row| {
                c.accept(row.index().max(0) as usize);
            });
        }
        let focus = gtk::EventControllerFocus::new();
        {
            let c = me.clone();
            focus.connect_leave(move |_| c.hide());
        }
        entry.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let c = me.clone();
            keys.connect_key_pressed(move |_, key, _, state| c.on_key(key, state));
        }
        entry.add_controller(keys);
        me
    }

    pub fn set_fields(&self, fields: Rc<Vec<Field>>) {
        *self.fields.borrow_mut() = fields;
        if self.popover.is_visible() {
            self.refresh();
        }
    }

    /// Unparent the popover before the entry goes away.
    pub fn detach(&self) {
        self.hide();
        self.popover.unparent();
    }

    fn entry_focused(&self) -> bool {
        self.entry
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|w| w == self.entry || w.is_ancestor(&self.entry))
    }

    fn cursor_byte(&self) -> usize {
        let text = self.entry.text();
        let pos = self.entry.position().max(0) as usize;
        text.char_indices()
            .nth(pos)
            .map(|(i, _)| i)
            .unwrap_or(text.len())
    }

    fn refresh(&self) {
        if self.dismissed.get() {
            return;
        }
        let text = self.entry.text().to_string();
        let cursor = self.cursor_byte();
        let fields = self.fields.borrow().clone();
        let items = query_complete::complete(&text, cursor, &fields, self.kind);
        if items.is_empty() {
            self.hide();
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for c in items.iter().take(MAX_ROWS) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let label = gtk::Label::builder()
                .label(&c.label)
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["viti-mono"])
                .build();
            let detail = gtk::Label::builder()
                .label(&c.detail)
                .xalign(1.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build();
            row.append(&label);
            row.append(&detail);
            let lbr = gtk::ListBoxRow::builder()
                .child(&row)
                .focusable(false)
                .can_focus(false)
                .build();
            self.list.append(&lbr);
        }
        *self.items.borrow_mut() = items;
        self.navigated.set(false);
        self.select(0);
        self.point_at_caret(cursor);
        if !self.popover.is_visible() {
            self.entry.add_css_class(COMPLETING_CLASS);
            self.popover.popup();
        }
    }

    /// Drop the popover under the caret rather than under the whole entry.
    fn point_at_caret(&self, cursor: usize) {
        // GTK 4 keeps the entry's PangoLayout private: measure the text
        // before the caret with the same font and add the inner text
        // widget's offset (icons, padding).
        let text = self.entry.text();
        let head = &text[..cursor.min(text.len())];
        let inner = self
            .entry
            .first_child()
            .and_then(|t| t.compute_bounds(&self.entry))
            .map(|b| b.x() as i32)
            .unwrap_or(0);
        let width = self.entry.create_pango_layout(Some(head)).pixel_size().0;
        let x = inner + width;
        let x = x.clamp(0, self.entry.width().max(1) - 1);
        let rect = gtk::gdk::Rectangle::new(x, 0, 1, self.entry.height());
        self.popover.set_pointing_to(Some(&rect));
    }

    fn select(&self, i: usize) {
        let n = self.items.borrow().len().min(MAX_ROWS);
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        self.selected.set(i);
        if let Some(row) = self.list.row_at_index(i as i32) {
            self.list.select_row(Some(&row));
            // Keep the highlighted row in view.
            if let Some(adj) = self
                .list
                .parent()
                .and_then(|p| p.parent())
                .and_downcast::<gtk::ScrolledWindow>()
                .map(|s| s.vadjustment())
            {
                let y = row
                    .compute_bounds(&self.list)
                    .map(|b| b.y() as f64)
                    .unwrap_or(0.0);
                let h = row.height() as f64;
                let top = adj.value();
                let bottom = top + adj.page_size();
                if y < top {
                    adj.set_value(y);
                } else if y + h > bottom {
                    adj.set_value(y + h - adj.page_size());
                }
            }
        }
    }

    fn step(&self, dir: i32) {
        let n = self.items.borrow().len().min(MAX_ROWS) as i32;
        if n == 0 {
            return;
        }
        let next = (self.selected.get() as i32 + dir).rem_euclid(n) as usize;
        self.navigated.set(true);
        self.select(next);
    }

    fn accept(&self, i: usize) {
        let item = self.items.borrow().get(i).cloned();
        let Some(c) = item else { return };
        let text = self.entry.text().to_string();
        let (new_text, caret) = query_complete::apply(&text, &c);
        let caret_chars = new_text[..caret].chars().count() as i32;
        self.suppress.set(true);
        self.entry.set_text(&new_text);
        self.suppress.set(false);
        self.entry.grab_focus_without_selecting();
        self.entry.set_position(caret_chars);
        self.hide();
    }

    fn hide(&self) {
        if self.popover.is_visible() {
            self.popover.popdown();
        }
        self.entry.remove_css_class(COMPLETING_CLASS);
    }

    fn on_key(&self, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> gtk::glib::Propagation {
        use gtk::gdk::Key;
        use gtk::glib::Propagation;
        let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if !self.popover.is_visible() {
            if key == Key::space && ctrl {
                self.dismissed.set(false);
                self.refresh();
                return Propagation::Stop;
            }
            return Propagation::Proceed;
        }
        match key {
            Key::Escape => {
                self.dismissed.set(true);
                self.hide();
                Propagation::Stop
            }
            Key::Tab => {
                self.accept(self.selected.get());
                Propagation::Stop
            }
            Key::ISO_Left_Tab => {
                self.step(-1);
                Propagation::Stop
            }
            Key::Down => {
                self.step(1);
                Propagation::Stop
            }
            Key::Up => {
                self.step(-1);
                Propagation::Stop
            }
            Key::n if ctrl => {
                self.step(1);
                Propagation::Stop
            }
            Key::p if ctrl => {
                self.step(-1);
                Propagation::Stop
            }
            Key::Return | Key::KP_Enter if self.navigated.get() => {
                self.accept(self.selected.get());
                Propagation::Stop
            }
            Key::Return | Key::KP_Enter => {
                self.hide();
                Propagation::Proceed
            }
            _ => Propagation::Proceed,
        }
    }
}
