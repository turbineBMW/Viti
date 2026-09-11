//! Direct page navigation. The list model creates page numbers on demand, so
//! opening the picker does not allocate one object per page in a large collection.
use super::DocumentsPane;
use adw::prelude::*;
use glib::subclass::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;
use std::cell::Cell;
use std::rc::Rc;

mod imp {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct PageModel(pub Cell<u32>);

    #[glib::object_subclass]
    impl ObjectSubclass for PageModel {
        const NAME: &'static str = "VitiPageModel";
        type Type = super::PageModel;
        type Interfaces = (gio::ListModel,);
    }
    impl ObjectImpl for PageModel {}
    impl ListModelImpl for PageModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }
        fn n_items(&self) -> u32 {
            self.0.get()
        }
        fn item(&self, position: u32) -> Option<glib::Object> {
            (position < self.0.get())
                .then(|| glib::BoxedAnyObject::new(u64::from(position) + 1).upcast())
        }
    }
}

glib::wrapper! {
    pub struct PageModel(ObjectSubclass<imp::PageModel>) @implements gio::ListModel;
}

fn page_count(total: u64, size: u64) -> u64 {
    total.div_ceil(size.max(1))
}

fn document_range(page: u64, size: u64, total: u64) -> (u64, u64) {
    let start = page.saturating_sub(1).saturating_mul(size);
    (
        start.saturating_add(1).min(total),
        start.saturating_add(size).min(total),
    )
}

pub(super) struct PagePicker {
    dialog: adw::Dialog,
    first: gtk::Button,
    last: gtk::Button,
    summary: gtk::Label,
    list: gtk::ListView,
    model: PageModel,
    selection: gtk::SingleSelection,
    // Also used by recycled list rows to show the current document ranges.
    range: Rc<Cell<(u64, u64, u64)>>,
}

impl PagePicker {
    pub(super) fn update(&self, pane: &DocumentsPane) {
        let total = pane.total.get();
        let size = pane.page_size.get();
        let count = page_count(total.unwrap_or(0), size);
        let current = pane.loaded_page.get() + 1;
        self.first
            .set_sensitive(!pane.busy.get() && total != Some(0));
        self.last.set_sensitive(!pane.busy.get() && count > 0);
        self.list.set_sensitive(!pane.busy.get());
        self.summary.set_text(&match total {
            Some(0) => "No documents match this query.".into(),
            Some(_) => format!(
                "{} pages · {} documents per page",
                crate::ui::thousands(count),
                size
            ),
            None if pane.count_inflight.borrow().is_some() => "Counting documents…".into(),
            None => "Page count unavailable. Refresh the collection to try again.".into(),
        });
        let range = (size, total.unwrap_or(0), current);
        if self.range.replace(range) != range {
            let old = self.model.n_items();
            let n = count.min(u64::from(u32::MAX)) as u32;
            self.model.imp().0.set(n);
            self.model.items_changed(0, old, n);
            if current <= u64::from(n) {
                self.selection.set_selected((current - 1) as u32);
            }
        }
    }
}

