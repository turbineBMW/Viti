//! The driver side. `Connections` lives in `App` on the GTK thread; the
//! `Client`s it hands out are `Clone + Send` and used from tokio tasks.
pub mod ejson;
pub mod explain;
pub mod export;
pub mod import;
pub mod ops;
pub mod pipeline;
pub mod profile;
pub mod schema;
pub mod tunnel;
pub mod update_preview;
pub mod validation;

pub use ops::ServerInfo;

use crate::config::ConnectionProfile;
use anyhow::{Context, Result};
use mongodb::Client;
use mongodb::options::ClientOptions;
use std::time::Duration;

pub type ConnectionId = uuid::Uuid;

/// A live connection, owned by the GTK thread. Dropping it closes the SSH
/// tunnel, if there is one.
pub struct Conn {
    pub id: ConnectionId,
    pub client: Client,
    pub profile: ConnectionProfile,
    pub server: ServerInfo,
    pub tunnel: Option<tunnel::Tunnel>,
}

/// Everything `connect` produces; `Send` so it can ride the event bus.
#[derive(Debug)]
pub struct Connected {
    pub client: Client,
    pub server: ServerInfo,
    pub tunnel: Option<tunnel::Tunnel>,
}

/// Build the client (through an SSH tunnel when the profile has one) and
/// prove it works with a `hello`. Runs on tokio.
pub async fn connect(
    profile: &ConnectionProfile,
    secrets: &crate::secrets::ProfileSecrets,
) -> Result<Connected> {
    let mut uri = match &secrets.password {
        Some(pw) if !pw.is_empty() => profile::with_password(&profile.uri, pw),
        _ => profile.uri.clone(),
    };
    if let Some(kp) = secrets
        .tls_key_password
        .as_deref()
        .filter(|s| !s.is_empty())
        && let Ok(mut parts) = profile::UriParts::parse(&uri)
    {
        parts.set("tlsCertificateKeyFilePassword", Some(kp));
        uri = parts.to_uri();
    }
    let redacted = profile::redact_uri(&profile.uri);
    let (tunnel, uri) = match profile.ssh.clone().filter(|s| s.is_configured()) {
        Some(ssh) => {
            let uri2 = uri.clone();
            let (t, local) = tokio::task::spawn_blocking(move || tunnel::open(&ssh, &uri2))
                .await
                .context("tunnel task")?
                .with_context(|| format!("SSH tunnel for {redacted}"))?;
            (Some(t), local)
        }
        None => (None, uri),
    };
    let mut opts = ClientOptions::parse(&uri)
        .await
        .with_context(|| format!("invalid connection string {redacted}"))?;
    if opts.app_name.is_none() {
        opts.app_name = Some(format!("viti/{}", env!("CARGO_PKG_VERSION")));
    }
    if opts.server_selection_timeout.is_none() {
        opts.server_selection_timeout = Some(Duration::from_secs(10));
    }
    if opts.connect_timeout.is_none() {
        opts.connect_timeout = Some(Duration::from_secs(10));
    }
    let client = Client::with_options(opts).context("could not build client")?;
    let server = ops::server_info(&client)
        .await
        .with_context(|| format!("could not reach {redacted}"))?;
    Ok(Connected {
        client,
        server,
        tunnel,
    })
}
