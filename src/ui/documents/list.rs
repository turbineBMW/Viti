//! The List view: one card per document, fields as key / value / type rows,
//! nested documents and arrays behind expanders built on demand.
use super::DocumentsPane;
use crate::mongo::ejson;
use adw::prelude::*;
use bson::Bson;
use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use std::rc::{Rc, Weak};

/// Fields shown per card before the "… more" line.
const PREVIEW_ROWS: usize = 5;

pub fn setup(pane: &Rc<DocumentsPane>) {
    let factory = gtk::SignalListItemFactory::new();
    let weak: Weak<DocumentsPane> = Rc::downgrade(pane);
    let weak_setup = weak.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("viti-doc-card");
        // A click on the card selects it and opens the document dialog.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let item2 = item.downgrade();
        let weak = weak_setup.clone();
        click.connect_released(move |_, n, _, _| {
            if n != 1 {
                return;
            }
            let Some(pane) = weak.upgrade() else { return };
            let Some(item2) = item2.upgrade() else { return };
            let pos = item2.position();
            if pos != gtk::INVALID_LIST_POSITION {
                pane.selection.set_selected(pos);
                pane.peek(false);
            }
        });
        card.add_controller(click);
        item.set_child(Some(&card));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let card = item.child().and_downcast::<gtk::Box>().unwrap();
        while let Some(c) = card.first_child() {
            card.remove(&c);
        }
        let Some(pane) = weak.upgrade() else { return };
        let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
            return;
        };
        let idx = *obj.borrow::<usize>();
        let docs = pane.docs.borrow();
        let Some(doc) = docs.get(idx) else { return };
        if pane.marked.borrow().contains(&idx) {
            card.add_css_class("viti-marked");
        } else {
            card.remove_css_class("viti-marked");
        }
        let expanded = pane.expanded_all.get();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let num = gtk::Label::builder()
            .label(format!(
                "{}",
                pane.page.get() * pane.page_size.get() + idx as u64 + 1
            ))
            .css_classes(["viti-count"])
            .build();
        header.append(&num);
        if let Some(id) = doc.get("_id") {
            let l = gtk::Label::builder()
                .label(ejson::id_display(id))
                .css_classes(["viti-value", ejson::type_class(id)])
                .xalign(0.0)
                .selectable(false)
                .build();
            header.append(&l);
        }
        let count = gtk::Label::builder()
            .label(format!("{} fields", doc.len()))
            .css_classes(["viti-count"])
            .hexpand(true)
            .xalign(1.0)
            .build();
        header.append(&count);
        card.append(&header);
        // Only the first few fields; the rest live behind the open dialog.
        let mut shown = 0usize;
        for (k, v) in doc
            .iter()
            .filter(|(k, _)| k.as_str() != "_id")
            .take(PREVIEW_ROWS)
        {
            let (preview, truncated) = super::preview::value(v);
            card.append(&field_row(k, &preview, expanded, 0, Some(v)));
            shown += 1;
            if truncated {
                let notice = gtk::Label::builder()
                    .label("… Preview shortened · Open the document for the full value")
                    .css_classes(["viti-doc-more"])
                    .xalign(0.0)
                    .build();
                card.append(&notice);
            }
        }
        let hidden = doc.len() - usize::from(doc.contains_key("_id")) - shown;
        if hidden > 0 {
            let more = gtk::Label::builder()
                .label(format!("… {hidden} more fields"))
                .css_classes(["viti-doc-more"])
                .xalign(0.0)
                .build();
            card.append(&more);
        }
    });
    pane.list_view.set_factory(Some(&factory));
    pane.list_view.set_single_click_activate(false);
    pane.list_view.add_css_class("navigation-sidebar");
}

/// One `key: value  Type` line; nested values get an expander whose body is
/// built the first time it opens.
fn field_row(
    key: &str,
    value: &Bson,
    expanded: bool,
    depth: usize,
    original: Option<&Bson>,
) -> gtk::Widget {
    let display = original.unwrap_or(value);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start((depth * 16) as i32);
    let key_label = gtk::Label::builder()
        .label(ejson::truncate(key, 120))
        .css_classes(["viti-key"])
        .xalign(0.0)
        .build();
    let type_badge = gtk::Label::builder()
        .label(ejson::type_name(display))
        .css_classes(["viti-type"])
        .valign(gtk::Align::Center)
        .build();
    match value {
        Bson::Document(_) | Bson::Array(_) => {
            let is_empty = match value {
                Bson::Document(d) => d.is_empty(),
                Bson::Array(a) => a.is_empty(),
                _ => false,
            };
            let summary = gtk::Label::builder()
                .label(ejson::summary(display, 60))
                .css_classes(["viti-value", "viti-dim"])
                .xalign(0.0)
                .hexpand(true)
                .build();
            row.append(&key_label);
            row.append(&summary);
            row.append(&type_badge);
            if is_empty {
                return row.upcast();
            }
            let expander = gtk::Expander::builder()
                .label_widget(&row)
                .expanded(expanded)
                .build();
            let value = value.clone();
            let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
            body.set_margin_start(12);
            let built = std::cell::Cell::new(false);
            let fill = move |body: &gtk::Box| {
                if built.get() {
                    return;
                }
                built.set(true);
                match &value {
                    Bson::Document(d) => {
                        for (k, v) in d.iter() {
                            body.append(&field_row(k, v, false, 0, None));
                        }
                    }
                    Bson::Array(a) => {
                        for (i, v) in a.iter().enumerate() {
                            body.append(&field_row(&i.to_string(), v, false, 0, None));
                        }
                    }
                    _ => {}
                }
            };
            if expanded {
                fill(&body);
            } else {
                let body2 = body.clone();
                expander.connect_expanded_notify(move |e| {
                    if e.is_expanded() {
                        fill(&body2);
                    }
                });
            }
            expander.set_child(Some(&body));
            expander.upcast()
        }
        _ => {
            let value_label = gtk::Label::builder()
                .label(ejson::summary(display, 200))
                .css_classes(["viti-value", ejson::type_class(display)])
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .selectable(false)
                .build();
            value_label.set_tooltip_text(Some(&ejson::summary(display, 2000)));
            row.append(&key_label);
            row.append(&value_label);
            row.append(&type_badge);
            row.upcast()
        }
    }
}
