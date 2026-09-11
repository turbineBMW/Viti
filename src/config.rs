//! Persistent state under `~/.config/viti/`: settings, connection profiles and
//! query history. Every struct is `#[serde(default)]` so files written by older
//! or newer versions still load; unknown fields are dropped silently.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

pub fn config_dir() -> PathBuf {
    gtk4::glib::user_config_dir().join("viti")
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "kind", content = "path", rename_all = "kebab-case")]
pub enum Sound {
    #[default]
    SystemDefault,
    File(PathBuf),
    None,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DocView {
    #[default]
    List,
    Json,
    Table,
}

/// Where `e` / the pencil edit a document.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EditorMode {
    /// The in-app Save/Cancel dialog.
    #[default]
    InApp,
    /// The external editor in the embedded terminal.
    External,
}

impl DocView {
    pub fn next(self) -> Self {
        match self {
            DocView::List => DocView::Json,
            DocView::Json => DocView::Table,
            DocView::Table => DocView::List,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            DocView::List => "List",
            DocView::Json => "JSON",
            DocView::Table => "Table",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SecretStore {
    #[default]
    Keyring,
    File,
    /// Never persist passwords; prompt on connect.
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AiSettings {
    /// "claude", "codex" or "custom".
    pub backend: String,
    pub custom_argv: Vec<String>,
    /// Whether the custom backend prints claude-style `{"result": ...}` JSON.
    pub custom_json_result: bool,
    pub include_sample_values: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            backend: "claude".into(),
            custom_argv: Vec::new(),
            custom_json_result: false,
            include_sample_values: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub read_only: bool,
    /// Cap applied to every server operation.
    pub max_time_ms: u64,
    pub theme: Theme,
    pub default_sort: String,
    pub protect_connection_strings: bool,
    pub page_size: u32,
    pub default_view: DocView,
    /// Where `e` edits; a non-empty `editor_command` forces `External`.
    pub editor_mode: EditorMode,
    /// Empty = `$EDITOR`, then `vi`.
    pub editor_command: String,
    pub mongosh_command: String,
    pub ai: AiSettings,
    pub schema_sample_size: u32,
    pub secret_store: SecretStore,
    pub desktop_notifications: bool,
    pub notification_sound: Sound,
    /// Action id -> accelerator override ("" = unbound). Mirrored to keybindings.json.
    pub keybindings: BTreeMap<String, String>,
    pub sidebar_width: i32,
    pub editor_pane_height: i32,
    /// Last language picked in "Export to language" (its label, e.g. "Python").
    pub export_language: String,
    /// "Export to language": wrap the literal in connect + find/aggregate code.
    pub export_driver_code: bool,
    /// Aggregations page: re-run stage previews while typing.
    pub agg_auto_preview: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            read_only: false,
            max_time_ms: 60_000,
            theme: Theme::System,
            default_sort: String::new(),
            protect_connection_strings: false,
            page_size: 25,
            default_view: DocView::List,
            editor_mode: EditorMode::InApp,
            editor_command: String::new(),
            mongosh_command: "mongosh".into(),
            ai: AiSettings::default(),
            schema_sample_size: 1000,
            secret_store: SecretStore::Keyring,
            desktop_notifications: true,
            notification_sound: Sound::SystemDefault,
            keybindings: BTreeMap::new(),
            sidebar_width: 280,
            editor_pane_height: 360,
            export_language: "Python".into(),
            export_driver_code: false,
            agg_auto_preview: true,
        }
    }
}

impl Settings {
    /// Whether `e` / the pencil should use the external editor: chosen
    /// explicitly, or implied by a configured editor command.
    pub fn uses_external_editor(&self) -> bool {
        self.editor_mode == EditorMode::External || !self.editor_command.trim().is_empty()
    }

    /// The editor argv: configured command, else `$EDITOR`, else `vi`.
    pub fn editor_argv(&self) -> Vec<String> {
        let cmd = if self.editor_command.trim().is_empty() {
            std::env::var("EDITOR").unwrap_or_default()
        } else {
            self.editor_command.clone()
        };
        let cmd = if cmd.trim().is_empty() {
            "vi".to_string()
        } else {
            cmd
        };
        shell_words::split(&cmd).unwrap_or_else(|_| vec![cmd])
    }
}

/// `ssh -N -L` port forwarding in front of the connection. Authentication is
/// key-based only (agent or identity file): the tunnel runs without a TTY.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SshTunnel {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: String,
}

impl Default for SshTunnel {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            username: String::new(),
            identity_file: String::new(),
        }
    }
}

