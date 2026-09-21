//! The `:` command line: a small grammar, parsed and completed here without any
//! widgets so it can be unit-tested. `App::run_command` executes an `Invocation`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgSpec {
    None,
    Db,
    Coll,
    Conn,
    Setting,
    /// Free text: the rest of the line, verbatim.
    Text,
    OneOf(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: ArgSpec,
    pub help: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "db",
        aliases: &["use"],
        args: ArgSpec::Db,
        help: "Select a database",
    },
    Command {
        name: "coll",
        aliases: &["c", "collection"],
        args: ArgSpec::Coll,
        help: "Open a collection",
    },
    Command {
        name: "conn",
        aliases: &["connect"],
        args: ArgSpec::Conn,
        help: "Connect to a saved connection",
    },
    Command {
        name: "disconnect",
        aliases: &[],
        args: ArgSpec::None,
        help: "Disconnect the current connection",
    },
    Command {
        name: "mkdb",
        aliases: &["createdb"],
        args: ArgSpec::Text,
        help: "Create a database (asks for its first collection)",
    },
    Command {
        name: "mkcoll",
        aliases: &["createcoll", "mkcollection"],
        args: ArgSpec::Text,
        help: "Create a collection in the current database",
    },
    Command {
        name: "drop",
        aliases: &[],
        args: ArgSpec::Coll,
        help: "Drop a collection (db.coll) or database (db)",
    },
    Command {
        name: "rename",
        aliases: &["mv"],
        args: ArgSpec::Text,
        help: "Rename the current collection",
    },
    Command {
        name: "update",
        aliases: &["updatemany"],
        args: ArgSpec::Text,
        help: "Bulk update the documents matching the query",
    },
    Command {
        name: "delete",
        aliases: &["deletemany"],
        args: ArgSpec::None,
        help: "Bulk delete the documents matching the query",
    },
    Command {
        name: "queries",
        aliases: &["favourites", "favorites"],
        args: ArgSpec::None,
        help: "My Queries: saved favourites",
    },
    Command {
        name: "find",
        aliases: &["f"],
        args: ArgSpec::Text,
        help: "Run a filter in the current collection",
    },
    Command {
        name: "export",
        aliases: &[],
        args: ArgSpec::OneOf(&["json", "csv", "language"]),
        help: "Export documents, or the query/pipeline as driver code",
    },
    Command {
        name: "import",
        aliases: &[],
        args: ArgSpec::Text,
        help: "Import JSON or CSV (optionally: the file path)",
    },
    Command {
        name: "agg",
        aliases: &["aggregate"],
        args: ArgSpec::None,
        help: "Open the aggregations page",
    },
    Command {
        name: "index",
        aliases: &["indexes"],
        args: ArgSpec::None,
        help: "Open the indexes tab",
    },
    Command {
        name: "explain",
        aliases: &[],
        args: ArgSpec::None,
        help: "Explain the query (or the pipeline, on the aggregations page)",
    },
    Command {
        name: "schema",
        aliases: &[],
        args: ArgSpec::None,
        help: "Open the schema tab",
    },
    Command {
        name: "validation",
        aliases: &["validate", "rules"],
        args: ArgSpec::None,
        help: "Open the validation tab",
    },
    Command {
        name: "shell",
        aliases: &["sh"],
        args: ArgSpec::None,
        help: "Toggle mongosh",
    },
    Command {
        name: "ai",
        aliases: &[],
        args: ArgSpec::Text,
        help: "Ask AI: a query (or pipeline / plan explanation / indexes, per page) from a description",
    },
    Command {
        name: "perf",
        aliases: &["performance", "stats"],
        args: ArgSpec::None,
        help: "Open the Performance page of the current connection",
    },
    Command {
        name: "set",
        aliases: &[],
        args: ArgSpec::Setting,
        help: "Change a setting: readonly, pagesize, view, theme, maxtime",
    },
    Command {
        name: "view",
        aliases: &[],
        args: ArgSpec::OneOf(&["list", "json", "table"]),
        help: "Documents view",
    },
    Command {
        name: "page",
        aliases: &["p"],
        args: ArgSpec::Text,
        help: "Go to page N",
    },
    Command {
        name: "settings",
        aliases: &["prefs", "preferences"],
        args: ArgSpec::None,
        help: "Open settings",
    },
    Command {
        name: "help",
        aliases: &["h", "?"],
        args: ArgSpec::None,
        help: "Show keybindings",
    },
    Command {
        name: "quit",
        aliases: &["q", "exit"],
        args: ArgSpec::None,
        help: "Quit",
    },
];

pub const SETTINGS_KEYS: &[(&str, &str)] = &[
    ("readonly", "on | off"),
    ("pagesize", "25 | 50 | 75 | 100"),
    ("view", "list | json | table"),
    ("theme", "system | light | dark"),
    ("maxtime", "milliseconds"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub name: &'static str,
    pub args: Vec<String>,
    /// Everything after the command, untrimmed of internal spaces.
    pub rest: String,
}

pub fn lookup(word: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .find(|c| c.name == word || c.aliases.contains(&word))
}

pub fn parse(line: &str) -> Result<Invocation, String> {
    let line = line.trim_start_matches(':').trim();
    if line.is_empty() {
        return Err("empty command".into());
    }
    // A bare number is `:page N`, like vim's `:N`.
    if line.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Invocation {
            name: "page",
            args: vec![line.to_string()],
            rest: line.to_string(),
        });
    }
    let (word, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    };
    let cmd = lookup(word).ok_or_else(|| format!("unknown command `{word}`"))?;
    let args = match cmd.args {
        ArgSpec::Text => {
            if rest.is_empty() {
                vec![]
            } else {
                vec![rest.to_string()]
            }
        }
        _ => rest.split_whitespace().map(str::to_string).collect(),
    };
    Ok(Invocation {
        name: cmd.name,
        args,
        rest: rest.to_string(),
    })
}