impl DocumentsPane {
    pub fn show_page_picker(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let Some(app) = self.app() else { return };
        if let Some(picker) = self.page_picker.borrow().as_ref() {
            picker.dialog.present(Some(&app.window));
            return;
        }
        let dialog = adw::Dialog::builder()
            .title("Go to page")
            .content_width(400)
            .content_height(520)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_bottom(18)
            .build();
        let shortcuts = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        shortcuts.set_homogeneous(true);
        let first = gtk::Button::with_label("First Page");
        let last = gtk::Button::with_label("Last Page");
        shortcuts.append(&first);
        shortcuts.append(&last);
        body.append(&shortcuts);
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        body.append(&summary);

        let model: PageModel = glib::Object::new();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        let range = Rc::new(Cell::new((0, 0, 0)));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = adw::ActionRow::new();
            row.set_title_selectable(false);
            item.set_child(Some(&row));
        });
        {
            let range = range.clone();
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let row = item.child().and_downcast::<adw::ActionRow>().unwrap();
                let obj = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
                let page = *obj.borrow::<u64>();
                let (size, total, current) = range.get();
                row.set_title(&format!(
                    "Page {}{}",
                    crate::ui::thousands(page),
                    if page == current {
                        " · Current page"
                    } else {
                        ""
                    }
                ));
                let (start, end) = document_range(page, size, total);
                row.set_subtitle(&format!(
                    "Documents {}–{}",
                    crate::ui::thousands(start),
                    crate::ui::thousands(end)
                ));
            });
        }
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_single_click_activate(true);
        list.add_css_class("boxed-list");
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        body.append(&scroll);
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));
        {
            let weak = Rc::downgrade(self);
            let dialog = dialog.downgrade();
            first.connect_clicked(move |_| {
                if let (Some(pane), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) {
                    dialog.close();
                    pane.goto_page(1);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let dialog = dialog.downgrade();
            last.connect_clicked(move |_| {
                if let (Some(pane), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) {
                    if let Some(total) = pane.total.get() {
                        dialog.close();
                        pane.goto_page(page_count(total, pane.page_size.get()).max(1));
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let dialog = dialog.downgrade();
            list.connect_activate(move |_, position| {
                if let (Some(pane), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) {
                    dialog.close();
                    pane.goto_page(u64::from(position) + 1);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            dialog.connect_closed(move |_| {
                if let Some(pane) = weak.upgrade() {
                    pane.page_picker.borrow_mut().take();
                    pane.focus_views();
                }
            });
        }
        let picker = PagePicker {
            dialog: dialog.clone(),
            first,
            last,
            summary,
            list,
            model,
            selection,
            range,
        };
        picker.update(self);
        *self.page_picker.borrow_mut() = Some(picker);
        dialog.present(Some(&app.window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with xvfb-run and an isolated XDG_CONFIG_HOME.
    #[test]
    #[ignore = "requires GTK display and isolated config"]
    fn gtk_page_picker_navigation() {
        adw::init().unwrap();
        let application = adw::Application::builder()
            .application_id("dev.turbinebmw.Viti.PagePickerTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let app = crate::app::App::build(&application);
        let pane = DocumentsPane::new(
            &app,
            uuid::Uuid::new_v4(),
            crate::mongo::ops::Namespace::new("picker_test", "documents"),
        );
        app.window.set_content(Some(&pane.root));
        app.window.present();
        glib::MainContext::default().block_on(async {
            pane.goto_btn.emit_clicked();
            let (last, list, model) = {
                let picker = pane.page_picker.borrow();
                let picker = picker.as_ref().unwrap();
                assert!(!picker.last.is_sensitive());
                assert_eq!(picker.model.n_items(), 0);
                (
                    picker.last.clone(),
                    picker.list.clone(),
                    picker.model.clone(),
                )
            };
            // An arriving count updates the already-open modal.
            pane.total.set(Some(9657));
            pane.page_size.set(100);
            pane.update_status();
            assert_eq!(model.n_items(), 97);
            assert!(last.is_sensitive());
            glib::timeout_future(std::time::Duration::from_millis(100)).await;
            list.emit_by_name::<()>("activate", &[&36u32]);
            assert_eq!(pane.page.get(), 36);
            glib::timeout_future(std::time::Duration::from_millis(300)).await;
            assert!(pane.page_picker.borrow().is_none());

            pane.show_page_picker();
            let last = pane.page_picker.borrow().as_ref().unwrap().last.clone();
            last.emit_clicked();
            assert_eq!(pane.page.get(), 96);
            glib::timeout_future(std::time::Duration::from_millis(300)).await;

            pane.show_page_picker();
            let first = pane.page_picker.borrow().as_ref().unwrap().first.clone();
            first.emit_clicked();
            assert_eq!(pane.page.get(), 0);
            glib::timeout_future(std::time::Duration::from_millis(300)).await;

            pane.total.set(Some(0));
            pane.show_page_picker();
            {
                let picker = pane.page_picker.borrow();
                let picker = picker.as_ref().unwrap();
                assert_eq!(picker.model.n_items(), 0);
                assert!(!picker.first.is_sensitive());
                assert!(!picker.last.is_sensitive());
            }
            pane.set_busy(true);
            pane.goto_page(9);
            assert_eq!(pane.page.get(), 0);
            assert!(!pane.goto_btn.is_sensitive());
        });
        app.window.destroy();
    }

    #[test]
    fn page_boundaries_and_partial_last_page() {
        for (total, size, count, last) in [
            (0, 25, 0, (0, 0)),
            (1, 25, 1, (1, 1)),
            (50, 25, 2, (26, 50)),
            (51, 25, 3, (51, 51)),
            (9657, 100, 97, (9601, 9657)),
            (9657, 25, 387, (9651, 9657)),
        ] {
            assert_eq!(page_count(total, size), count);
            assert_eq!(document_range(count, size, total), last);
        }
        assert_eq!(page_count(u64::MAX, 100), u64::MAX / 100 + 1);
    }

    #[test]
    fn page_model_handles_large_collections_without_materializing_rows() {
        let model: PageModel = glib::Object::new();
        model.imp().0.set(100_000_000);
        assert_eq!(model.n_items(), 100_000_000);
        let item = model
            .item(99_999_999)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        assert_eq!(*item.borrow::<u64>(), 100_000_000);
        assert!(model.item(100_000_000).is_none());
    }
}