impl SshTunnel {
    pub fn is_configured(&self) -> bool {
        !self.host.trim().is_empty()
    }
    pub fn target(&self) -> String {
        if self.username.trim().is_empty() {
            self.host.trim().to_string()
        } else {
            format!("{}@{}", self.username.trim(), self.host.trim())
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub colour: Option<String>,
    pub favourite: bool,
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
    /// Connection string with any password removed; secrets.rs holds it.
    /// Everything the driver understands (auth mechanism, TLS files, read
    /// preference…) lives here as URI options; the form edits them in place.
    pub uri: String,
    pub ssh: Option<SshTunnel>,
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            colour: None,
            favourite: false,
            last_used: None,
            uri: "mongodb://localhost:27017".into(),
            ssh: None,
        }
    }
}

/// Compass names its colours `color1`…`color10`; map them onto our palette.
pub const COLOURS: &[(&str, &str)] = &[
    ("None", ""),
    ("Red", "#e01b24"),
    ("Orange", "#ff7800"),
    ("Yellow", "#f6d32d"),
    ("Green", "#33d17a"),
    ("Blue", "#3584e4"),
    ("Purple", "#9141ac"),
    ("Pink", "#f66151"),
    ("Teal", "#2190a4"),
    ("Brown", "#986a44"),
];

fn compass_colour(c: &str) -> Option<String> {
    let idx: usize = c.strip_prefix("color")?.parse().ok()?;
    COLOURS.get(idx).map(|(_, hex)| hex.to_string())
}

/// A profile plus the password to travel with it, for import/export files.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ProfileExport {
    #[serde(flatten)]
    pub profile: ConnectionProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct ExportFile {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    connections: Vec<ProfileExport>,
}

pub fn export_profiles(entries: &[ProfileExport]) -> String {
    serde_json::to_string_pretty(&ExportFile {
        kind: "Viti Connections".into(),
        version: 1,
        connections: entries.to_vec(),
    })
    .unwrap_or_default()
}

/// Read a Viti export, a Compass export (`{"type":"Compass Connections",
/// "connections":[{"connectionOptions":{"connectionString":…}}]}`), or a bare
/// array of profiles. Passwords found inside connection strings are split off.
pub fn import_profiles(text: &str) -> Result<Vec<ProfileExport>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let items: Vec<serde_json::Value> = match &value {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => o
            .get("connections")
            .and_then(|c| c.as_array())
            .cloned()
            .ok_or_else(|| "no \"connections\" array".to_string())?,
        _ => return Err("expected an object or array".into()),
    };
    let mut out = Vec::new();
    for item in items {
        let obj = item
            .as_object()
            .ok_or("connection entry is not an object")?;
        let mut entry: ProfileExport = if obj.contains_key("connectionOptions") {
            let opts = &obj["connectionOptions"];
            let uri = opts
                .get("connectionString")
                .and_then(|s| s.as_str())
                .ok_or("Compass entry without connectionString")?
                .to_string();
            let fav = obj.get("favorite").and_then(|f| f.as_object());
            let ssh = opts
                .get("sshTunnel")
                .and_then(|t| t.as_object())
                .map(|t| SshTunnel {
                    host: t.get("host").and_then(|v| v.as_str()).unwrap_or("").into(),
                    port: t
                        .get("port")
                        .and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        })
                        .unwrap_or(22) as u16,
                    username: t
                        .get("username")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .into(),
                    identity_file: t
                        .get("identityKeyFile")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .into(),
                });
            ProfileExport {
                profile: ConnectionProfile {
                    id: obj
                        .get("id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .unwrap_or_else(Uuid::new_v4),
                    name: fav
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string(),
                    colour: fav
                        .and_then(|f| f.get("color"))
                        .and_then(|c| c.as_str())
                        .and_then(compass_colour),
                    favourite: fav.is_some(),
                    last_used: obj
                        .get("lastUsed")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok()),
                    uri,
                    ssh: ssh.filter(|s| s.is_configured()),
                },
                password: None,
            }
        } else {
            serde_json::from_value(item.clone()).map_err(|e| format!("bad profile: {e}"))?
        };
        let (bare, pw) = crate::mongo::profile::split_password(&entry.profile.uri);
        entry.profile.uri = bare;
        if pw.is_some() {
            entry.password = pw;
        }
        out.push(entry);
    }
    Ok(out)
}

