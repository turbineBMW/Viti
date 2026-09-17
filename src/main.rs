//! Viti: MongoDB Compass, the GTK way, with vi keys.
//!
//! All state lives on the GTK thread; the driver runs on tokio and reports
//! back through one-shot channels or the `Event` bus drained by a glib-local
//! future.
// Later phases consume the remaining unused items (events, ops, helpers).
#![allow(dead_code)]

mod accent;
mod ai;
mod app;
mod commands;
mod config;
mod dispatch;
mod events;
mod export_to_language;
mod focus;
mod keybinds;
mod mongo;
mod notify;
mod query_complete;
mod rt;
mod secrets;
mod style;
mod ui;

use gtk4::prelude::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VITI_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("viti=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // `viti [mongodb://...]` connects straight away (saved as a profile if new).
    let uri_arg: Option<String> = std::env::args().nth(1).filter(|a| a.starts_with("mongodb"));

    let application = adw::Application::builder()
        .application_id(notify::APP_ID)
        .flags(gtk4::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_activate(move |a| {
        let app = app::App::build(a);
        if let Some(uri) = uri_arg.clone() {
            let (bare, pw) = mongo::profile::split_password(&uri);
            let id = {
                let mut cfg = app.config.borrow_mut();
                match cfg.connections.iter().find(|p| p.uri == bare) {
                    Some(p) => p.id,
                    None => {
                        let p = config::ConnectionProfile {
                            uri: bare.clone(),
                            ..Default::default()
                        };
                        let id = p.id;
                        cfg.connections.push(p);
                        id
                    }
                }
            };
            if let Some(pw) = pw {
                let store = app.config.borrow().settings.secret_store;
                if let Some(w) = secrets::set(store, id, &bare, &pw) {
                    app.toast(&w);
                }
            }
            app.save_now();
            app.sidebar.reload_connections();
            app.connect_profile(id);
        }
    });
    application.run_with_args::<&str>(&[]);
}
