//! AI features by shelling out to a local CLI (`claude -p`, `codex exec`, or a
//! custom command): natural language → query / pipeline, an explanation of an
//! explain plan, and index suggestions. The prompt goes on stdin; the reply is
//! extracted from the CLI's output and parsed with `ejson`. Nothing here touches
//! widgets or runs what it produces — results are handed back for review.
use crate::config::{AiSettings, Query};
use crate::mongo::ejson::{self, Mode};
use crate::mongo::schema::{Field, Schema, Values};
use anyhow::{Context, Result, anyhow};
use bson::{Bson, Document};
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(120);
/// Fields listed in the schema summary.
pub const MAX_SCHEMA_LINES: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Claude,
    Codex,
    Custom {
        argv: Vec<String>,
        json_result: bool,
    },
}

impl Backend {
    pub fn from_settings(s: &AiSettings) -> std::result::Result<Self, String> {
        match s.backend.as_str() {
            "codex" => Ok(Backend::Codex),
            "custom" => {
                if s.custom_argv.is_empty() {
                    Err("No custom AI command set (Settings › Editor › AI backend)".into())
                } else {
                    Ok(Backend::Custom {
                        argv: s.custom_argv.clone(),
                        json_result: s.custom_json_result,
                    })
                }
            }
            _ => Ok(Backend::Claude),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        match self {
            // No tools (they only add context), no session file per request.
            Backend::Claude => [
                "claude",
                "-p",
                "--tools",
                "",
                "--no-session-persistence",
                "--output-format",
                "json",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            Backend::Codex => ["codex", "exec", "--skip-git-repo-check", "-"]
                .into_iter()
                .map(String::from)
                .collect(),
            Backend::Custom { argv, .. } => argv.clone(),
        }
    }

    /// Whether stdout is claude-style `{"result": "..."}` JSON.
    pub fn json_result(&self) -> bool {
        match self {
            Backend::Claude => true,
            Backend::Codex => false,
            Backend::Custom { json_result, .. } => *json_result,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Backend::Claude => "claude".into(),
            Backend::Codex => "codex".into(),
            Backend::Custom { argv, .. } => argv.first().cloned().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Query,
    Pipeline,
    ExplainPlan,
    IndexSuggest,
}

impl Task {
    pub fn title(self) -> &'static str {
        match self {
            Task::Query => "Generate query",
            Task::Pipeline => "Generate pipeline",
            Task::ExplainPlan => "Explain the plan",
            Task::IndexSuggest => "Suggest indexes",
        }
    }

    /// Whether the user's request text is required (the other tasks have a
    /// default request and the text only refines it).
    pub fn needs_text(self) -> bool {
        matches!(self, Task::Query | Task::Pipeline)
    }

    fn contract(self) -> &'static str {
        match self {
            Task::Query => {
                "Task: turn the request into a MongoDB find. Respond with ONLY one JSON object of \
                 this exact shape (omit nothing, use {} / null when not needed):\n\
                 {\"filter\": {}, \"project\": {}, \"sort\": {}, \"limit\": null, \"skip\": null}\n\
                 Use MongoDB query operators ($gt, $in, $regex, $elemMatch…). Dates as \
                 {\"$date\": \"ISO-8601\"}, ObjectIds as {\"$oid\": \"hex\"}."
            }
            Task::Pipeline => {
                "Task: turn the request into a MongoDB aggregation pipeline. Respond with ONLY \
                 one JSON object of this exact shape:\n\
                 {\"pipeline\": [ {\"$match\": {}}, ... ]}\n\
                 One object per stage, in order. Dates as {\"$date\": \"ISO-8601\"}, ObjectIds \
                 as {\"$oid\": \"hex\"}. Never use $out or $merge."
            }
            Task::ExplainPlan => {
                "Task: explain the explain-plan output below to a developer: which plan was \
                 chosen, whether it used an index or scanned the collection, how many documents \
                 and keys were examined versus returned, sorts done in memory, and what would \
                 make it faster. Respond with ONLY one JSON object of this exact shape:\n\
                 {\"explanation\": \"plain text, a few short paragraphs\"}"
            }
            Task::IndexSuggest => {
                "Task: suggest indexes for this collection that would help the query / workload \
                 described, skipping indexes that already exist or are redundant with them. \
                 Respond with ONLY one JSON object of this exact shape:\n\
                 {\"indexes\": [ {\"keys\": {\"field\": 1}, \"options\": {}, \"reason\": \"why\"} ]}\n\
                 Key values are 1, -1, \"text\", \"2dsphere\" or \"hashed\"; options may hold \
                 unique, sparse, expireAfterSeconds, partialFilterExpression. Fewer, better \
                 indexes beat many; an empty list is a valid answer."
            }
        }
    }
}

/// Everything a prompt is built from.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub task: Option<Task>,
    /// `db.coll`
    pub namespace: String,
    /// `path: Type (80%), Type (20%)` lines (see `schema_lines`).
    pub schema: Vec<String>,
    /// Extra labelled context: existing indexes, the explain output, the
    /// current query…
    pub context: Vec<(String, String)>,
    pub request: String,
}

/// The schema summary handed to the model: one line per field, optionally with
/// a few sample values.
pub fn schema_lines(schema: &Schema, include_values: bool, max: usize) -> Vec<String> {
    schema
        .flatten()
        .into_iter()
        .take(max)
        .map(|f| {
            let mut line = format!("{}: {}", f.path, f.types_text());
            if include_values && let Some(v) = sample_values(f) {
                line.push_str(&format!("  e.g. {v}"));
            }
            line
        })
        .collect()
}

fn sample_values(f: &Field) -> Option<String> {
    let t = f.main_type()?;
    match &t.values {
        Values::Strings(map) => {
            let mut top: Vec<(&String, &usize)> = map.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let vals: Vec<String> = top
                .iter()
                .take(3)
                .map(|(s, _)| format!("{:?}", ejson::truncate(s, 40)))
                .collect();
            (!vals.is_empty()).then(|| vals.join(", "))
        }
        Values::Numbers(n) if !n.is_empty() => {
            let min = n.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = n.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            Some(format!("{min} … {max}"))
        }
        Values::Dates(d) if !d.is_empty() => {
            let min = *d.iter().min()?;
            let max = *d.iter().max()?;
            Some(format!(
                "{} … {}",
                crate::mongo::schema::date_label(min),
                crate::mongo::schema::date_label(max)
            ))
        }
        Values::Booleans { t, f } => Some(format!("true ×{t}, false ×{f}")),
        _ => None,
    }
}

pub fn build_prompt(r: &Request) -> String {
    let task = r.task.unwrap_or(Task::Query);
    let mut p = String::new();
    p.push_str(
        "You are a MongoDB expert assisting inside a database GUI. Respond with ONLY the JSON \
         object asked for — no prose, no markdown fences, no comments. Use MongoDB Extended \
         JSON where a value needs a type.\n\n",
    );
    p.push_str(task.contract());
    p.push_str("\n\nCollection: ");
    p.push_str(&r.namespace);
    p.push('\n');
    if !r.schema.is_empty() {
        p.push_str("\nFields (path: type (share of documents)):\n");
        for l in &r.schema {
            p.push_str(l);
            p.push('\n');
        }
    }
    for (label, text) in &r.context {
        if text.trim().is_empty() {
            continue;
        }
        p.push_str(&format!("\n{label}:\n{}\n", text.trim_end()));
    }
    p.push_str("\nRequest: ");
    p.push_str(if r.request.trim().is_empty() {
        match task {
            Task::ExplainPlan => "explain this plan",
            Task::IndexSuggest => "suggest indexes for this collection and its current query",
            _ => "",
        }
    } else {
        r.request.trim()
    });
    p.push('\n');
    p
}

// ----- running the CLI -------------------------------------------------------

/// Run the backend with `prompt` on stdin; returns its stdout. The child is
/// killed if the future is dropped (cancel) or after `timeout`.
pub async fn run(backend: &Backend, prompt: String, timeout: Duration) -> Result<String> {
    let argv = backend.argv();
    let Some(program) = argv.first() else {
        return Err(anyhow!("empty AI command"));
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Run outside any project directory so `claude` / `codex` do not pull in
    // a CLAUDE.md / AGENTS.md as context; a nested `claude` also refuses to
    // start inside a Claude Code session.
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CLAUDECODE");
    cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run `{program}` (is it installed and on PATH?)"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Write on its own task: a large prompt could fill the pipe before the
        // child starts reading.
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| anyhow!("`{program}` did not answer within {} s", timeout.as_secs()))?
        .context("waiting for the AI command")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        let detail = if tail.trim().is_empty() {
            stdout.lines().last().unwrap_or("").to_string()
        } else {
            tail
        };
        return Err(anyhow!(
            "`{program}` exited with {}: {}",
            out.status,
            ejson::truncate(detail.trim(), 300)
        ));
    }
    Ok(stdout)
}

