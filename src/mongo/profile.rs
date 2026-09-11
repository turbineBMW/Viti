//! Connection-string helpers: the password is kept out of the saved profile and
//! reinserted only when connecting, and `UriParts` lets the advanced connection
//! form and the URI entry edit the same thing from either side.

/// Split `scheme://user:pass@rest` into the URI without its password and the password.
pub fn split_password(uri: &str) -> (String, Option<String>) {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return (uri.to_string(), None);
    };
    let Some(at) = userinfo_end(rest) else {
        return (uri.to_string(), None);
    };
    let (userinfo, hosts) = rest.split_at(at);
    let Some((user, pass)) = userinfo.split_once(':') else {
        return (uri.to_string(), None);
    };
    let pass = percent_decode(pass);
    (format!("{scheme}://{user}{hosts}"), Some(pass))
}

/// Reinsert a password after the user in `scheme://user@rest`.
pub fn with_password(uri: &str, password: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_string();
    };
    match userinfo_end(rest) {
        Some(at) => {
            let (user, hosts) = rest.split_at(at);
            let user = user.split_once(':').map(|(u, _)| u).unwrap_or(user);
            format!("{scheme}://{user}:{}{hosts}", percent_encode(password))
        }
        None => uri.to_string(),
    }
}

/// Index of the `@` that ends the userinfo, if any. Only the part before the
/// first `/` or `?` is a candidate, so an `@` inside an option value never
/// counts.
fn userinfo_end(rest: &str) -> Option<usize> {
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    rest[..authority_end].rfind('@')
}

/// The username in the URI, if any.
pub fn username(uri: &str) -> Option<String> {
    let rest = uri.split_once("://")?.1;
    let at = userinfo_end(rest)?;
    let userinfo = &rest[..at];
    let user = userinfo.split_once(':').map(|(u, _)| u).unwrap_or(userinfo);
    (!user.is_empty()).then(|| percent_decode(user))
}

/// A short label for an unnamed connection: `host[:port]` of the first host.
pub fn host_label(uri: &str) -> String {
    let rest = uri.split_once("://").map(|(_, r)| r).unwrap_or(uri);
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let hosts = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    hosts.split(',').next().unwrap_or(hosts).to_string()
}

/// Option keys whose values are secrets.
const SECRET_OPTIONS: &[&str] = &["tlscertificatekeyfilepassword", "proxypassword"];

/// The URI with the password (and any secret option values) replaced by `*****`.
pub fn redact_uri(uri: &str) -> String {
    let (bare, pw) = split_password(uri);
    let mut out = match pw {
        Some(_) => with_password(&bare, "*****").replace("%2A%2A%2A%2A%2A", "*****"),
        None => bare,
    };
    if let Some(q) = out.find('?') {
        let (head, query) = out.split_at(q + 1);
        let redacted: Vec<String> = query
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) if SECRET_OPTIONS.contains(&k.to_ascii_lowercase().as_str()) => {
                    format!("{k}=*****")
                }
                _ => kv.to_string(),
            })
            .collect();
        out = format!("{head}{}", redacted.join("&"));
    }
    out
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A connection string taken apart. Options keep their original key spelling
/// and order so a round trip changes nothing the user did not touch; lookups
/// are case-insensitive, as the driver's are.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UriParts {
    pub srv: bool,
    pub hosts: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// The path segment: the default (auth) database.
    pub database: Option<String>,
    pub options: Vec<(String, String)>,
}

impl UriParts {
    pub fn parse(uri: &str) -> Result<Self, String> {
        let uri = uri.trim();
        let (scheme, rest) = uri
            .split_once("://")
            .ok_or_else(|| "missing mongodb:// scheme".to_string())?;
        let srv = match scheme {
            "mongodb" => false,
            "mongodb+srv" => true,
            other => return Err(format!("unknown scheme {other}://")),
        };
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        let (userinfo, hosts) = match authority.rfind('@') {
            Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
                None => ((!ui.is_empty()).then(|| percent_decode(ui)), None),
            },
            None => (None, None),
        };
        let hosts: Vec<String> = hosts
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect();
        if hosts.is_empty() {
            return Err("no host".into());
        }
        let (path, query) = match tail.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (tail, None),
        };
        let database = path
            .strip_prefix('/')
            .filter(|d| !d.is_empty())
            .map(percent_decode);
        let options = query
            .unwrap_or("")
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) => (k.to_string(), percent_decode(v)),
                None => (kv.to_string(), String::new()),
            })
            .collect();
        Ok(Self {
            srv,
            hosts,
            username,
            password,
            database,
            options,
        })
    }

    pub fn to_uri(&self) -> String {
        let mut s = String::from(if self.srv {
            "mongodb+srv://"
        } else {
            "mongodb://"
        });
        if let Some(u) = &self.username {
            s.push_str(&percent_encode(u));
            if let Some(p) = &self.password {
                s.push(':');
                s.push_str(&percent_encode(p));
            }
            s.push('@');
        }
        s.push_str(&self.hosts.join(","));
        let has_db = self.database.as_deref().is_some_and(|d| !d.is_empty());
        if has_db || !self.options.is_empty() {
            s.push('/');
        }
        if let Some(d) = &self.database
            && !d.is_empty()
        {
            s.push_str(&percent_encode(d));
        }
        if !self.options.is_empty() {
            s.push('?');
            let q: Vec<String> = self
                .options
                .iter()
                .map(|(k, v)| format!("{k}={}", percent_encode_option(v)))
                .collect();
            s.push_str(&q.join("&"));
        }
        s
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)?.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        }
    }

    /// Set an option (replacing any existing spelling), or remove it when
    /// `value` is `None` or empty.
    pub fn set(&mut self, key: &str, value: Option<&str>) {
        let pos = self
            .options
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(key));
        match (pos, value.filter(|v| !v.is_empty())) {
            (Some(i), Some(v)) => self.options[i].1 = v.to_string(),
            (Some(i), None) => {
                self.options.remove(i);
            }
            (None, Some(v)) => self.options.push((key.to_string(), v.to_string())),
            (None, None) => {}
        }
    }

    pub fn set_bool(&mut self, key: &str, value: Option<bool>) {
        self.set(key, value.map(|b| if b { "true" } else { "false" }));
    }

    pub fn remove(&mut self, key: &str) {
        self.set(key, None);
    }
}

