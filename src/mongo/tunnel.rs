//! SSH port forwarding: `ssh -N -L 127.0.0.1:<local>:<host>:<port> …` per host
//! in the connection string, started before the driver connects and killed
//! when the connection is dropped. Key-based auth only (agent or identity
//! file): there is no TTY to type a passphrase into.
use crate::config::SshTunnel;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Read;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A running `ssh` child; killed on drop.
#[derive(Debug)]
pub struct Tunnel {
    child: Child,
    /// (remote host:port, local port) for every forwarded host.
    pub forwards: Vec<(String, u16)>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> Result<u16> {
    let l = TcpListener::bind(("127.0.0.1", 0)).context("no free local port")?;
    Ok(l.local_addr()?.port())
}

/// `host[:port]` -> (host, port).
fn split_host(h: &str) -> (String, u16) {
    match h.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.starts_with('[') => (
            host.trim_matches(['[', ']']).to_string(),
            port.parse().unwrap_or(27017),
        ),
        _ => (h.to_string(), 27017),
    }
}

/// The argv for `ssh`, without spawning it (unit-tested).
pub fn ssh_argv(cfg: &SshTunnel, forwards: &[(String, u16)]) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_string(),
        "-N".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ServerAliveInterval=30".into(),
        "-p".into(),
        cfg.port.to_string(),
    ];
    if !cfg.identity_file.trim().is_empty() {
        argv.push("-i".into());
        argv.push(cfg.identity_file.trim().to_string());
    }
    for (remote, local) in forwards {
        let (host, port) = split_host(remote);
        argv.push("-L".into());
        argv.push(format!("127.0.0.1:{local}:{host}:{port}"));
    }
    argv.push(cfg.target());
    argv
}

/// Rewrite the hosts of a URI to their local forwards. A single-host tunnel
/// also pins `directConnection=true` (unless already set) so replica-set
/// discovery does not send the driver to hosts it cannot reach.
pub fn rewrite_uri(uri: &str, forwards: &[(String, u16)]) -> Result<String> {
    let mut parts = super::profile::UriParts::parse(uri).map_err(|e| anyhow!(e))?;
    if parts.srv {
        bail!("SSH tunnels need explicit hosts, not mongodb+srv://");
    }
    parts.hosts = forwards
        .iter()
        .map(|(_, local)| format!("127.0.0.1:{local}"))
        .collect();
    if forwards.len() == 1 && parts.get("directConnection").is_none() {
        parts.set_bool("directConnection", Some(true));
    }
    Ok(parts.to_uri())
}

/// Start the tunnel and wait until every local port accepts connections (or
/// ssh gives up). Blocking; run it on tokio's blocking pool.
pub fn open(cfg: &SshTunnel, uri: &str) -> Result<(Tunnel, String)> {
    let parts = super::profile::UriParts::parse(uri).map_err(|e| anyhow!(e))?;
    if parts.srv {
        bail!("SSH tunnels need explicit hosts, not mongodb+srv://");
    }
    let mut forwards = Vec::new();
    for h in &parts.hosts {
        forwards.push((h.clone(), free_port()?));
    }
    let argv = ssh_argv(cfg, &forwards);
    tracing::info!("ssh tunnel: {}", argv.join(" "));
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ssh")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            bail!(
                "ssh exited ({status}) before the tunnel came up: {}",
                err.trim()
            );
        }
        let all_up = forwards
            .iter()
            .all(|(_, p)| std::net::TcpStream::connect(("127.0.0.1", *p)).is_ok());
        if all_up {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("ssh tunnel did not come up within 20 s");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let local_uri = rewrite_uri(uri, &forwards)?;
    Ok((Tunnel { child, forwards }, local_uri))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_and_rewrite() {
        let cfg = SshTunnel {
            host: "bastion".into(),
            port: 2222,
            username: "me".into(),
            identity_file: "~/.ssh/id".into(),
        };
        let fw = vec![("db1:27017".to_string(), 40001), ("db2".to_string(), 40002)];
        let argv = ssh_argv(&cfg, &fw);
        assert_eq!(argv[0], "ssh");
        assert!(argv.contains(&"-L".to_string()));
        assert!(argv.contains(&"127.0.0.1:40001:db1:27017".to_string()));
        assert!(argv.contains(&"127.0.0.1:40002:db2:27017".to_string()));
        assert_eq!(argv.last().unwrap(), "me@bastion");
        assert_eq!(
            rewrite_uri("mongodb://u@db1:27017,db2/app?replicaSet=rs", &fw).unwrap(),
            "mongodb://u@127.0.0.1:40001,127.0.0.1:40002/app?replicaSet=rs"
        );
        assert_eq!(
            rewrite_uri("mongodb://db1", &fw[..1]).unwrap(),
            "mongodb://127.0.0.1:40001/?directConnection=true"
        );
        assert!(rewrite_uri("mongodb+srv://c.example.net", &fw).is_err());
    }
}