// ----- response extraction ---------------------------------------------------

/// The model's text out of the CLI's stdout: claude's `{"result": "..."}`
/// (or a stream array whose last `type == "result"` entry holds it), else the
/// raw output.
pub fn extract_result(stdout: &str, json_result: bool) -> Result<String> {
    if !json_result {
        return Ok(stdout.to_string());
    }
    let trimmed = stdout.trim();
    let v: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        // Some versions print a line per event.
        Err(_) => {
            let last = trimmed
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .rfind(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"));
            match last {
                Some(v) => v,
                None => return Ok(stdout.to_string()),
            }
        }
    };
    let obj = match &v {
        serde_json::Value::Array(items) => items
            .iter()
            .rev()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("result"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        _ => v.clone(),
    };
    if obj.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
        let msg = obj
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("the AI command reported an error");
        return Err(anyhow!("{msg}"));
    }
    match obj.get("result") {
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(other) if !other.is_null() => Ok(other.to_string()),
        _ => Ok(stdout.to_string()),
    }
}

/// The first balanced `{…}` or `[…]` in `text`, skipping any prose or ```json
/// fence around it. Strings and escapes are honoured.
pub fn extract_json(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find(['{', '['])?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexSuggestion {
    pub keys: Document,
    pub options: Document,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Query(Query),
    /// Pipeline text, one stage per element, as the Aggregations page loads it.
    Pipeline(String),
    Explanation(String),
    Indexes(Vec<IndexSuggestion>),
}

/// Parse the model's answer for `task`.
pub fn parse_response(task: Task, text: &str) -> std::result::Result<Response, String> {
    let json = extract_json(text);
    let doc = json.and_then(|j| ejson::parse_document(j).ok());
    match task {
        Task::Query => {
            let doc = doc.ok_or_else(|| no_object(text))?;
            let sub = |k: &str| -> String {
                match doc.get(k) {
                    Some(Bson::Document(d)) if !d.is_empty() => ejson::compact(d, Mode::Relaxed),
                    _ => String::new(),
                }
            };
            let num = |k: &str| -> u64 {
                match doc.get(k) {
                    Some(Bson::Int32(n)) => (*n).max(0) as u64,
                    Some(Bson::Int64(n)) => (*n).max(0) as u64,
                    Some(Bson::Double(n)) => n.max(0.0) as u64,
                    _ => 0,
                }
            };
            if !matches!(doc.get("filter"), Some(Bson::Document(_)))
                && !matches!(doc.get("pipeline"), Some(Bson::Array(_)))
            {
                // A bare filter object is the most common "wrong" shape.
                return Ok(Response::Query(Query {
                    filter: ejson::compact(&doc, Mode::Relaxed),
                    ..Query::default()
                }));
            }
            Ok(Response::Query(Query {
                filter: sub("filter"),
                project: sub("project"),
                sort: sub("sort"),
                skip: num("skip"),
                limit: num("limit"),
                ..Query::default()
            }))
        }
        Task::Pipeline => {
            let stages: Vec<Document> = match &doc {
                Some(d) => match d.get("pipeline") {
                    Some(Bson::Array(a)) => {
                        a.iter().filter_map(|s| s.as_document().cloned()).collect()
                    }
                    _ => vec![d.clone()],
                },
                // A bare array of stages.
                None => match json.and_then(|j| ejson::parse_documents(j).ok()) {
                    Some(v) if !v.is_empty() => v,
                    _ => return Err(no_object(text)),
                },
            };
            let p = crate::mongo::pipeline::Pipeline::from_documents(&stages)?;
            Ok(Response::Pipeline(p.to_text()))
        }
        Task::ExplainPlan => {
            let explanation = match &doc {
                Some(d) => match d.get_str("explanation") {
                    Ok(s) => s.to_string(),
                    Err(_) => text.trim().to_string(),
                },
                None => text.trim().to_string(),
            };
            if explanation.is_empty() {
                return Err("the model returned an empty explanation".into());
            }
            Ok(Response::Explanation(explanation))
        }
        Task::IndexSuggest => {
            let doc = doc.ok_or_else(|| no_object(text))?;
            let items: Vec<Document> = match doc.get("indexes") {
                Some(Bson::Array(a)) => a.iter().filter_map(|s| s.as_document().cloned()).collect(),
                _ if doc.contains_key("keys") => vec![doc.clone()],
                _ => return Err(no_object(text)),
            };
            let mut out = Vec::new();
            for it in items {
                let Ok(keys) = it.get_document("keys") else {
                    continue;
                };
                if keys.is_empty() {
                    continue;
                }
                out.push(IndexSuggestion {
                    keys: keys.clone(),
                    options: it.get_document("options").cloned().unwrap_or_default(),
                    reason: it.get_str("reason").unwrap_or("").to_string(),
                });
            }
            Ok(Response::Indexes(out))
        }
    }
}

fn no_object(text: &str) -> String {
    format!(
        "the model did not answer with a JSON object: {}",
        ejson::truncate(text.trim(), 200)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn backend_from_settings() {
        let mut s = AiSettings::default();
        assert_eq!(Backend::from_settings(&s), Ok(Backend::Claude));
        s.backend = "codex".into();
        assert_eq!(Backend::from_settings(&s), Ok(Backend::Codex));
        s.backend = "custom".into();
        assert!(Backend::from_settings(&s).is_err());
        s.custom_argv = vec!["my-llm".into(), "--json".into()];
        let b = Backend::from_settings(&s).unwrap();
        assert_eq!(b.argv(), vec!["my-llm", "--json"]);
        assert!(!b.json_result());
        assert_eq!(b.name(), "my-llm");
        assert_eq!(Backend::Claude.argv()[0], "claude");
        assert!(Backend::Claude.json_result());
    }

    #[test]
    fn prompt_has_contract_schema_context_and_request() {
        let r = Request {
            task: Some(Task::Query),
            namespace: "shop.orders".into(),
            schema: vec!["_id: ObjectId (100%)".into(), "total: Double (98%)".into()],
            context: vec![("Existing indexes".into(), "_id_: { _id: 1 }".into())],
            request: "orders over 100 dollars".into(),
        };
        let p = build_prompt(&r);
        assert!(p.contains("\"filter\": {}"));
        assert!(p.contains("Collection: shop.orders"));
        assert!(p.contains("total: Double (98%)"));
        assert!(p.contains("Existing indexes:\n_id_: { _id: 1 }"));
        assert!(p.ends_with("Request: orders over 100 dollars\n"));
        // Default requests for the optional-text tasks.
        let r2 = Request {
            task: Some(Task::ExplainPlan),
            ..Request::default()
        };
        assert!(build_prompt(&r2).ends_with("Request: explain this plan\n"));
    }

    #[test]
    fn schema_lines_with_values() {
        let docs = vec![
            doc! { "name": "ann", "n": 3, "ok": true },
            doc! { "name": "bob", "n": 7, "ok": false },
            doc! { "name": "ann", "n": 5 },
        ];
        let s = crate::mongo::schema::analyze(&docs);
        let plain = schema_lines(&s, false, 60);
        assert!(plain.iter().any(|l| l.starts_with("name: String")));
        assert!(!plain.iter().any(|l| l.contains("e.g.")));
        let with = schema_lines(&s, true, 60);
        let name = with.iter().find(|l| l.starts_with("name:")).unwrap();
        assert!(name.contains("e.g. \"ann\", \"bob\""), "{name}");
        let n = with.iter().find(|l| l.starts_with("n:")).unwrap();
        assert!(n.contains("3 … 7"), "{n}");
        let ok = with.iter().find(|l| l.starts_with("ok:")).unwrap();
        assert!(ok.contains("true ×1, false ×1"), "{ok}");
        assert_eq!(schema_lines(&s, true, 1).len(), 1);
    }

    #[test]
    fn extract_result_claude_shapes() {
        let single =
            r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"filter\":{}}"}"#;
        assert_eq!(extract_result(single, true).unwrap(), "{\"filter\":{}}");
        let stream = r#"[{"type":"system"},{"type":"assistant"},{"type":"result","result":"hi"}]"#;
        assert_eq!(extract_result(stream, true).unwrap(), "hi");
        let lines = "{\"type\":\"system\"}\n{\"type\":\"result\",\"result\":\"per line\"}\n";
        assert_eq!(extract_result(lines, true).unwrap(), "per line");
        let err = r#"{"type":"result","is_error":true,"result":"not logged in"}"#;
        assert!(
            extract_result(err, true)
                .unwrap_err()
                .to_string()
                .contains("not logged in")
        );
        // Not JSON at all: pass through.
        assert_eq!(
            extract_result("plain {\"a\":1}", true).unwrap(),
            "plain {\"a\":1}"
        );
        assert_eq!(extract_result("anything", false).unwrap(), "anything");
    }

    #[test]
    fn extract_json_skips_prose_and_fences() {
        assert_eq!(extract_json("{\"a\": 1}"), Some("{\"a\": 1}"));
        let fenced = "Here you go:\n```json\n{\"a\": {\"b\": \"}\"}}\n```\nDone.";
        assert_eq!(extract_json(fenced), Some("{\"a\": {\"b\": \"}\"}}"));
        assert_eq!(extract_json("x [1, [2]] y"), Some("[1, [2]]"));
        assert_eq!(extract_json("no json"), None);
        assert_eq!(extract_json("{\"unterminated\": 1"), None);
        assert_eq!(
            extract_json(r#"{"s": "esc\"aped}"}"#),
            Some(r#"{"s": "esc\"aped}"}"#)
        );
    }

    #[test]
    fn parse_query() {
        let text = r#"{"filter": {"age": {"$gt": 30}}, "project": {}, "sort": {"age": -1}, "limit": 10, "skip": null}"#;
        let Response::Query(q) = parse_response(Task::Query, text).unwrap() else {
            panic!()
        };
        assert_eq!(q.filter, "{\"age\":{\"$gt\":30}}");
        assert_eq!(q.project, "");
        assert_eq!(q.sort, "{\"age\":-1}");
        assert_eq!(q.limit, 10);
        assert_eq!(q.skip, 0);
        // A bare filter is accepted as the filter.
        let Response::Query(q) = parse_response(Task::Query, "{ age: 3 }").unwrap() else {
            panic!()
        };
        assert_eq!(q.filter, "{\"age\":3}");
        assert!(parse_response(Task::Query, "sorry, no").is_err());
    }

    #[test]
    fn parse_pipeline() {
        let text = "```json\n{\"pipeline\": [{\"$match\": {\"a\": 1}}, {\"$group\": {\"_id\": \"$b\", \"n\": {\"$sum\": 1}}}]}\n```";
        let Response::Pipeline(p) = parse_response(Task::Pipeline, text).unwrap() else {
            panic!()
        };
        assert!(p.contains("$match"), "{p}");
        assert!(p.contains("$group"), "{p}");
        // A bare array of stages.
        let Response::Pipeline(p) = parse_response(Task::Pipeline, "[{ $limit: 5 }]").unwrap()
        else {
            panic!()
        };
        assert!(p.contains("$limit"));
        assert!(parse_response(Task::Pipeline, "").is_err());
    }

    #[test]
    fn parse_explanation_and_indexes() {
        let Response::Explanation(e) =
            parse_response(Task::ExplainPlan, "{\"explanation\": \"It scans.\"}").unwrap()
        else {
            panic!()
        };
        assert_eq!(e, "It scans.");
        // Plain prose is fine for an explanation.
        let Response::Explanation(e) =
            parse_response(Task::ExplainPlan, "  The plan is a COLLSCAN.\n").unwrap()
        else {
            panic!()
        };
        assert_eq!(e, "The plan is a COLLSCAN.");
        let text = r#"{"indexes": [{"keys": {"age": 1, "name": -1}, "options": {"unique": true}, "reason": "the sort"}, {"keys": {}}]}"#;
        let Response::Indexes(ix) = parse_response(Task::IndexSuggest, text).unwrap() else {
            panic!()
        };
        assert_eq!(ix.len(), 1);
        assert_eq!(ix[0].keys, doc! { "age": 1, "name": -1 });
        assert_eq!(ix[0].options, doc! { "unique": true });
        assert_eq!(ix[0].reason, "the sort");
        let Response::Indexes(none) =
            parse_response(Task::IndexSuggest, "{\"indexes\": []}").unwrap()
        else {
            panic!()
        };
        assert!(none.is_empty());
    }
}