impl ConnectionProfile {
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            crate::mongo::profile::redact_uri(&self.uri)
        } else {
            self.name.clone()
        }
    }

    /// Short form for tab titles: the name, else just the host.
    pub fn short_name(&self) -> String {
        if self.name.trim().is_empty() {
            crate::mongo::profile::host_label(&self.uri)
        } else {
            self.name.clone()
        }
    }
}

/// The query bar's fields, kept as typed text so the user's formatting survives
/// history round-trips; parsed to BSON only when run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Query {
    pub filter: String,
    pub project: String,
    pub sort: String,
    pub collation: String,
    pub skip: u64,
    pub limit: u64,
    pub max_time_ms: Option<u64>,
    pub hint: String,
}

impl Query {
    pub fn is_default(&self) -> bool {
        *self == Query::default()
    }
    /// One-line summary for history rows.
    pub fn summary(&self) -> String {
        let mut s = if self.filter.trim().is_empty() {
            "{}".to_string()
        } else {
            self.filter.trim().to_string()
        };
        if !self.sort.trim().is_empty() {
            s.push_str(&format!("  sort {}", self.sort.trim()));
        }
        if !self.project.trim().is_empty() {
            s.push_str(&format!("  project {}", self.project.trim()));
        }
        if self.skip > 0 {
            s.push_str(&format!("  skip {}", self.skip));
        }
        if self.limit > 0 {
            s.push_str(&format!("  limit {}", self.limit));
        }
        s
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SavedQuery {
    pub id: Uuid,
    pub name: Option<String>,
    /// "db.collection"
    pub ns: String,
    pub query: Query,
    pub last_run: chrono::DateTime<chrono::Utc>,
    pub favourite: bool,
    /// The connection it was saved on; My Queries prefers it when running.
    pub conn: Option<Uuid>,
}

impl Default for SavedQuery {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: None,
            ns: String::new(),
            query: Query::default(),
            last_run: chrono::Utc::now(),
            favourite: false,
            conn: None,
        }
    }
}

/// A saved aggregation (the Aggregations page's Save / Open).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SavedPipeline {
    pub id: Uuid,
    pub name: String,
    /// "db.collection"
    pub ns: String,
    pub conn: Option<Uuid>,
    pub pipeline: crate::mongo::pipeline::Pipeline,
    pub saved: chrono::DateTime<chrono::Utc>,
}

impl Default for SavedPipeline {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            ns: String::new(),
            conn: None,
            pipeline: Default::default(),
            saved: chrono::Utc::now(),
        }
    }
}

pub const HISTORY_PER_NS: usize = 200;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub connections: Vec<ConnectionProfile>,
    pub queries: Vec<SavedQuery>,
    pub pipelines: Vec<SavedPipeline>,
}

impl Config {
    pub fn profile(&self, id: Uuid) -> Option<&ConnectionProfile> {
        self.connections.iter().find(|p| p.id == id)
    }
    pub fn profile_mut(&mut self, id: Uuid) -> Option<&mut ConnectionProfile> {
        self.connections.iter_mut().find(|p| p.id == id)
    }

    /// Record a run query: bumps an identical history entry instead of duplicating
    /// it, and trims the per-namespace history (favourites are never trimmed).
    pub fn record_query(&mut self, ns: &str, query: &Query, conn: Option<Uuid>) {
        if let Some(existing) = self
            .queries
            .iter_mut()
            .find(|q| q.ns == ns && q.query == *query && !q.favourite)
        {
            existing.last_run = chrono::Utc::now();
            if conn.is_some() {
                existing.conn = conn;
            }
        } else {
            self.queries.push(SavedQuery {
                ns: ns.to_string(),
                query: query.clone(),
                conn,
                ..Default::default()
            });
        }
        let mut plain: Vec<usize> = self
            .queries
            .iter()
            .enumerate()
            .filter(|(_, q)| q.ns == ns && !q.favourite)
            .map(|(i, _)| i)
            .collect();
        if plain.len() > HISTORY_PER_NS {
            plain.sort_by_key(|&i| std::cmp::Reverse(self.queries[i].last_run));
            let drop: Vec<usize> = plain[HISTORY_PER_NS..].to_vec();
            let mut idx = 0;
            self.queries.retain(|_| {
                let keep = !drop.contains(&idx);
                idx += 1;
                keep
            });
        }
    }

