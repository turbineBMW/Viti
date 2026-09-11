//! Which pane owns the vi keys. Every pane root registers its `Scope`; the
//! focused scope is derived from the window's focus widget by walking up its
//! ancestors, and the pane root gets `.viti-focused` so the user can see it.
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Global,
    Sidebar,
    Documents,
    QueryBar,
    Aggregation,
    Schema,
    Explain,
    Indexes,
    Validation,
    Performance,
    Shell,
    Editor,
    Palette,
    Dialog,
}

impl Scope {
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Everywhere",
            Scope::Sidebar => "Sidebar",
            Scope::Documents => "Documents",
            Scope::QueryBar => "Query bar",
            Scope::Aggregation => "Aggregation",
            Scope::Schema => "Schema",
            Scope::Explain => "Explain",
            Scope::Indexes => "Indexes",
            Scope::Validation => "Validation",
            Scope::Performance => "Performance",
            Scope::Shell => "Shell",
            Scope::Editor => "Editor",
            Scope::Palette => "Command line",
            Scope::Dialog => "Dialog",
        }
    }
    pub fn from_id_prefix(prefix: &str) -> Scope {
        match prefix {
            "sidebar" => Scope::Sidebar,
            "docs" => Scope::Documents,
            "query" => Scope::QueryBar,
            "agg" => Scope::Aggregation,
            "schema" => Scope::Schema,
            "explain" => Scope::Explain,
            "idx" => Scope::Indexes,
            "validation" => Scope::Validation,
            "perf" => Scope::Performance,
            "shell" => Scope::Shell,
            "editor" => Scope::Editor,
            "palette" => Scope::Palette,
            _ => Scope::Global,
        }
    }
    /// Panes whose keys must never be stolen: the terminal owns everything.
    pub fn is_terminal(self) -> bool {
        matches!(self, Scope::Shell | Scope::Editor)
    }
}

#[derive(Default)]
pub struct FocusTracker {
    panes: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Scope)>>,
    current: Cell<Option<Scope>>,
}

impl FocusTracker {
    /// Make `root` a vi pane: focusable, styled when current, and a key target.
    pub fn register(&self, root: &impl IsA<gtk::Widget>, scope: Scope) {
        let w = root.as_ref();
        w.set_focusable(true);
        w.set_can_focus(true);
        self.panes.borrow_mut().push((w.downgrade(), scope));
    }

    pub fn current(&self) -> Scope {
        self.current.get().unwrap_or(Scope::Global)
    }

    /// Recompute from the window's focus widget. Returns the scope.
    pub fn update(&self, focus: Option<gtk::Widget>) -> Scope {
        let mut found: Option<(gtk::Widget, Scope)> = None;
        if let Some(mut w) = focus {
            'walk: loop {
                for (weak, scope) in self.panes.borrow().iter() {
                    if let Some(p) = weak.upgrade()
                        && p == w
                    {
                        found = Some((p, *scope));
                        break 'walk;
                    }
                }
                match w.parent() {
                    Some(p) => w = p,
                    None => break,
                }
            }
        }
        let scope = found.as_ref().map(|(_, s)| *s).unwrap_or(Scope::Global);
        for (weak, _) in self.panes.borrow().iter() {
            if let Some(p) = weak.upgrade() {
                let is = found.as_ref().is_some_and(|(f, _)| *f == p);
                if is {
                    p.add_css_class("viti-focused");
                } else {
                    p.remove_css_class("viti-focused");
                }
            }
        }
        self.current.set(Some(scope));
        scope
    }

    /// The registered root for a scope, if it is mapped.
    pub fn root_of(&self, scope: Scope) -> Option<gtk::Widget> {
        self.panes
            .borrow()
            .iter()
            .filter(|(_, s)| *s == scope)
            .filter_map(|(w, _)| w.upgrade())
            .find(|w| w.is_mapped())
    }

    /// Visible panes in registration order, for Tab cycling.
    pub fn visible(&self) -> Vec<(gtk::Widget, Scope)> {
        self.panes
            .borrow()
            .iter()
            .filter_map(|(w, s)| w.upgrade().map(|w| (w, *s)))
            .filter(|(w, _)| w.is_mapped() && w.is_sensitive())
            .collect()
    }

    /// Move focus to the next/previous visible pane.
    pub fn cycle(&self, dir: i32) -> Option<Scope> {
        let panes = self.visible();
        if panes.is_empty() {
            return None;
        }
        let cur = self.current();
        let idx = panes.iter().position(|(_, s)| *s == cur);
        let next = match idx {
            Some(i) => (i as i32 + dir).rem_euclid(panes.len() as i32) as usize,
            None => 0,
        };
        let (w, s) = &panes[next];
        w.grab_focus();
        Some(*s)
    }

    pub fn focus(&self, scope: Scope) -> bool {
        match self.root_of(scope) {
            Some(w) => {
                w.grab_focus();
                true
            }
            None => false,
        }
    }
}
