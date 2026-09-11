//! Connection profiles: the editor dialog and the manager list behind Ctrl+O.
//!
//! The editor is a URI builder in both directions: the connection-string entry
//! and the form pages (General / Authentication / TLS / SSH / Advanced) edit
//! the same `UriParts`, so pasting a string fills the form and toggling a
//! switch rewrites the string. Secrets never land in `connections.json`: the
//! user password and the TLS key password are split off and stored via
//! `secrets`. The SSH tunnel is the one thing that is not a URI option.
use crate::app::App;
use crate::config::{COLOURS, ConnectionProfile, ProfileExport, SshTunnel};
use crate::mongo::ConnectionId;
use crate::mongo::profile::{self as uri, UriParts};
use crate::secrets::ProfileSecrets;
use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::Cell;
use std::rc::Rc;

/// (label, authMechanism value; "" = not set)
const MECHANISMS: &[(&str, &str)] = &[
    ("Default (username/password or none)", ""),
    ("SCRAM-SHA-256", "SCRAM-SHA-256"),
    ("SCRAM-SHA-1", "SCRAM-SHA-1"),
    ("X.509 certificate", "MONGODB-X509"),
    ("LDAP (PLAIN)", "PLAIN"),
    ("Kerberos (GSSAPI)", "GSSAPI"),
    ("AWS IAM", "MONGODB-AWS"),
];

const READ_PREFS: &[(&str, &str)] = &[
    ("Default (primary)", ""),
    ("primary", "primary"),
    ("primaryPreferred", "primaryPreferred"),
    ("secondary", "secondary"),
    ("secondaryPreferred", "secondaryPreferred"),
    ("nearest", "nearest"),
];

/// URI options the form owns; anything else is kept verbatim in "Other options".
const HANDLED: &[&str] = &[
    "authmechanism",
    "authsource",
    "authmechanismproperties",
    "tls",
    "ssl",
    "tlscafile",
    "tlscertificatekeyfile",
    "tlscertificatekeyfilepassword",
    "tlsallowinvalidhostnames",
    "tlsallowinvalidcertificates",
    "tlsinsecure",
    "directconnection",
    "readpreference",
    "replicaset",
    "appname",
    "connecttimeoutms",
    "serverselectiontimeoutms",
];

/// Every widget of the editor form.
struct Form {
    name: adw::EntryRow,
    uri_row: adw::EntryRow,
    colour: adw::ComboRow,
    fav: adw::SwitchRow,
    // General
    hosts: adw::EntryRow,
    srv: adw::SwitchRow,
    direct: adw::SwitchRow,
    database: adw::EntryRow,
    // Authentication
    mechanism: adw::ComboRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    auth_source: adw::EntryRow,
    mech_props: adw::EntryRow,
    // TLS
    tls: adw::SwitchRow,
    ca_file: adw::EntryRow,
    cert_file: adw::EntryRow,
    key_password: adw::PasswordEntryRow,
    allow_invalid_hosts: adw::SwitchRow,
    allow_invalid_certs: adw::SwitchRow,
    // SSH
    ssh_on: adw::SwitchRow,
    ssh_host: adw::EntryRow,
    ssh_port: adw::SpinRow,
    ssh_user: adw::EntryRow,
    ssh_identity: adw::EntryRow,
    // Advanced
    read_pref: adw::ComboRow,
    replica_set: adw::EntryRow,
    app_name: adw::EntryRow,
    connect_timeout: adw::SpinRow,
    select_timeout: adw::SpinRow,
    extra: adw::EntryRow,
    /// Guards against the two sync directions re-entering each other.
    syncing: Cell<bool>,
    base: ConnectionProfile,
}

fn entry(title: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).build()
}
fn mono_entry(title: &str) -> adw::EntryRow {
    let e = entry(title);
    e.add_css_class("viti-mono");
    e
}
fn switch(title: &str, subtitle: Option<&str>) -> adw::SwitchRow {
    let s = adw::SwitchRow::builder().title(title).build();
    if let Some(sub) = subtitle {
        s.set_subtitle(sub);
    }
    s
}
fn combo(title: &str, items: &[(&str, &str)]) -> adw::ComboRow {
    let c = adw::ComboRow::builder().title(title).build();
    c.set_model(Some(&gtk::StringList::new(
        &items.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
    )));
    c
}
fn spin(title: &str, min: f64, max: f64, step: f64) -> adw::SpinRow {
    adw::SpinRow::with_range(min, max, step).tap(|s| s.set_title(title))
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}
impl<T> Tap for T {}

