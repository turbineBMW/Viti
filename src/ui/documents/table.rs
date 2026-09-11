//! The Table view: a ColumnView whose columns are the union of the page's
//! top-level keys. Columns are rebuilt on every page load.
use super::DocumentsPane;
use crate::mongo::ejson;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib::BoxedAnyObject;
use std::rc::{Rc, Weak};

pub fn setup(pane: &Rc<DocumentsPane>) {
    pane.table_view.set_reorderable(true);
    pane.table_view.set_show_row_separators(true);
    pane.table_view.set_show_column_separators(true);
    pane.table_view.add_css_class("viti-table");
    pane.table_view.add_css_class("data-table");
}

pub fn rebuild_columns(pane: &DocumentsPane) {
    if pane.view.get() != crate::config::DocView::Table {
        return;
    }
    let view = &pane.table_view;
    let existing = view.columns();
    while existing.n_items() > 0 {
        if let Some(c) = existing.item(0).and_downcast::<gtk::ColumnViewColumn>() {
            view.remove_column(&c);
        }
    }
    for key in pane.visible_columns() {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(48)
                .css_classes(["viti-value"])
                .build();
            item.set_child(Some(&label));
        });
        let k = key.clone();
        let docs_weak: Weak<DocumentsPane> = pane.me.borrow().clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            let Some(pane) = docs_weak.upgrade() else {
                return;
            };
            let Some(obj) = item.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            let idx = *obj.borrow::<usize>();
            let docs = pane.docs.borrow();
            for c in [
                "string", "number", "boolean", "objectid", "date", "null", "other",
            ] {
                label.remove_css_class(c);
            }
            match docs.get(idx).and_then(|d| d.get(&k)) {
                Some(v) => {
                    label.set_text(&ejson::summary(v, 120));
                    label.add_css_class(ejson::type_class(v));
                    label.set_tooltip_text(Some(&format!(
                        "{}: {}",
                        ejson::type_name(v),
                        ejson::summary(v, 1000)
                    )));
                }
                None => {
                    label.set_text("");
                    label.set_tooltip_text(None);
                }
            }
            if pane.marked.borrow().contains(&idx) {
                label.add_css_class("viti-marked");
            } else {
                label.remove_css_class("viti-marked");
            }
        });
        let column = gtk::ColumnViewColumn::new(Some(&ejson::truncate(&key, 120)), Some(factory));
        column.set_resizable(true);
        column.set_expand(key != "_id");
        view.append_column(&column);
    }
    highlight_column(pane, pane.column_cursor.get());
}

/// Mark the current column's header so `S`/`H` are predictable.
pub fn highlight_column(pane: &DocumentsPane, idx: usize) {
    let cols = pane.table_view.columns();
    for i in 0..cols.n_items() {
        if let Some(c) = cols.item(i).and_downcast::<gtk::ColumnViewColumn>() {
            let title = c.title().map(|t| t.to_string()).unwrap_or_default();
            let bare = title.trim_start_matches("▸ ").to_string();
            c.set_title(Some(&if i as usize == idx {
                format!("▸ {bare}")
            } else {
                bare
            }));
        }
    }
}
