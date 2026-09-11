//! Connection passwords: the secret-service keyring when available, else a
//! 0600 JSON file under the config dir. Keyed by profile uuid. All calls block
//! on IPC, so they run on tokio (see `mongo::connect`).
use crate::config::{SecretStore, config_dir};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

const SCHEMA: &str = "dev.turbinebmw.Viti.Connection";

/// The key under which a profile's TLS client-key password is stored.
pub fn tls_key_id(profile: Uuid) -> Uuid {
    Uuid::new_v5(&profile, b"tls-key-password")
}

/// Both secrets of a profile: the user password and the TLS key password.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileSecrets {
    pub password: Option<String>,
    pub tls_key_password: Option<String>,
}

pub fn get_all(store: SecretStore, id: Uuid) -> ProfileSecrets {
    ProfileSecrets {
        password: get(store, id).filter(|s| !s.is_empty()),
        tls_key_password: get(store, tls_key_id(id)).filter(|s| !s.is_empty()),
    }
}

/// Store both (deleting whichever is empty); returns any warning to surface.
pub fn set_all(store: SecretStore, id: Uuid, label: &str, s: &ProfileSecrets) -> Option<String> {
    let mut warn = None;
    for (key, value, what) in [
        (id, &s.password, ""),
        (tls_key_id(id), &s.tls_key_password, " (TLS key)"),
    ] {
        match value.as_deref().filter(|v| !v.is_empty()) {
            Some(v) => {
                if let Some(w) = set(store, key, &format!("{label}{what}"), v) {
                    warn = Some(w);
                }
            }
            None => delete(key),
        }
    }
    warn
}

pub fn delete_all(id: Uuid) {
    delete(id);
    delete(tls_key_id(id));
}

fn file_path() -> PathBuf {
    config_dir().join("secrets.json")
}

fn read_file() -> BTreeMap<String, String> {
    std::fs::read_to_string(file_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_file(map: &BTreeMap<String, String>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = file_path();
    crate::config::write_atomic(&path, serde_json::to_string_pretty(map)?.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn attrs(id: Uuid) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("xdg:schema".to_string(), SCHEMA.to_string());
    m.insert("profile".to_string(), id.to_string());
    m
}

fn keyring_get(id: Uuid) -> anyhow::Result<Option<String>> {
    let ss = secret_service::blocking::SecretService::connect(secret_service::EncryptionType::Dh)?;
    let attrs = attrs(id);
    let attrs: std::collections::HashMap<&str, &str> = attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let items = ss.search_items(attrs)?;
    let Some(item) = items.unlocked.first().or(items.locked.first()) else {
        return Ok(None);
    };
    item.unlock()?;
    Ok(Some(String::from_utf8(item.get_secret()?)?))
}

fn keyring_set(id: Uuid, label: &str, secret: &str) -> anyhow::Result<()> {
    let ss = secret_service::blocking::SecretService::connect(secret_service::EncryptionType::Dh)?;
    let collection = ss.get_default_collection()?;
    let attrs = attrs(id);
    let attrs: std::collections::HashMap<&str, &str> = attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    collection.create_item(
        &format!("Viti: {label}"),
        attrs,
        secret.as_bytes(),
        true,
        "text/plain",
    )?;
    Ok(())
}

fn keyring_delete(id: Uuid) -> anyhow::Result<()> {
    let ss = secret_service::blocking::SecretService::connect(secret_service::EncryptionType::Dh)?;
    let attrs = attrs(id);
    let attrs: std::collections::HashMap<&str, &str> = attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    for item in ss.search_items(attrs)?.unlocked {
        item.delete()?;
    }
    Ok(())
}

/// Look the password up in the configured store, falling back to the file when
/// the keyring is unreachable so an absent daemon never blocks connecting.
pub fn get(store: SecretStore, id: Uuid) -> Option<String> {
    match store {
        SecretStore::None => None,
        SecretStore::File => read_file().get(&id.to_string()).cloned(),
        SecretStore::Keyring => match keyring_get(id) {
            Ok(v) => v.or_else(|| read_file().get(&id.to_string()).cloned()),
            Err(e) => {
                tracing::warn!("keyring unavailable ({e}); reading secrets.json");
                read_file().get(&id.to_string()).cloned()
            }
        },
    }
}

/// Returns a warning to surface when the keyring was wanted but the file was used.
pub fn set(store: SecretStore, id: Uuid, label: &str, secret: &str) -> Option<String> {
    match store {
        SecretStore::None => None,
        SecretStore::File => {
            let mut m = read_file();
            m.insert(id.to_string(), secret.to_string());
            write_file(&m)
                .err()
                .map(|e| format!("could not write secrets.json: {e}"))
        }
        SecretStore::Keyring => match keyring_set(id, label, secret) {
            Ok(()) => None,
            Err(e) => {
                let mut m = read_file();
                m.insert(id.to_string(), secret.to_string());
                let _ = write_file(&m);
                Some(format!(
                    "keyring unavailable ({e}); password stored in secrets.json (0600)"
                ))
            }
        },
    }
}

pub fn delete(id: Uuid) {
    if let Err(e) = keyring_delete(id) {
        tracing::debug!("keyring delete: {e}");
    }
    let mut m = read_file();
    if m.remove(&id.to_string()).is_some() {
        let _ = write_file(&m);
    }
}