/// Option values keep `/`, `:`, `,`, `=` and `{}` readable (authMechanismProperties,
/// file paths); everything else that is not unreserved is escaped.
fn percent_encode_option(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':'
            | b','
            | b'='
            | b'{'
            | b'}'
            | b'+' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_rejoin() {
        let (bare, pw) = split_password("mongodb://root:p%40ss@localhost:27017/?authSource=admin");
        assert_eq!(bare, "mongodb://root@localhost:27017/?authSource=admin");
        assert_eq!(pw.as_deref(), Some("p@ss"));
        assert_eq!(
            with_password(&bare, "p@ss"),
            "mongodb://root:p%40ss@localhost:27017/?authSource=admin"
        );
        assert_eq!(username(&bare).as_deref(), Some("root"));
    }

    #[test]
    fn no_userinfo() {
        let (bare, pw) = split_password("mongodb://localhost");
        assert_eq!(bare, "mongodb://localhost");
        assert!(pw.is_none());
        assert_eq!(with_password(&bare, "x"), "mongodb://localhost");
        assert_eq!(username(&bare), None);
    }

    #[test]
    fn at_in_option_is_not_userinfo() {
        let uri = "mongodb://localhost/?appName=x@y";
        assert_eq!(split_password(uri), (uri.to_string(), None));
        assert_eq!(username(uri), None);
        assert_eq!(host_label(uri), "localhost");
    }

    #[test]
    fn host_labels() {
        assert_eq!(
            host_label("mongodb://u:p@db.example.com:27018/app?x=1"),
            "db.example.com:27018"
        );
        assert_eq!(host_label("mongodb+srv://c.example.net"), "c.example.net");
        assert_eq!(host_label("mongodb://a:1,b:2/db"), "a:1");
    }

    #[test]
    fn redact() {
        assert_eq!(
            redact_uri("mongodb+srv://u:secret@c.example.net/db"),
            "mongodb+srv://u:*****@c.example.net/db"
        );
        assert_eq!(
            redact_uri("mongodb://h/?tls=true&tlsCertificateKeyFilePassword=pw&x=1"),
            "mongodb://h/?tls=true&tlsCertificateKeyFilePassword=*****&x=1"
        );
    }

    #[test]
    fn parts_round_trip() {
        let uri = "mongodb://u%40x:p%3Aw@a:1,b:2/admin?replicaSet=rs0&tls=true&authMechanismProperties=SERVICE_NAME:mongodb,CANONICALIZE_HOST_NAME:true";
        let p = UriParts::parse(uri).unwrap();
        assert!(!p.srv);
        assert_eq!(p.hosts, vec!["a:1", "b:2"]);
        assert_eq!(p.username.as_deref(), Some("u@x"));
        assert_eq!(p.password.as_deref(), Some("p:w"));
        assert_eq!(p.database.as_deref(), Some("admin"));
        assert_eq!(p.get("REPLICASET"), Some("rs0"));
        assert_eq!(p.get_bool("tls"), Some(true));
        assert_eq!(p.to_uri(), uri);
    }

    #[test]
    fn parts_minimal_and_set() {
        let mut p = UriParts::parse("mongodb+srv://cluster0.example.net").unwrap();
        assert!(p.srv);
        assert_eq!(p.to_uri(), "mongodb+srv://cluster0.example.net");
        p.set("readPreference", Some("secondary"));
        p.set_bool("directConnection", Some(true));
        assert_eq!(
            p.to_uri(),
            "mongodb+srv://cluster0.example.net/?readPreference=secondary&directConnection=true"
        );
        p.set("READPREFERENCE", None);
        p.set("directconnection", Some("false"));
        assert_eq!(
            p.options,
            vec![("directConnection".to_string(), "false".to_string())]
        );
        p.database = Some("app".into());
        p.options.clear();
        assert_eq!(p.to_uri(), "mongodb+srv://cluster0.example.net/app");
        assert!(UriParts::parse("http://x").is_err());
        assert!(UriParts::parse("mongodb://").is_err());
    }
}