/// A file-picker button that fills an entry row with the chosen path.
fn attach_file_button(row: &adw::EntryRow, window: &adw::ApplicationWindow, title: &str) {
    let btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Choose file")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let row2 = row.clone();
    let window = window.clone();
    let title = title.to_string();
    btn.connect_clicked(move |_| {
        let dialog = gtk::FileDialog::builder().title(&title).modal(true).build();
        let row = row2.clone();
        let window = window.clone();
        glib::spawn_future_local(async move {
            if let Ok(f) = dialog.open_future(Some(&window)).await
                && let Some(p) = f.path()
            {
                row.set_text(&p.to_string_lossy());
            }
        });
    });
    row.add_suffix(&btn);
}

fn selected_value(combo: &adw::ComboRow, items: &[(&'static str, &'static str)]) -> &'static str {
    items
        .get(combo.selected() as usize)
        .map(|(_, v)| *v)
        .unwrap_or("")
}

fn select_value(combo: &adw::ComboRow, items: &[(&str, &str)], value: &str) {
    let idx = items
        .iter()
        .position(|(_, v)| v.eq_ignore_ascii_case(value))
        .unwrap_or(0);
    combo.set_selected(idx as u32);
}

impl Form {
    /// Fill every field from a connection string. Secrets found inside it
    /// (password, TLS key password) move to their password rows.
    fn load_uri(&self, text: &str) {
        let Ok(parts) = UriParts::parse(text) else {
            return;
        };
        self.hosts.set_text(&parts.hosts.join(","));
        self.srv.set_active(parts.srv);
        self.database
            .set_text(parts.database.as_deref().unwrap_or(""));
        self.direct
            .set_active(parts.get_bool("directConnection").unwrap_or(false));
        select_value(
            &self.mechanism,
            MECHANISMS,
            parts.get("authMechanism").unwrap_or(""),
        );
        self.username
            .set_text(parts.username.as_deref().unwrap_or(""));
        if let Some(pw) = &parts.password
            && !pw.is_empty()
        {
            self.password.set_text(pw);
        }
        self.auth_source
            .set_text(parts.get("authSource").unwrap_or(""));
        self.mech_props
            .set_text(parts.get("authMechanismProperties").unwrap_or(""));
        let tls = parts
            .get_bool("tls")
            .or_else(|| parts.get_bool("ssl"))
            .unwrap_or(parts.srv && parts.get("tls").is_none() && parts.get("ssl").is_none());
        self.tls.set_active(tls);
        self.ca_file.set_text(parts.get("tlsCAFile").unwrap_or(""));
        self.cert_file
            .set_text(parts.get("tlsCertificateKeyFile").unwrap_or(""));
        if let Some(kp) = parts.get("tlsCertificateKeyFilePassword")
            && !kp.is_empty()
        {
            self.key_password.set_text(kp);
        }
        let insecure = parts.get_bool("tlsInsecure").unwrap_or(false);
        self.allow_invalid_hosts
            .set_active(insecure || parts.get_bool("tlsAllowInvalidHostnames").unwrap_or(false));
        self.allow_invalid_certs.set_active(
            insecure
                || parts
                    .get_bool("tlsAllowInvalidCertificates")
                    .unwrap_or(false),
        );
        select_value(
            &self.read_pref,
            READ_PREFS,
            parts.get("readPreference").unwrap_or(""),
        );
        self.replica_set
            .set_text(parts.get("replicaSet").unwrap_or(""));
        self.app_name.set_text(parts.get("appName").unwrap_or(""));
        self.connect_timeout.set_value(
            parts
                .get("connectTimeoutMS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
        );
        self.select_timeout.set_value(
            parts
                .get("serverSelectionTimeoutMS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
        );
        let extra: Vec<String> = parts
            .options
            .iter()
            .filter(|(k, _)| !HANDLED.contains(&k.to_ascii_lowercase().as_str()))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        self.extra.set_text(&extra.join("&"));
    }

    /// The connection string the form describes, without secrets.
    fn build_parts(&self) -> UriParts {
        let mut parts = UriParts {
            srv: self.srv.is_active(),
            hosts: self
                .hosts
                .text()
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string)
                .collect(),
            username: Some(self.username.text().trim().to_string()).filter(|u| !u.is_empty()),
            password: None,
            database: Some(self.database.text().trim().to_string()).filter(|d| !d.is_empty()),
            options: Vec::new(),
        };
        if parts.hosts.is_empty() {
            parts.hosts.push("localhost:27017".into());
        }
        // Unhandled options first, so they keep their place.
        for kv in self
            .extra
            .text()
            .split('&')
            .filter(|s| !s.trim().is_empty())
        {
            match kv.trim().split_once('=') {
                Some((k, v)) if !HANDLED.contains(&k.to_ascii_lowercase().as_str()) => {
                    parts.options.push((k.to_string(), v.to_string()))
                }
                None if !HANDLED.contains(&kv.trim().to_ascii_lowercase().as_str()) => {
                    parts.options.push((kv.trim().to_string(), String::new()))
                }
                _ => {}
            }
        }
        let mech = selected_value(&self.mechanism, MECHANISMS);
        parts.set("authMechanism", Some(mech));
        parts.set("authSource", Some(self.auth_source.text().trim()));
        parts.set(
            "authMechanismProperties",
            Some(self.mech_props.text().trim()),
        );
        let tls = self.tls.is_active();
        // SRV implies TLS; only spell it out when it deviates or is explicit.
        if tls && !parts.srv {
            parts.set_bool("tls", Some(true));
        } else if !tls && parts.srv {
            parts.set_bool("tls", Some(false));
        }
        if tls {
            parts.set("tlsCAFile", Some(self.ca_file.text().trim()));
            parts.set("tlsCertificateKeyFile", Some(self.cert_file.text().trim()));
            if self.allow_invalid_hosts.is_active() {
                parts.set_bool("tlsAllowInvalidHostnames", Some(true));
            }
            if self.allow_invalid_certs.is_active() {
                parts.set_bool("tlsAllowInvalidCertificates", Some(true));
            }
        }
        if self.direct.is_active() {
            parts.set_bool("directConnection", Some(true));
        }
        parts.set(
            "readPreference",
            Some(selected_value(&self.read_pref, READ_PREFS)),
        );
        parts.set("replicaSet", Some(self.replica_set.text().trim()));
        parts.set("appName", Some(self.app_name.text().trim()));
        let ct = self.connect_timeout.value() as u64;
        parts.set(
            "connectTimeoutMS",
            (ct > 0).then(|| ct.to_string()).as_deref(),
        );
        let st = self.select_timeout.value() as u64;
        parts.set(
            "serverSelectionTimeoutMS",
            (st > 0).then(|| st.to_string()).as_deref(),
        );
        parts
    }

    fn ssh(&self) -> Option<SshTunnel> {
        if !self.ssh_on.is_active() {
            return None;
        }
        Some(SshTunnel {
            host: self.ssh_host.text().trim().to_string(),
            port: self.ssh_port.value() as u16,
            username: self.ssh_user.text().trim().to_string(),
            identity_file: self.ssh_identity.text().trim().to_string(),
        })
        .filter(|s| s.is_configured())
    }

    fn collect(&self) -> (ConnectionProfile, ProfileSecrets) {
        let c = COLOURS[self.colour.selected() as usize].1;
        (
            ConnectionProfile {
                id: self.base.id,
                name: self.name.text().trim().to_string(),
                colour: (!c.is_empty()).then(|| c.to_string()),
                favourite: self.fav.is_active(),
                last_used: self.base.last_used,
                uri: self.build_parts().to_uri(),
                ssh: self.ssh(),
            },
            ProfileSecrets {
                password: Some(self.password.text().to_string()).filter(|s| !s.is_empty()),
                tls_key_password: Some(self.key_password.text().to_string())
                    .filter(|s| !s.is_empty()),
            },
        )
    }
}

/// New (`None`) or edit an existing profile.
pub fn show_editor(app: &Rc<App>, existing: Option<ConnectionId>) {
    let profile = existing
        .and_then(|id| app.config.borrow().profile(id).cloned())
        .unwrap_or_default();
    let is_new = existing.is_none();
    let stored = if is_new {
        ProfileSecrets::default()
    } else {
        crate::secrets::get_all(app.config.borrow().settings.secret_store, profile.id)
    };

    let dialog = adw::Dialog::builder()
        .title(if is_new {
            "New connection"
        } else {
            "Edit connection"
        })
        .content_width(680)
        .content_height(760)
        .build();
    let toolbar = adw::ToolbarView::new();
    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    toolbar.add_top_bar(&header);

    // ----- General -----
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("network-server-symbolic")
        .build();
    let g1 = adw::PreferencesGroup::new();
    let name = entry("Name");
    name.set_text(&profile.name);
    let uri_row = mono_entry("Connection string");
    uri_row.set_text(&profile.uri);
    let colour = combo("Colour", COLOURS);
    colour.set_selected(
        COLOURS
            .iter()
            .position(|(_, c)| Some(*c) == profile.colour.as_deref())
            .unwrap_or(0) as u32,
    );
    let fav = switch("Favourite", None);
    fav.set_active(profile.favourite);
    for w in [&name, &uri_row] {
        g1.add(w);
    }
    g1.add(&colour);
    g1.add(&fav);
    general.add(&g1);
    let g2 = adw::PreferencesGroup::builder()
        .title("Hosts")
        .description("The form and the connection string edit each other.")
        .build();
    let hosts = mono_entry("Hosts (comma-separated host:port)");
    let srv = switch(
        "DNS seed list (mongodb+srv://)",
        Some("One hostname; hosts and TLS come from DNS"),
    );
    let direct = switch(
        "Direct connection",
        Some("Talk to this host only, without replica-set discovery"),
    );
    let database = entry("Default database");
    for w in [&hosts, &database] {
        g2.add(w);
    }
    g2.add(&srv);
    g2.add(&direct);
    general.add(&g2);
    stack
        .add_titled(&general, Some("general"), "General")
        .set_icon_name(Some("network-server-symbolic"));

    // ----- Authentication -----
    let auth = adw::PreferencesPage::builder()
        .title("Authentication")
        .build();
    let a1 = adw::PreferencesGroup::new();
    let mechanism = combo("Mechanism", MECHANISMS);
    let username = entry("Username");
    let password = adw::PasswordEntryRow::builder().title("Password").build();
    let auth_source = entry("Authentication database (authSource)");
    let mech_props = mono_entry("Mechanism properties (KEY:value,…)");
    a1.add(&mechanism);
    a1.add(&username);
    a1.add(&password);
    a1.add(&auth_source);
    a1.add(&mech_props);
    let pw_hint = gtk::Label::builder()
        .label("The password is stored in the keyring, never in the connection string on disk. A password pasted inside the string is moved here.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .margin_top(6)
        .build();
    a1.add(&pw_hint);
    auth.add(&a1);
    stack
        .add_titled(&auth, Some("auth"), "Auth")
        .set_icon_name(Some("dialog-password-symbolic"));

    // ----- TLS -----
    let tls_page = adw::PreferencesPage::builder().title("TLS").build();
    let t1 = adw::PreferencesGroup::new();
    let tls = switch("TLS / SSL", Some("Implied by mongodb+srv://"));
    let ca_file = mono_entry("CA certificate file (tlsCAFile)");
    attach_file_button(&ca_file, &app.window, "CA certificate");
    let cert_file = mono_entry("Client certificate and key (tlsCertificateKeyFile)");
    attach_file_button(&cert_file, &app.window, "Client certificate");
    let key_password = adw::PasswordEntryRow::builder()
        .title("Client key password")
        .build();
    let allow_invalid_hosts = switch("Allow invalid hostnames", None);
    let allow_invalid_certs = switch("Allow invalid certificates", None);
    t1.add(&tls);
    t1.add(&ca_file);
    t1.add(&cert_file);
    t1.add(&key_password);
    t1.add(&allow_invalid_hosts);
    t1.add(&allow_invalid_certs);
    tls_page.add(&t1);
    stack
        .add_titled(&tls_page, Some("tls"), "TLS")
        .set_icon_name(Some("channel-secure-symbolic"));

    // ----- SSH -----
    let ssh_page = adw::PreferencesPage::builder().title("SSH tunnel").build();
    let s1 = adw::PreferencesGroup::builder()
        .description("Port forwarding through a jump host with your ssh client. Key-based authentication only (ssh-agent or an identity file): there is no terminal to type a passphrase into.")
        .build();
    let ssh_on = switch("Use an SSH tunnel", None);
    let ssh_host = entry("SSH host");
    let ssh_port = spin("SSH port", 1.0, 65535.0, 1.0);
    ssh_port.set_value(22.0);
    let ssh_user = entry("SSH username");
    let ssh_identity = mono_entry("Identity file (optional)");
    attach_file_button(&ssh_identity, &app.window, "Identity file");
    s1.add(&ssh_on);
    s1.add(&ssh_host);
    s1.add(&ssh_port);
    s1.add(&ssh_user);
    s1.add(&ssh_identity);
    ssh_page.add(&s1);
    stack
        .add_titled(&ssh_page, Some("ssh"), "SSH")
        .set_icon_name(Some("utilities-terminal-symbolic"));

    // ----- Advanced -----
    let adv = adw::PreferencesPage::builder().title("Advanced").build();
    let v1 = adw::PreferencesGroup::new();
    let read_pref = combo("Read preference", READ_PREFS);
    let replica_set = entry("Replica set name");
    let app_name = entry("Application name (appName)");
    let connect_timeout = spin("Connect timeout (ms, 0 = default)", 0.0, 600_000.0, 1000.0);
    let select_timeout = spin(
        "Server selection timeout (ms, 0 = default)",
        0.0,
        600_000.0,
        1000.0,
    );
    let extra = mono_entry("Other URI options (key=value&amp;key=value)");
    v1.add(&read_pref);
    v1.add(&replica_set);
    v1.add(&app_name);
    v1.add(&connect_timeout);
    v1.add(&select_timeout);
    v1.add(&extra);
    adv.add(&v1);
    stack
        .add_titled(&adv, Some("advanced"), "Advanced")
        .set_icon_name(Some("emblem-system-symbolic"));

    // ----- bottom bar -----
    let test = gtk::Button::with_label("Test");
    let save = gtk::Button::with_label("Save");
    let connect = gtk::Button::with_label("Save & connect");
    connect.add_css_class("suggested-action");
    let status = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label", "caption"])
        .wrap(true)
        .build();
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom.set_margin_start(12);
    bottom.set_margin_end(12);
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(12);
    bottom.append(&status);
    bottom.append(&test);
    bottom.append(&save);
    bottom.append(&connect);
    toolbar.set_content(Some(&stack));
    toolbar.add_bottom_bar(&bottom);
    dialog.set_child(Some(&toolbar));

    let form = Rc::new(Form {
        name,
        uri_row: uri_row.clone(),
        colour,
        fav,
        hosts,
        srv,
        direct,
        database,
        mechanism,
        username,
        password,
        auth_source,
        mech_props,
        tls,
        ca_file,
        cert_file,
        key_password,
        allow_invalid_hosts,
        allow_invalid_certs,
        ssh_on,
        ssh_host,
        ssh_port,
        ssh_user,
        ssh_identity,
        read_pref,
        replica_set,
        app_name,
        connect_timeout,
        select_timeout,
        extra,
        syncing: Cell::new(false),
        base: profile.clone(),
    });

    // Initial state: URI -> form, stored secrets, SSH block.
    form.syncing.set(true);
    form.load_uri(&profile.uri);
    if let Some(pw) = &stored.password {
        form.password.set_text(pw);
    }
    if let Some(kp) = &stored.tls_key_password {
        form.key_password.set_text(kp);
    }
    if let Some(ssh) = &profile.ssh {
        form.ssh_on.set_active(true);
        form.ssh_host.set_text(&ssh.host);
        form.ssh_port.set_value(ssh.port as f64);
        form.ssh_user.set_text(&ssh.username);
        form.ssh_identity.set_text(&ssh.identity_file);
    }
    form.syncing.set(false);

    // form -> URI
    let form_changed = {
        let form = form.clone();
        Rc::new(move || {
            if form.syncing.get() {
                return;
            }
            form.syncing.set(true);
            form.uri_row.set_text(&form.build_parts().to_uri());
            form.syncing.set(false);
        })
    };
    for e in [
        &form.hosts,
        &form.database,
        &form.username,
        &form.auth_source,
        &form.mech_props,
        &form.ca_file,
        &form.cert_file,
        &form.replica_set,
        &form.app_name,
        &form.extra,
    ] {
        let f = form_changed.clone();
        e.connect_changed(move |_| f());
    }
    for s in [
        &form.srv,
        &form.direct,
        &form.tls,
        &form.allow_invalid_hosts,
        &form.allow_invalid_certs,
    ] {
        let f = form_changed.clone();
        s.connect_active_notify(move |_| f());
    }
    for c in [&form.mechanism, &form.read_pref] {
        let f = form_changed.clone();
        c.connect_selected_notify(move |_| f());
    }
    for s in [&form.connect_timeout, &form.select_timeout] {
        let f = form_changed.clone();
        s.connect_value_notify(move |_| f());
    }
    // URI -> form (and secrets pasted inside it move to their rows).
    {
        let form = form.clone();
        uri_row.connect_changed(move |row| {
            if form.syncing.get() {
                return;
            }
            let text = row.text().to_string();
            form.syncing.set(true);
            form.load_uri(&text);
            form.syncing.set(false);
            let (bare, pw) = uri::split_password(&text);
            let mut stripped = bare;
            if let Ok(mut parts) = UriParts::parse(&stripped)
                && parts.get("tlsCertificateKeyFilePassword").is_some()
            {
                parts.remove("tlsCertificateKeyFilePassword");
                stripped = parts.to_uri();
            }
            if pw.is_some() || stripped != text {
                let row = row.clone();
                let form = form.clone();
                crate::ui::idle(move || {
                    form.syncing.set(true);
                    row.set_text(&stripped);
                    form.syncing.set(false);
                });
            }
        });
    }
    // The SSH switch reveals nothing in the URI; it only gates the rows.
    {
        let form2 = form.clone();
        let sync = move |on: bool| {
            for r in [&form2.ssh_host, &form2.ssh_user, &form2.ssh_identity] {
                r.set_sensitive(on);
            }
            form2.ssh_port.set_sensitive(on);
        };
        sync(form.ssh_on.is_active());
        form.ssh_on
            .connect_active_notify(move |s| sync(s.is_active()));
    }

    let persist = {
        let app = app.clone();
        let form = form.clone();
        Rc::new(move || -> ConnectionProfile {
            let (p, secrets) = form.collect();
            {
                let mut cfg = app.config.borrow_mut();
                match cfg.profile_mut(p.id) {
                    Some(existing) => *existing = p.clone(),
                    None => cfg.connections.push(p.clone()),
                }
            }
            let store = app.config.borrow().settings.secret_store;
            if let Some(warn) = crate::secrets::set_all(store, p.id, &p.display_name(), &secrets) {
                app.toast(&warn);
            }
            app.save_now();
            app.sidebar.reload_connections();
            p
        })
    };

    {
        let form = form.clone();
        let status = status.clone();
        test.connect_clicked(move |btn| {
            let (p, secrets) = form.collect();
            btn.set_sensitive(false);
            status.set_text(if p.ssh.is_some() {
                "Opening the SSH tunnel and connecting…"
            } else {
                "Connecting…"
            });
            let status = status.clone();
            let btn = btn.clone();
            glib::spawn_future_local(async move {
                let r =
                    crate::rt::io(async move { crate::mongo::connect(&p, &secrets).await }).await;
                btn.set_sensitive(true);
                match r {
                    Ok(c) => status.set_text(&format!(
                        "Connected: MongoDB {} ({}){}",
                        c.server.version,
                        c.server.topology,
                        if c.tunnel.is_some() { " via SSH" } else { "" }
                    )),
                    Err(e) => status.set_text(&format!("Failed: {e:#}")),
                }
            });
        });
    }
    {
        let persist = persist.clone();
        let dialog = dialog.clone();
        save.connect_clicked(move |_| {
            persist();
            dialog.close();
        });
    }
    {
        let persist = persist.clone();
        let dialog = dialog.clone();
        let app = app.clone();
        connect.connect_clicked(move |_| {
            let p = persist();
            dialog.close();
            app.connect_profile(p.id);
        });
    }
    dialog.present(Some(&app.window));
    if is_new {
        uri_row.grab_focus();
    } else {
        form.name.grab_focus();
    }
}

pub fn remove_profile(app: &Rc<App>, id: ConnectionId) {
    let name = app
        .config
        .borrow()
        .profile(id)
        .map(|p| p.display_name())
        .unwrap_or_default();
    let app = app.clone();
    crate::ui::confirm(
        &app.window.clone(),
        "Remove connection?",
        &format!("“{name}” and its stored password will be forgotten."),
        "Remove",
        true,
        move || {
            app.disconnect(id);
            app.config.borrow_mut().connections.retain(|p| p.id != id);
            crate::secrets::delete_all(id);
            app.save_now();
            app.sidebar.reload_connections();
        },
    );
}

/// Duplicate a profile (and its secrets) under a new id.
pub fn duplicate_profile(app: &Rc<App>, id: ConnectionId) {
    let Some(mut p) = app.config.borrow().profile(id).cloned() else {
        return;
    };
    let store = app.config.borrow().settings.secret_store;
    let secrets = crate::secrets::get_all(store, id);
    p.id = uuid::Uuid::new_v4();
    p.name = format!("{} (copy)", p.display_name());
    p.last_used = None;
    if let Some(w) = crate::secrets::set_all(store, p.id, &p.display_name(), &secrets) {
        app.toast(&w);
    }
    app.config.borrow_mut().connections.push(p);
    app.save_now();
    app.sidebar.reload_connections();
}

/// Ctrl+O: the list of saved connections with connect / edit / remove, and
/// import/export behind the menu.
pub fn show_manager(app: &Rc<App>) {
    let dialog = adw::Dialog::builder()
        .title("Connections")
        .content_width(560)
        .content_height(520)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let new_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("New connection")
        .build();
    header.pack_start(&new_btn);
    let menu = gtk4::gio::Menu::new();
    menu.append(Some("Import connections…"), Some("conns.import"));
    menu.append(Some("Export connections…"), Some("conns.export"));
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .build();
    header.pack_end(&menu_btn);
    toolbar.add_top_bar(&header);
    let actions = gtk4::gio::SimpleActionGroup::new();
    {
        let a = gtk4::gio::SimpleAction::new("import", None);
        let app = app.clone();
        let dialog = dialog.clone();
        a.connect_activate(move |_, _| {
            dialog.close();
            import_dialog(&app);
        });
        actions.add_action(&a);
    }
    {
        let a = gtk4::gio::SimpleAction::new("export", None);
        let app = app.clone();
        let dialog = dialog.clone();
        a.connect_activate(move |_, _| {
            dialog.close();
            export_dialog(&app);
        });
        actions.add_action(&a);
    }
    toolbar.insert_action_group("conns", Some(&actions));

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_selection_mode(gtk::SelectionMode::None);

    let mut profiles = app.config.borrow().connections.clone();
    profiles.sort_by(|a, b| {
        b.favourite
            .cmp(&a.favourite)
            .then_with(|| a.display_name().cmp(&b.display_name()))
    });
    if profiles.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No connections yet")
            .description("Add one with the + button, paste a connection string, or import a Compass export from the menu.")
            .build();
        toolbar.set_content(Some(&empty));
    } else {
        for p in profiles {
            let mut subtitle = uri::redact_uri(&p.uri);
            if let Some(ssh) = &p.ssh {
                subtitle.push_str(&format!("  ·  ssh {}", ssh.target()));
            }
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&p.display_name()))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(true)
                .build();
            if let Some(c) = &p.colour {
                let dot = gtk::Label::new(None);
                dot.set_markup(&format!("<span foreground=\"{c}\">●</span>"));
                row.add_prefix(&dot);
            }
            if p.favourite {
                row.add_prefix(&gtk::Image::from_icon_name("starred-symbolic"));
            }
            let connected = app.conn(p.id).is_some();
            let go = gtk::Button::builder()
                .icon_name(if connected {
                    "network-server-symbolic"
                } else {
                    "media-playback-start-symbolic"
                })
                .tooltip_text(if connected { "Connected" } else { "Connect" })
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .sensitive(!connected)
                .build();
            let edit = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Edit")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let dup = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Duplicate")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&go);
            row.add_suffix(&edit);
            row.add_suffix(&dup);
            row.add_suffix(&del);
            {
                let app = app.clone();
                let dialog = dialog.clone();
                let id = p.id;
                let f = move || {
                    dialog.close();
                    app.connect_profile(id);
                };
                let f2 = f.clone();
                go.connect_clicked(move |_| f());
                row.connect_activated(move |_| f2());
            }
            {
                let app = app.clone();
                let dialog = dialog.clone();
                let id = p.id;
                edit.connect_clicked(move |_| {
                    dialog.close();
                    show_editor(&app, Some(id));
                });
            }
            {
                let app = app.clone();
                let dialog = dialog.clone();
                let id = p.id;
                dup.connect_clicked(move |_| {
                    dialog.close();
                    duplicate_profile(&app, id);
                    show_manager(&app);
                });
            }
            {
                let app = app.clone();
                let dialog = dialog.clone();
                let id = p.id;
                del.connect_clicked(move |_| {
                    dialog.close();
                    remove_profile(&app, id);
                });
            }
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        toolbar.set_content(Some(&scroller));
    }
    {
        let app = app.clone();
        let dialog = dialog.clone();
        new_btn.connect_clicked(move |_| {
            dialog.close();
            show_editor(&app, None);
        });
    }
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&app.window));
}

