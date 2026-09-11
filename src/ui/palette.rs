//! The `:` command line at the bottom of the window: an entry with inline
//! completion hints (Tab cycles), Enter runs, Escape closes.
use crate::commands::{self, Completion, CompletionCtx};
use adw::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[allow(clippy::type_complexity)]
pub struct Palette {
    pub root: gtk::Box,
    pub entry: gtk::Entry,
    hint: gtk::Label,
    completions: RefCell<Vec<Completion>>,
    index: Cell<Option<usize>>,
    on_run: RefCell<Option<Rc<dyn Fn(String)>>>,
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
    ctx: RefCell<Option<Rc<dyn CompletionCtx>>>,
}

impl Palette {
    pub fn new() -> Rc<Self> {
        let prompt = gtk::Label::builder()
            .label(":")
            .css_classes(["viti-cmdline"])
            .build();
        let entry = gtk::Entry::builder()
            .hexpand(true)
            .css_classes(["viti-cmdline", "flat"])
            .has_frame(false)
            .build();
        let hint = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["viti-cmdline-hint"])
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.append(&prompt);
        row.append(&entry);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolbar");
        root.append(&row);
        root.append(&hint);
        root.set_margin_start(6);
        root.set_margin_end(6);

        let p = Rc::new(Self {
            root,
            entry,
            hint,
            completions: RefCell::new(Vec::new()),
            index: Cell::new(None),
            on_run: RefCell::new(None),
            on_close: RefCell::new(None),
            ctx: RefCell::new(None),
        });
        {
            let me = p.clone();
            p.entry.connect_changed(move |e| {
                if me.index.get().is_none() {
                    me.refresh_hint(&e.text());
                }
            });
        }
        {
            let me = p.clone();
            p.entry.connect_activate(move |e| {
                let line = e.text().to_string();
                let cb = me.on_run.borrow().clone();
                me.close();
                if let Some(cb) = cb {
                    cb(line);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let me = p.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                use gtk::gdk::Key;
                match key {
                    Key::Escape => {
                        me.close();
                        gtk::glib::Propagation::Stop
                    }
                    Key::Tab | Key::ISO_Left_Tab => {
                        me.cycle(
                            if key == Key::ISO_Left_Tab
                                || state.contains(gtk::gdk::ModifierType::SHIFT_MASK)
                            {
                                -1
                            } else {
                                1
                            },
                        );
                        gtk::glib::Propagation::Stop
                    }
                    Key::u if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                        me.entry.set_text("");
                        gtk::glib::Propagation::Stop
                    }
                    _ => gtk::glib::Propagation::Proceed,
                }
            });
        }
        p.entry.add_controller(keys);
        p
    }

    pub fn set_on_run(&self, f: impl Fn(String) + 'static) {
        *self.on_run.borrow_mut() = Some(Rc::new(f));
    }
    pub fn set_on_close(&self, f: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Rc::new(f));
    }
    pub fn set_ctx(&self, ctx: Rc<dyn CompletionCtx>) {
        *self.ctx.borrow_mut() = Some(ctx);
    }

    pub fn open(&self, initial: &str) {
        self.index.set(None);
        self.entry.set_text(initial);
        self.entry.set_position(-1);
        self.refresh_hint(initial);
        self.entry.grab_focus();
    }

    pub fn close(&self) {
        let cb = self.on_close.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn refresh_hint(&self, line: &str) {
        let ctx = self.ctx.borrow().clone();
        let comps = match ctx {
            Some(c) => commands::complete(line, &*c),
            None => Vec::new(),
        };
        let text = if comps.is_empty() {
            match commands::parse(line) {
                Ok(inv) => commands::lookup(inv.name)
                    .map(|c| c.help.to_string())
                    .unwrap_or_default(),
                Err(_) if line.trim().is_empty() => {
                    "db · coll · find · export · set · shell · help · quit".into()
                }
                Err(e) => e,
            }
        } else if comps.len() == 1 && comps[0].line.trim() == line.trim() {
            comps[0].hint.clone()
        } else {
            comps
                .iter()
                .take(12)
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>()
                .join("   ")
        };
        self.hint.set_text(&text);
        *self.completions.borrow_mut() = comps;
        self.index.set(None);
    }

    fn cycle(&self, dir: i32) {
        let comps = self.completions.borrow();
        if comps.is_empty() {
            return;
        }
        let next = match self.index.get() {
            None => {
                if dir > 0 {
                    0
                } else {
                    comps.len() - 1
                }
            }
            Some(i) => (i as i32 + dir).rem_euclid(comps.len() as i32) as usize,
        };
        self.index.set(Some(next));
        let c = &comps[next];
        self.entry.set_text(&c.line);
        self.entry.set_position(-1);
        let labels: Vec<String> = comps
            .iter()
            .enumerate()
            .take(12)
            .map(|(i, c)| {
                if i == next {
                    format!("[{}]", c.label)
                } else {
                    c.label.clone()
                }
            })
            .collect();
        self.hint.set_text(&labels.join("   "));
    }
}
