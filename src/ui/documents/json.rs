//! The JSON view: one read-only source view per document.
use super::DocumentsPane;
use crate::mongo::ejson::{self, Mode};
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use std::rc::{Rc, Weak};

pub fn setup(pane: &Rc<DocumentsPane>) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("viti-doc-json");
        let view = crate::ui::json_view("", false);
        view.set_can_focus(false);
        view.set_focusable(false);
        frame.append(&view);
        item.set_child(Some(&frame));
    });
    let weak: Weak<DocumentsPane> = Rc::downgrade(pane);
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let frame = item.child().and_downcast::<gtk::Box>().unwrap();
        let view = frame
            .first_child()
            .and_downcast::<sourceview5::View>()
            .unwrap();
        let Some(pane) = weak.upgrade() else { return };
        let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
            return;
        };
        let idx = *obj.borrow::<usize>();
        let docs = pane.docs.borrow();
        let Some(doc) = docs.get(idx) else { return };
        if pane.marked.borrow().contains(&idx) {
            frame.add_css_class("viti-marked");
        } else {
            frame.remove_css_class("viti-marked");
        }
        let text = if pane.expanded_all.get() {
            ejson::pretty(doc, Mode::Relaxed)
        } else {
            // Collapsed: nested documents/arrays on one line each.
            collapsed_text(doc)
        };
        view.buffer().set_text(&text);
    });
    pane.json_view.set_model(Some(&pane.selection));
    pane.json_view.set_factory(Some(&factory));
    pane.json_view.set_single_click_activate(false);
    pane.json_view.add_css_class("navigation-sidebar");
}

/// Top-level fields one per line; nested values compact.
pub fn collapsed_text(doc: &bson::Document) -> String {
    let mut out = String::from("{\n");
    let n = doc.len();
    for (i, (k, v)) in doc.iter().enumerate() {
        let value = serde_json::to_string(&v.clone().into_relaxed_extjson()).unwrap_or_default();
        out.push_str(&format!(
            "  {}: {}{}\n",
            serde_json::to_string(k).unwrap_or_default(),
            value,
            if i + 1 < n { "," } else { "" }
        ));
    }
    out.push('}');
    out
}