fn json_filter() -> gtk::FileFilter {
    let f = gtk::FileFilter::new();
    f.set_name(Some("JSON"));
    f.add_suffix("json");
    f
}

/// Export every profile to a JSON file; passwords only on explicit opt-in.
pub fn export_dialog(app: &Rc<App>) {
    let ask = adw::AlertDialog::new(
        Some("Export connections"),
        Some(
            "Writes every saved connection to a JSON file that Viti (and Compass-style importers) can read back.",
        ),
    );
    let include = gtk::CheckButton::with_label("Include passwords (plain text in the file)");
    ask.set_extra_child(Some(&include));
    ask.add_responses(&[("cancel", "Cancel"), ("export", "Choose file…")]);
    ask.set_response_appearance("export", adw::ResponseAppearance::Suggested);
    ask.set_default_response(Some("export"));
    let window = app.window.clone();
    let app = app.clone();
    ask.connect_response(None, move |_, r| {
        if r != "export" {
            return;
        }
        let with_secrets = include.is_active();
        let app = app.clone();
        glib::spawn_future_local(async move {
            let filters = gtk4::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&json_filter());
            let picker = gtk::FileDialog::builder()
                .title("Export connections")
                .initial_name("viti-connections.json")
                .filters(&filters)
                .modal(true)
                .build();
            let Ok(file) = picker.save_future(Some(&app.window)).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            let store = app.config.borrow().settings.secret_store;
            let profiles = app.config.borrow().connections.clone();
            let entries: Vec<ProfileExport> = profiles
                .into_iter()
                .map(|p| {
                    let password = if with_secrets {
                        crate::secrets::get(store, p.id).filter(|s| !s.is_empty())
                    } else {
                        None
                    };
                    ProfileExport {
                        profile: p,
                        password,
                    }
                })
                .collect();
            let n = entries.len();
            let text = crate::config::export_profiles(&entries);
            match std::fs::write(&path, text) {
                Ok(()) => app.toast(&format!(
                    "Exported {n} connection{} to {}",
                    if n == 1 { "" } else { "s" },
                    path.display()
                )),
                Err(e) => app.toast_error("export connections", &e.into()),
            }
        });
    });
    ask.present(Some(&window));
}

