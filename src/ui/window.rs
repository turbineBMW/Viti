//! The window chrome. Layout, top to bottom:
//!
//! ToastOverlay
//!   Banner (connection state / long ops)
//!   OverlaySplitView
//!     sidebar: connections + database tree
//!     content: TabBar + TabView of collection tabs (or the welcome page)
//!   Paned (vertical): the above | editor/shell pane (hidden until used)
//!   Revealer: the `:` command line
use adw::prelude::*;
use gtk4 as gtk;

pub struct Chrome {
    pub window: adw::ApplicationWindow,
    pub toast: adw::ToastOverlay,
    pub banner: adw::Banner,
    pub split: adw::OverlaySplitView,
    pub content_stack: gtk::Stack,
    pub tab_view: adw::TabView,
    pub tab_bar: adw::TabBar,
    pub page_picker: gtk::DropDown,
    pub cmd_revealer: gtk::Revealer,
    pub welcome: adw::StatusPage,
    pub menu_button: gtk::MenuButton,
}

pub fn build(app: &adw::Application, sidebar: &gtk::Widget, cmdline: &gtk::Widget) -> Chrome {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Viti")
        .default_width(1280)
        .default_height(820)
        .build();

    // Bubo-style split header: the sidebar owns "Viti", the content owns the
    // namespace title. Both bars sit inside the OverlaySplitView.
    let sidebar_header = adw::HeaderBar::builder()
        .title_widget(
            &gtk::Label::builder()
                .label("Viti")
                .css_classes(["title"])
                .build(),
        )
        .show_end_title_buttons(false)
        .build();
    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle sidebar (Ctrl+N)")
        .active(true)
        .build();
    sidebar_header.pack_start(&sidebar_toggle);
    let menu = gtk::gio::Menu::new();
    menu.append(Some("New connection…"), Some("win.new-connection"));
    menu.append(Some("Connections…"), Some("win.connections"));
    menu.append(Some("Keybindings"), Some("win.help"));
    menu.append(Some("Settings"), Some("win.settings"));
    menu.append(Some("About Viti"), Some("win.about"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu")
        .build();
    sidebar_header.pack_end(&menu_button);
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(sidebar));

    // The content header carries the tab bar itself, in place of a title.
    // Tabs keep their natural width (vmux-style chips starting at the left)
    // rather than one title-shaped band across the header; `style::BUILTIN`
    // strips the toolbar padding Adwaita gives a standalone tab bar so the
    // strip fits a header's 34px content height.
    let tab_view = adw::TabView::new();
    let tab_bar = adw::TabBar::builder()
        .view(&tab_view)
        .autohide(false)
        .expand_tabs(false)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&tab_bar)
        .show_start_title_buttons(false)
        .build();
    // Only shown while the sidebar is hidden, so there is always a way back.
    let sidebar_restore = gtk::Button::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Show sidebar (Ctrl+N)")
        .visible(false)
        .build();
    header.pack_start(&sidebar_restore);
    // Picks the current tab's page (Documents / Aggregations / …). Hidden
    // while no tab is open; the app keeps it in sync with the selected tab.
    let titles: Vec<&str> = crate::ui::collection::PAGES.iter().map(|p| p.1).collect();
    let page_picker = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&titles))
        .tooltip_text("Collection page")
        .visible(false)
        .valign(gtk::Align::Center)
        .css_classes(["viti-page-picker"])
        .build();
    header.pack_start(&page_picker);

    let welcome = adw::StatusPage::builder()
        .icon_name("network-server-symbolic")
        .paintable(
            &crate::ui::app_icon_paintable()
                .map(|t| t.upcast::<gtk::gdk::Paintable>())
                .unwrap_or_else(|| gtk::gdk::Paintable::new_empty(0, 0)),
        )
        .title("Viti")
        .description("Press Ctrl+O to add a connection, : for the command line, ? for keys")
        .build();
    let content_stack = gtk::Stack::new();
    content_stack.add_named(&welcome, Some("welcome"));
    tab_view.set_vexpand(true);
    content_stack.add_named(&tab_view, Some("tabs"));
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&header);
    content_view.set_content(Some(&content_stack));

    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_view)
        .content(&content_view)
        .min_sidebar_width(200.0)
        .max_sidebar_width(480.0)
        .sidebar_width_fraction(0.22)
        .build();
    split
        .bind_property("show-sidebar", &sidebar_toggle, "active")
        .bidirectional()
        .sync_create()
        .build();
    split
        .bind_property("show-sidebar", &sidebar_restore, "visible")
        .invert_boolean()
        .sync_create()
        .build();
    {
        let split = split.clone();
        sidebar_restore.connect_clicked(move |_| split.set_show_sidebar(true));
    }

    let cmd_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(120)
        .reveal_child(false)
        .child(cmdline)
        .build();

    let banner = adw::Banner::builder().revealed(false).build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner);
    body.append(&split);
    split.set_vexpand(true);
    body.append(&cmd_revealer);

    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&body));
    window.set_content(Some(&toast));

    Chrome {
        window,
        toast,
        banner,
        split,
        content_stack,
        tab_view,
        tab_bar,
        page_picker,
        cmd_revealer,
        welcome,
        menu_button,
    }
}
