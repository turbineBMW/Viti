//! Desktop notifications over org.freedesktop.Notifications directly (rather than
//! `gio::Notification`), so we control the hints: the sound request, the
//! desktop-entry for icon/grouping, and the xdg-activation token the daemon hands
//! us when the user clicks — the only portable way to take focus on Wayland.
use crate::config::Sound;
use gtk4::gio;
use gtk4::glib::{self, prelude::*};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const BUS: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const APP_NAME: &str = "Viti";
pub const APP_ID: &str = "dev.turbinebmw.Viti";

pub struct Notice {
    /// Groups notifications: a new one on the same topic replaces the bubble.
    pub topic: String,
    pub title: String,
    pub body: String,
}

type OpenHandler = Box<dyn Fn(&str, Option<String>)>;

pub struct Notifier {
    conn: gio::DBusConnection,
    by_topic: RefCell<HashMap<String, u32>>,
    live: RefCell<HashMap<u32, String>>,
    /// Activation token the daemon sent just before an ActionInvoked, keyed by notification id.
    tokens: RefCell<HashMap<u32, String>>,
    on_open: RefCell<Option<OpenHandler>>,
}

impl Notifier {
    pub fn new() -> Option<Rc<Self>> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .map_err(|e| tracing::warn!("session bus: {e}"))
            .ok()?;
        let me = Rc::new(Self {
            conn,
            by_topic: Default::default(),
            live: Default::default(),
            tokens: Default::default(),
            on_open: Default::default(),
        });
        for sig in ["ActivationToken", "ActionInvoked", "NotificationClosed"] {
            let weak = Rc::downgrade(&me);
            #[allow(deprecated)]
            me.conn.signal_subscribe(
                Some(BUS),
                Some(BUS),
                Some(sig),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, name, params| {
                    if let Some(me) = weak.upgrade() {
                        me.on_signal(name, params);
                    }
                },
            );
        }
        Some(me)
    }

    /// Called with (topic, activation token) when a notification is clicked.
    pub fn set_on_open(&self, f: impl Fn(&str, Option<String>) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    fn on_signal(&self, name: &str, params: &glib::Variant) {
        let Some(id) = params.child_value(0).get::<u32>() else {
            return;
        };
        match name {
            "ActivationToken" => {
                if let Some(t) = params.child_value(1).get::<String>() {
                    self.tokens.borrow_mut().insert(id, t);
                }
            }
            "ActionInvoked" => {
                let token = self.tokens.borrow_mut().remove(&id);
                let live = self.live.borrow();
                let Some(topic) = live.get(&id) else { return };
                if let Some(f) = self.on_open.borrow().as_ref() {
                    f(topic, token);
                }
            }
            "NotificationClosed" => {
                if let Some(topic) = self.live.borrow_mut().remove(&id) {
                    self.by_topic.borrow_mut().remove(&topic);
                }
                self.tokens.borrow_mut().remove(&id);
            }
            _ => {}
        }
    }

    pub fn send(self: &Rc<Self>, n: Notice, sound: &Sound) {
        let replaces = self.by_topic.borrow().get(&n.topic).copied().unwrap_or(0);
        let actions = vec!["default".to_string(), "Open".to_string()];
        let icon = app_icon();
        let mut hints: HashMap<String, glib::Variant> = HashMap::new();
        hints.insert("desktop-entry".into(), APP_ID.to_variant());
        if icon.starts_with('/') {
            hints.insert("image-path".into(), icon.to_variant());
        }
        hints.insert("urgency".into(), 1u8.to_variant());
        match sound {
            Sound::SystemDefault => {
                hints.insert("sound-name".into(), "message-new-instant".to_variant());
            }
            Sound::File(p) => {
                hints.insert(
                    "sound-file".into(),
                    p.to_string_lossy().as_ref().to_variant(),
                );
            }
            Sound::None => {
                hints.insert("suppress-sound".into(), true.to_variant());
            }
        }
        let args = (
            APP_NAME,
            replaces,
            icon.as_str(),
            n.title.as_str(),
            n.body.as_str(),
            actions,
            hints,
            -1i32,
        )
            .to_variant();
        let me = self.clone();
        let topic = n.topic;
        glib::spawn_future_local(async move {
            match me
                .conn
                .call_future(
                    Some(BUS),
                    PATH,
                    BUS,
                    "Notify",
                    Some(&args),
                    None,
                    gio::DBusCallFlags::NONE,
                    5000,
                )
                .await
            {
                Ok(r) => {
                    let Some(id) = r.child_value(0).get::<u32>() else {
                        return;
                    };
                    if replaces != 0 && replaces != id {
                        me.live.borrow_mut().remove(&replaces);
                    }
                    me.by_topic.borrow_mut().insert(topic.clone(), id);
                    me.live.borrow_mut().insert(id, topic);
                }
                Err(e) => tracing::warn!("Notify: {e}"),
            }
        });
    }
}

/// The app icon as an absolute path when it's installed (any daemon can show that), else the
/// theme name for the daemon to resolve itself.
fn app_icon() -> String {
    let rel = format!("icons/hicolor/128x128/apps/{APP_ID}.png");
    let candidates = [
        directories::BaseDirs::new().map(|b| b.data_dir().join(&rel)),
        Some(std::path::PathBuf::from("/usr/share").join(&rel)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| APP_ID.into())
}
