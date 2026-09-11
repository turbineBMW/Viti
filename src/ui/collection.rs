//! One tab: a collection's Documents / Aggregations / Schema / Explain /
//! Indexes / Validation pages in a ViewStack. The page is picked from the
//! dropdown in the window header (see `window.rs`), which the app keeps in
//! sync with the selected tab.
use crate::app::App;
use crate::mongo::ConnectionId;
use crate::mongo::ops::Namespace;
use crate::ui::aggregation::AggregationPane;
use crate::ui::documents::DocumentsPane;
use crate::ui::explain::ExplainPane;
use crate::ui::indexes::IndexesPane;
use crate::ui::schema::SchemaPane;
use crate::ui::validation::ValidationPane;
use adw::prelude::*;
use gtk4 as gtk;
use std::rc::Rc;

/// (stack child name, title, icon) — order is the dropdown order.
pub const PAGES: [(&str, &str, &str); 6] = [
    ("documents", "Documents", "view-list-symbolic"),
    ("aggregations", "Aggregations", "system-run-symbolic"),
    ("schema", "Schema", "view-grid-symbolic"),
    ("explain", "Explain Plan", "dialog-information-symbolic"),
    ("indexes", "Indexes", "view-sort-descending-symbolic"),
    ("validation", "Validation", "emblem-ok-symbolic"),
];

pub fn page_index(name: &str) -> Option<u32> {
    PAGES.iter().position(|p| p.0 == name).map(|i| i as u32)
}

pub struct CollectionTab {
    pub conn: ConnectionId,
    pub ns: Namespace,
    pub root: gtk::Box,
    pub page: adw::TabPage,
    pub stack: adw::ViewStack,
    pub docs: Rc<DocumentsPane>,
    pub agg: Rc<AggregationPane>,
    pub schema: Rc<SchemaPane>,
    pub explain: Rc<ExplainPane>,
    pub indexes: Rc<IndexesPane>,
    pub validation: Rc<ValidationPane>,
}

impl CollectionTab {
    pub fn new(app: &Rc<App>, conn: ConnectionId, ns: Namespace) -> Rc<Self> {
        let docs = DocumentsPane::new(app, conn, ns.clone());
        let agg = AggregationPane::new(app, conn, ns.clone());
        let explain = ExplainPane::new(app, conn, ns.clone());
        explain.set_sources(&docs, &agg);
        let indexes = IndexesPane::new(app, conn, ns.clone());
        let schema = SchemaPane::new(app, conn, ns.clone());
        schema.set_docs(&docs);
        let validation = ValidationPane::new(app, conn, ns.clone());
        validation.set_schema(&schema);
        let stack = adw::ViewStack::new();
        for (name, title, icon) in PAGES {
            let child: gtk::Widget = match name {
                "documents" => docs.root.clone().upcast(),
                "aggregations" => agg.root.clone().upcast(),
                "schema" => schema.root.clone().upcast(),
                "explain" => explain.root.clone().upcast(),
                "indexes" => indexes.root.clone().upcast(),
                _ => validation.root.clone().upcast(),
            };
            stack
                .add_titled(&child, Some(name), title)
                .set_icon_name(Some(icon));
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.set_vexpand(true);
        root.append(&stack);
        {
            // Pages load lazily the first time they are shown, and take focus.
            let indexes = indexes.clone();
            let docs = docs.clone();
            let agg = agg.clone();
            let explain = explain.clone();
            let schema = schema.clone();
            let validation = validation.clone();
            stack.connect_visible_child_name_notify(move |s| {
                match s.visible_child_name().as_deref() {
                    Some("indexes") => {
                        indexes.ensure_loaded();
                        indexes.root.grab_focus();
                    }
                    Some("documents") => docs.focus_views(),
                    Some("aggregations") => agg.focus(),
                    Some("explain") => {
                        explain.root.grab_focus();
                    }
                    Some("schema") => {
                        schema.ensure_loaded();
                        schema.root.grab_focus();
                    }
                    Some("validation") => {
                        validation.ensure_loaded();
                        validation.root.grab_focus();
                    }
                    _ => {}
                }
            });
        }

        let page = app.tab_view.append(&root);
        page.set_tooltip(&ns.to_string());
        Rc::new(Self {
            conn,
            ns,
            root,
            page,
            stack,
            docs,
            agg,
            schema,
            explain,
            indexes,
            validation,
        })
    }

    pub fn show_page(&self, name: &str) {
        self.stack.set_visible_child_name(name);
    }
}