    /// History for a namespace, newest first.
    pub fn history(&self, ns: &str) -> Vec<&SavedQuery> {
        let mut v: Vec<&SavedQuery> = self.queries.iter().filter(|q| q.ns == ns).collect();
        v.sort_by_key(|q| std::cmp::Reverse(q.last_run));
        v
    }
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}
fn connections_path() -> PathBuf {
    config_dir().join("connections.json")
}
fn queries_path() -> PathBuf {
    config_dir().join("queries.json")
}
fn pipelines_path() -> PathBuf {
    config_dir().join("pipelines.json")
}
pub fn keybindings_path() -> PathBuf {
    config_dir().join("keybindings.json")
}

fn read_json<T: Default + for<'a> Deserialize<'a>>(path: &PathBuf) -> T {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| tracing::warn!("{}: {e}; using defaults", path.display()))
            .unwrap_or_default(),
        Err(_) => T::default(),
    }
}

/// Atomic write: tmp + rename, so a crash mid-write never leaves a torn file.
pub fn write_atomic(path: &PathBuf, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T) {
    let body = serde_json::to_string_pretty(value).unwrap_or_default();
    if let Err(e) = write_atomic(path, body.as_bytes()) {
        tracing::warn!("failed to save {}: {e}", path.display());
    }
}

pub fn load() -> Config {
    let mut settings: Settings = read_json(&config_path());
    // keybindings.json is the hand-editable source; it wins over config.json.
    let kb: BTreeMap<String, String> = read_json(&keybindings_path());
    if !kb.is_empty() {
        settings.keybindings = kb;
    }
    Config {
        settings,
        connections: read_json(&connections_path()),
        queries: read_json(&queries_path()),
        pipelines: read_json(&pipelines_path()),
    }
}

pub fn save(cfg: &Config) {
    write_json(&config_path(), &cfg.settings);
    write_json(&connections_path(), &cfg.connections);
    write_json(&queries_path(), &cfg.queries);
    write_json(&pipelines_path(), &cfg.pipelines);
    write_json(&keybindings_path(), &cfg.settings.keybindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_are_ignored() {
        let s: Settings =
            serde_json::from_str(r#"{"page_size": 50, "bogus": 1, "theme": "dark"}"#).unwrap();
        assert_eq!(s.page_size, 50);
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.max_time_ms, 60_000);
    }

    #[test]
    fn history_dedupes_and_trims() {
        let mut cfg = Config::default();
        let q = Query {
            filter: "{a: 1}".into(),
            ..Default::default()
        };
        cfg.record_query("db.c", &q, None);
        cfg.record_query("db.c", &q, None);
        assert_eq!(cfg.queries.len(), 1);
        for i in 0..(HISTORY_PER_NS + 5) {
            cfg.record_query(
                "db.c",
                &Query {
                    filter: format!("{{x: {i}}}"),
                    ..Default::default()
                },
                None,
            );
        }
        assert_eq!(cfg.history("db.c").len(), HISTORY_PER_NS);
    }

    #[test]
    fn imports_compass_and_own_formats() {
        let compass = r#"{"type":"Compass Connections","version":{"$numberInt":"1"},"connections":[
          {"id":"6f1d1b9e-2b8e-4c9a-9f1a-1b2c3d4e5f60","connectionOptions":{"connectionString":"mongodb://u:s3cret@h:27017/?authSource=admin",
           "sshTunnel":{"host":"bastion","port":2222,"username":"me","identityKeyFile":"/home/me/.ssh/id"}},
           "favorite":{"name":"Prod","color":"color5"},"lastUsed":"2026-01-02T03:04:05.000Z"}]}"#;
        let got = import_profiles(compass).unwrap();
        assert_eq!(got.len(), 1);
        let p = &got[0].profile;
        assert_eq!(p.name, "Prod");
        assert_eq!(p.colour.as_deref(), Some("#3584e4"));
        assert_eq!(p.uri, "mongodb://u@h:27017/?authSource=admin");
        assert_eq!(got[0].password.as_deref(), Some("s3cret"));
        assert_eq!(p.ssh.as_ref().unwrap().port, 2222);
        assert!(p.favourite);

        let own = export_profiles(&got);
        let back = import_profiles(&own).unwrap();
        assert_eq!(back, got);
        assert!(import_profiles("[1]").is_err());
        assert!(import_profiles("{}").is_err());
    }

    #[test]
    fn export_omits_password_unless_given() {
        let e = ProfileExport::default();
        assert!(!export_profiles(&[e]).contains("password"));
    }

    #[test]
    fn editor_argv_splits() {
        let s = Settings {
            editor_command: "code -w".into(),
            ..Default::default()
        };
        assert_eq!(s.editor_argv(), vec!["code", "-w"]);
    }
}