/// Import profiles from a Viti or Compass export; same ids are replaced.
pub fn import_dialog(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let filters = gtk4::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&json_filter());
        let picker = gtk::FileDialog::builder()
            .title("Import connections")
            .filters(&filters)
            .modal(true)
            .build();
        let Ok(file) = picker.open_future(Some(&app.window)).await else {
            return;
        };
        let Some(path) = file.path() else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                app.toast_error("import connections", &e.into());
                return;
            }
        };
        let entries = match crate::config::import_profiles(&text) {
            Ok(e) => e,
            Err(e) => {
                app.toast(&format!("Import failed: {e}"));
                return;
            }
        };
        if entries.is_empty() {
            app.toast("No connections in that file");
            return;
        }
        let names: Vec<String> = entries
            .iter()
            .map(|e| format!("• {}", e.profile.display_name()))
            .collect();
        let with_pw = entries.iter().filter(|e| e.password.is_some()).count();
        let ask = adw::AlertDialog::new(
            Some(&format!(
                "Import {} connection{}?",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            )),
            Some(&format!(
                "{}{}",
                names.join("\n"),
                if with_pw > 0 {
                    format!("\n\n{with_pw} include a password; it will be stored in the keyring.")
                } else {
                    String::new()
                }
            )),
        );
        ask.add_responses(&[("cancel", "Cancel"), ("import", "Import")]);
        ask.set_response_appearance("import", adw::ResponseAppearance::Suggested);
        ask.set_default_response(Some("import"));
        let app2 = app.clone();
        ask.connect_response(None, move |_, r| {
            if r != "import" {
                return;
            }
            let store = app2.config.borrow().settings.secret_store;
            let mut replaced = 0;
            {
                let mut cfg = app2.config.borrow_mut();
                for e in &entries {
                    match cfg.profile_mut(e.profile.id) {
                        Some(existing) => {
                            *existing = e.profile.clone();
                            replaced += 1;
                        }
                        None => cfg.connections.push(e.profile.clone()),
                    }
                }
            }
            for e in &entries {
                if let Some(pw) = &e.password
                    && let Some(w) =
                        crate::secrets::set(store, e.profile.id, &e.profile.display_name(), pw)
                {
                    app2.toast(&w);
                }
            }
            app2.save_now();
            app2.sidebar.reload_connections();
            app2.toast(&format!(
                "Imported {} connection{}{}",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" },
                if replaced > 0 {
                    format!(" ({replaced} replaced)")
                } else {
                    String::new()
                }
            ));
        });
        ask.present(Some(&app.window));
    });
}