/// What the completer needs to know about the app.
pub trait CompletionCtx {
    fn databases(&self) -> Vec<String>;
    fn collections(&self) -> Vec<String>;
    fn connections(&self) -> Vec<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The full replacement line (without the leading `:`).
    pub line: String,
    pub label: String,
    pub hint: String,
}

pub fn complete(line: &str, ctx: &dyn CompletionCtx) -> Vec<Completion> {
    let line = line.trim_start_matches(':');
    let trimmed = line.trim_start();
    let has_space = trimmed.contains(char::is_whitespace);
    if !has_space {
        return COMMANDS
            .iter()
            .filter(|c| {
                c.name.starts_with(trimmed)
                    || c.aliases
                        .iter()
                        .any(|a| a.starts_with(trimmed) && !trimmed.is_empty())
            })
            .map(|c| Completion {
                line: format!("{} ", c.name),
                label: c.name.to_string(),
                hint: c.help.to_string(),
            })
            .collect();
    }
    let (word, rest) = trimmed.split_once(char::is_whitespace).unwrap();
    let rest = rest.trim_start();
    let Some(cmd) = lookup(word) else {
        return vec![];
    };
    let candidates: Vec<(String, String)> = match cmd.args {
        ArgSpec::Db => ctx
            .databases()
            .into_iter()
            .map(|d| (d, "database".into()))
            .collect(),
        ArgSpec::Coll => ctx
            .collections()
            .into_iter()
            .map(|c| (c, "collection".into()))
            .collect(),
        ArgSpec::Conn => ctx
            .connections()
            .into_iter()
            .map(|c| (c, "connection".into()))
            .collect(),
        ArgSpec::OneOf(opts) => opts
            .iter()
            .map(|o| (o.to_string(), String::new()))
            .collect(),
        ArgSpec::Setting => {
            if let Some((key, val)) = rest.split_once(char::is_whitespace) {
                let val = val.trim();
                let opts: &[&str] = match key {
                    "readonly" => &["on", "off"],
                    "pagesize" => &["25", "50", "75", "100"],
                    "view" => &["list", "json", "table"],
                    "theme" => &["system", "light", "dark"],
                    _ => &[],
                };
                return opts
                    .iter()
                    .filter(|o| o.starts_with(val))
                    .map(|o| Completion {
                        line: format!("{} {key} {o}", cmd.name),
                        label: o.to_string(),
                        hint: String::new(),
                    })
                    .collect();
            }
            SETTINGS_KEYS
                .iter()
                .map(|(k, h)| (k.to_string(), h.to_string()))
                .collect()
        }
        ArgSpec::None | ArgSpec::Text => vec![],
    };
    candidates
        .into_iter()
        .filter(|(c, _)| c.to_lowercase().starts_with(&rest.to_lowercase()))
        .map(|(c, hint)| Completion {
            line: format!("{} {}", cmd.name, quote_if_needed(&c)),
            label: c,
            hint,
        })
        .collect()
}

fn quote_if_needed(s: &str) -> String {
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ctx;
    impl CompletionCtx for Ctx {
        fn databases(&self) -> Vec<String> {
            vec!["admin".into(), "app".into(), "local".into()]
        }
        fn collections(&self) -> Vec<String> {
            vec!["people".into(), "orders".into()]
        }
        fn connections(&self) -> Vec<String> {
            vec!["local".into()]
        }
    }

    #[test]
    fn parses() {
        assert_eq!(parse(":db app").unwrap().name, "db");
        assert_eq!(parse("use app").unwrap().args, vec!["app"]);
        let f = parse("find { a: 1, b: 'x y' }").unwrap();
        assert_eq!(f.args, vec!["{ a: 1, b: 'x y' }"]);
        assert_eq!(
            parse("42").unwrap(),
            Invocation {
                name: "page",
                args: vec!["42".into()],
                rest: "42".into()
            }
        );
        assert!(parse("bogus").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn completes_commands_and_args() {
        let names: Vec<String> = complete("d", &Ctx).into_iter().map(|c| c.label).collect();
        assert_eq!(names, vec!["db", "disconnect", "drop", "delete"]);
        let dbs: Vec<String> = complete("db a", &Ctx)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(dbs, vec!["admin", "app"]);
        let all: Vec<String> = complete("db ", &Ctx).into_iter().map(|c| c.line).collect();
        assert_eq!(all, vec!["db admin", "db app", "db local"]);
        let v: Vec<String> = complete("set view j", &Ctx)
            .into_iter()
            .map(|c| c.line)
            .collect();
        assert_eq!(v, vec!["set view json"]);
        assert!(complete("nope x", &Ctx).is_empty());
    }
}
