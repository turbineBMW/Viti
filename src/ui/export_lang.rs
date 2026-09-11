//! "Export to language": the query bar's find or the aggregation pipeline as
//! driver code in eight languages, optionally wrapped in connect + run code.
use crate::app::App;
use crate::export_to_language::{self as etl, Input, LANGS, Lang, Options};
use crate::mongo::ConnectionId;
use crate::mongo::ops::Namespace;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use sourceview5::prelude::*;
use std::rc::Rc;

pub fn show(app: &Rc<App>, conn: ConnectionId, ns: Namespace, input: Input) {
    let settings = app.config.borrow().settings.clone();
    let uri = app
        .config
        .borrow()
        .profile(conn)
        .map(|p| crate::mongo::profile::redact_uri(&p.uri))
        .unwrap_or_else(|| "mongodb://localhost:27017".into());
    let what = match &input {
        Input::Find(_) => "query",
        Input::Pipeline(_) => "pipeline",
    };
    let dialog = adw::Dialog::builder()
        .title(format!("Export {what} to language"))
        .content_width(820)
        .content_height(640)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let labels: Vec<&str> = LANGS.iter().map(|l| l.1).collect();
    let lang_dd = gtk::DropDown::from_strings(&labels);
    lang_dd.set_selected(
        LANGS
            .iter()
            .position(|l| l.1 == settings.export_language)
            .unwrap_or(5) as u32,
    );
    let driver = gtk::ToggleButton::builder()
        .label("Driver code")
        .tooltip_text("Wrap in connect + find / aggregate code")
        .active(settings.export_driver_code)
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy (C)")
        .build();
    header.pack_start(&lang_dd);
    header.pack_start(&driver);
    header.pack_end(&copy);
    toolbar.add_top_bar(&header);
    let view = crate::ui::source_view("", "python3");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    let hint = gtk::Label::builder()
        .label(format!(
            "{ns} · the connection string is shown with secrets redacted"
        ))
        .css_classes(["dim-label", "caption"])
        .margin_start(12)
        .margin_bottom(6)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    body.append(&scroller);
    body.append(&hint);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let render: Rc<dyn Fn() -> String> = {
        let (lang_dd, driver, view, app) =
            (lang_dd.clone(), driver.clone(), view.clone(), app.clone());
        let ns = ns.clone();
        Rc::new(move || {
            let (lang, label, source_id) = LANGS
                .get(lang_dd.selected() as usize)
                .copied()
                .unwrap_or((Lang::Python, "Python", "python3"));
            let code = etl::export(
                &input,
                lang,
                &Options {
                    driver: driver.is_active(),
                    uri: uri.clone(),
                    ns: ns.clone(),
                },
            );
            let buffer = view.buffer();
            if let Ok(b) = buffer.downcast::<sourceview5::Buffer>() {
                b.set_language(
                    sourceview5::LanguageManager::default()
                        .language(source_id)
                        .as_ref(),
                );
                b.set_text(&code);
            }
            {
                let mut cfg = app.config.borrow_mut();
                cfg.settings.export_language = label.to_string();
                cfg.settings.export_driver_code = driver.is_active();
            }
            app.schedule_save();
            code
        })
    };
    render();
    {
        let render = render.clone();
        lang_dd.connect_selected_notify(move |_| {
            render();
        });
    }
    {
        let render = render.clone();
        driver.connect_toggled(move |_| {
            render();
        });
    }
    let do_copy: Rc<dyn Fn()> = {
        let view = view.clone();
        let app = app.clone();
        Rc::new(move || {
            crate::ui::copy_text(&crate::ui::buffer_text(&view.buffer()));
            app.toast("Code copied");
        })
    };
    {
        let do_copy = do_copy.clone();
        copy.connect_clicked(move |_| do_copy());
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let dialog = dialog.clone();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gtk::gdk::Key::Escape | gtk::gdk::Key::q => {
                dialog.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::C => {
                do_copy();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
    }
    dialog.add_controller(keys);
    dialog.present(Some(&app.window));
}
