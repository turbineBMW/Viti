//! Completions for the query bar entries: field names collected from the
//! documents on screen, query operators (`$gt`, `$in`, …) and the mongosh
//! constructors (`ObjectId("…")`, `ISODate("…")`, …) that the EJSON parser
//! accepts. Pure: `complete()` looks at the text around the caret and answers
//! with replacements; `ui::completer` shows them in a popover.
use crate::mongo::ejson;
use bson::{Bson, Document};
use std::collections::HashSet;
use std::ops::Range;

/// A field seen in a document: its dotted path and the BSON type it had the
/// first time it was seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub path: String,
    pub kind: &'static str,
}

/// How deep `merge_fields` descends and how many paths it keeps in total.
const MAX_DEPTH: usize = 6;
const MAX_FIELDS: usize = 800;

/// Add every dotted path in `docs` that `fields` does not know yet, in first
/// appearance order. Array elements that are documents contribute their keys
/// under the array's own path (`items.sku`), as Compass does.
pub fn merge_fields(fields: &mut Vec<Field>, docs: &[Document]) {
    let mut seen: HashSet<String> = fields.iter().map(|f| f.path.clone()).collect();
    fn walk(
        doc: &Document,
        prefix: &str,
        depth: usize,
        seen: &mut HashSet<String>,
        out: &mut Vec<Field>,
    ) {
        for (k, v) in doc {
            if out.len() >= MAX_FIELDS {
                return;
            }
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if seen.insert(path.clone()) {
                out.push(Field {
                    path: path.clone(),
                    kind: ejson::type_name(v),
                });
            }
            if depth + 1 >= MAX_DEPTH {
                continue;
            }
            match v {
                Bson::Document(d) => walk(d, &path, depth + 1, seen, out),
                Bson::Array(items) => {
                    for item in items {
                        if let Bson::Document(d) = item {
                            walk(d, &path, depth + 1, seen, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for d in docs {
        walk(d, "", 0, &mut seen, fields);
    }
}

/// Which entry is being completed: the filter takes values and operators, the
/// other entries (project, sort, hint) only take field names as keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Filter,
    Fields,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// What the list shows.
    pub label: String,
    /// Dimmed text after the label: the field's type, "operator", …
    pub detail: String,
    /// Byte range of the entry text that `insert` replaces.
    pub replace: Range<usize>,
    pub insert: String,
    /// Byte offset inside `insert` where the caret goes (`ObjectId("|")`).
    pub caret: usize,
}

struct Ctx {
    /// Where the typed token starts (after any opening quote).
    start: usize,
    prefix: String,
    quote: Option<char>,
    position: Position,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Position {
    /// After `{` or `,`: a field name or an operator.
    Key { operator_doc: bool },
    /// After `:` or `[`: a value.
    Value,
    /// Inside a constructor call, a string, or nowhere useful.
    None,
}

pub const OPERATORS: &[(&str, &str)] = &[
    ("$eq", "equals"),
    ("$ne", "not equal"),
    ("$gt", "greater than"),
    ("$gte", "greater or equal"),
    ("$lt", "less than"),
    ("$lte", "less or equal"),
    ("$in", "in array"),
    ("$nin", "not in array"),
    ("$exists", "field exists"),
    ("$type", "BSON type"),
    ("$regex", "regular expression"),
    ("$options", "regex options"),
    ("$and", "all of"),
    ("$or", "any of"),
    ("$nor", "none of"),
    ("$not", "negate"),
    ("$expr", "aggregation expression"),
    ("$mod", "modulo"),
    ("$size", "array length"),
    ("$all", "contains all"),
    ("$elemMatch", "array element matches"),
    ("$text", "text search"),
    ("$where", "JavaScript"),
    ("$jsonSchema", "JSON schema"),
    ("$geoWithin", "geo within"),
    ("$geoIntersects", "geo intersects"),
    ("$near", "geo near"),
    ("$nearSphere", "geo near (sphere)"),
    ("$bitsAllSet", "bits all set"),
    ("$bitsAnySet", "bits any set"),
    ("$bitsAllClear", "bits all clear"),
    ("$bitsAnyClear", "bits any clear"),
];

/// (label, inserted text, caret offset, detail)
pub const CONSTRUCTORS: &[(&str, &str, usize, &str)] = &[
    (
        "ObjectId(\"…\")",
        "ObjectId(\"\")",
        10,
        "12-byte id from hex",
    ),
    (
        "ISODate(\"…\")",
        "ISODate(\"\")",
        9,
        "date, e.g. 2024-01-31T00:00:00Z",
    ),
    ("Date()", "Date()", 6, "now"),
    ("NumberLong(…)", "NumberLong()", 11, "64-bit integer"),
    ("NumberInt(…)", "NumberInt()", 10, "32-bit integer"),
    (
        "NumberDecimal(\"…\")",
        "NumberDecimal(\"\")",
        15,
        "Decimal128",
    ),
    ("Timestamp(…, …)", "Timestamp(, )", 10, "seconds, increment"),
    ("UUID(\"…\")", "UUID(\"\")", 6, "binary subtype 4"),
    ("BinData(…, \"…\")", "BinData(, \"\")", 8, "subtype, base64"),
    ("RegExp(\"…\")", "RegExp(\"\")", 8, "pattern, flags"),
    ("MinKey()", "MinKey()", 8, "sorts before everything"),
    ("MaxKey()", "MaxKey()", 8, "sorts after everything"),
    ("true", "true", 4, "boolean"),
    ("false", "false", 5, "boolean"),
    ("null", "null", 4, "null"),
];

fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '.'
}

fn context(text: &str, cursor: usize) -> Ctx {
    let cursor = cursor.min(text.len());
    let head = &text[..cursor];
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_token_char(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(cursor);
    let prefix = head[start..].to_string();
    let before = &head[..start];
    let (quote, before) = match before.chars().next_back() {
        Some(q @ ('"' | '\'')) => (Some(q), &before[..before.len() - q.len_utf8()]),
        _ => (None, before),
    };
    let trimmed = before.trim_end();
    let position = match trimmed.chars().next_back() {
        None | Some('{') | Some(',') => {
            let operator_doc =
                trimmed.ends_with('{') && trimmed[..trimmed.len() - 1].trim_end().ends_with(':');
            Position::Key { operator_doc }
        }
        Some(':') | Some('[') => Position::Value,
        _ => Position::None,
    };
    // A quote that is not the one opening this token means we're inside a
    // string somewhere else (`"a b`), so don't guess.
    let position = if quote.is_none() && unbalanced_quote(before) {
        Position::None
    } else {
        position
    };
    Ctx {
        start,
        prefix,
        quote,
        position,
    }
}

fn unbalanced_quote(s: &str) -> bool {
    let mut open: Option<char> = None;
    let mut escaped = false;
    for c in s.chars() {
        match (open, c) {
            (_, '\\') if open.is_some() => escaped = !escaped,
            (Some(q), c) if c == q && !escaped => open = None,
            (None, '"' | '\'') => open = Some(c),
            _ => escaped = false,
        }
        if c != '\\' {
            escaped = false;
        }
    }
    open.is_some()
}

/// A key the loose parser can take unquoted.
fn bare_key_ok(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(|c: char| c.is_ascii_digit())
        && !path.contains('.')
        && path
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn rank(candidate: &str, prefix: &str) -> Option<usize> {
    if prefix.is_empty() {
        return Some(0);
    }
    let c = candidate.to_lowercase();
    let p = prefix.to_lowercase();
    if c.starts_with(&p) {
        Some(0)
    } else if c.split('.').any(|seg| seg.starts_with(&p)) {
        Some(1)
    } else if c.contains(&p) {
        Some(2)
    } else {
        None
    }
}

/// Completions for the caret at byte `cursor` of `text`. Empty when there is
/// nothing sensible to offer (inside a string, after `(`, an exact match…).
pub fn complete(text: &str, cursor: usize, fields: &[Field], kind: Kind) -> Vec<Completion> {
    let ctx = context(text, cursor);
    let cursor = cursor.min(text.len());
    let rest = &text[cursor..];
    let mut out = Vec::new();
    match ctx.position {
        Position::None => {}
        Position::Key { operator_doc } => {
            let mut ops = Vec::new();
            let mut flds = Vec::new();
            if kind == Kind::Filter && (operator_doc || ctx.prefix.starts_with('$')) {
                for (op, detail) in OPERATORS {
                    if let Some(r) = rank(op, &ctx.prefix) {
                        ops.push((r, key_completion(&ctx, cursor, rest, op, detail)));
                    }
                }
            }
            if !ctx.prefix.starts_with('$') {
                for f in fields {
                    if let Some(r) = rank(&f.path, &ctx.prefix) {
                        flds.push((r, key_completion(&ctx, cursor, rest, &f.path, f.kind)));
                    }
                }
            }
            ops.sort_by_key(|(r, _)| *r);
            flds.sort_by_key(|(r, _)| *r);
            let (first, second) = if operator_doc {
                (ops, flds)
            } else {
                (flds, ops)
            };
            out.extend(first.into_iter().map(|(_, c)| c));
            out.extend(second.into_iter().map(|(_, c)| c));
        }
        Position::Value => {
            if kind != Kind::Filter {
                return out;
            }
            if ctx.prefix.starts_with('$') {
                // `$expr: { $gt: ["$a", "$b"] }`: field references as strings.
                let bare = &ctx.prefix[1..];
                for f in fields {
                    if rank(&f.path, bare).is_some() {
                        let q = ctx.quote.unwrap_or('"');
                        let mut insert = String::new();
                        if ctx.quote.is_none() {
                            insert.push(q);
                        }
                        insert.push('$');
                        insert.push_str(&f.path);
                        if !rest.starts_with(q) {
                            insert.push(q);
                        }
                        let caret = insert.len();
                        out.push(Completion {
                            label: format!("\"${}\"", f.path),
                            detail: format!("{} · field reference", f.kind),
                            replace: ctx.start..cursor,
                            insert,
                            caret,
                        });
                    }
                }
                return out;
            }
            if ctx.quote.is_some() {
                return out;
            }
            for (label, insert, caret, detail) in CONSTRUCTORS {
                if rank(insert, &ctx.prefix).is_some() && *insert != ctx.prefix {
                    out.push(Completion {
                        label: (*label).into(),
                        detail: (*detail).into(),
                        replace: ctx.start..cursor,
                        insert: (*insert).into(),
                        caret: *caret,
                    });
                }
            }
        }
    }
    // One candidate that is exactly what is already typed is no help.
    if out.len() == 1 && out[0].insert.trim_end_matches(": ") == ctx.prefix {
        out.clear();
    }
    out
}

/// A field or operator in key position: quoted when the parser needs it,
/// followed by `: ` unless a colon is already there.
fn key_completion(ctx: &Ctx, cursor: usize, rest: &str, key: &str, detail: &str) -> Completion {
    let mut insert = String::new();
    match ctx.quote {
        Some(q) => {
            insert.push_str(key);
            if !rest.starts_with(q) {
                insert.push(q);
            }
        }
        None if bare_key_ok(key) => insert.push_str(key),
        None => {
            insert.push('"');
            insert.push_str(key);
            insert.push('"');
        }
    }
    // Skip over a closing quote the entry already has.
    let mut after = rest;
    if let Some(r) = ctx.quote.and_then(|q| after.strip_prefix(q)) {
        after = r;
    }
    if !after.trim_start().starts_with(':') {
        insert.push_str(": ");
    }
    let caret = insert.len();
    Completion {
        label: key.to_string(),
        detail: detail.to_string(),
        replace: ctx.start..cursor,
        insert,
        caret,
    }
}

/// Apply a completion: the new text and the byte offset of the caret.
pub fn apply(text: &str, c: &Completion) -> (String, usize) {
    let mut out = String::with_capacity(text.len() + c.insert.len());
    out.push_str(&text[..c.replace.start]);
    out.push_str(&c.insert);
    let caret = out.len() - c.insert.len() + c.caret;
    out.push_str(&text[c.replace.end..]);
    (out, caret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn fields() -> Vec<Field> {
        let docs = vec![
            doc! { "_id": bson::oid::ObjectId::new(), "name": "Ann", "age": 30,
            "address": { "city": "Oslo", "zip": "0150" },
            "items": [ { "sku": "a" }, { "sku": "b", "qty": 2 } ] },
            doc! { "_id": 2, "name": "Bob", "email": "b@x" },
        ];
        let mut f = Vec::new();
        merge_fields(&mut f, &docs);
        f
    }

    fn labels(cs: &[Completion]) -> Vec<&str> {
        cs.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn collects_paths_in_order_with_nested_and_array_docs() {
        let f = fields();
        let paths: Vec<&str> = f.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "_id",
                "name",
                "age",
                "address",
                "address.city",
                "address.zip",
                "items",
                "items.sku",
                "items.qty",
                "email"
            ]
        );
        assert_eq!(f[0].kind, "ObjectId");
        assert_eq!(f[6].kind, "Array");
    }

    #[test]
    fn merge_is_a_union() {
        let mut f = fields();
        let n = f.len();
        merge_fields(&mut f, &[doc! { "name": 1, "extra": true }]);
        assert_eq!(f.len(), n + 1);
        assert_eq!(f[n].path, "extra");
    }

    #[test]
    fn key_position_offers_fields() {
        let f = fields();
        let t = "{ na";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["name"]);
        assert_eq!(apply(t, &cs[0]), ("{ name: ".to_string(), 8));
    }

    #[test]
    fn empty_prefix_after_brace_lists_everything() {
        let f = fields();
        let cs = complete("{ ", 2, &f, Kind::Filter);
        assert_eq!(cs.len(), f.len());
        assert_eq!(cs[0].label, "_id");
    }

    #[test]
    fn dotted_paths_get_quoted() {
        let f = fields();
        let t = "{ city";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["address.city"]);
        assert_eq!(apply(t, &cs[0]).0, "{ \"address.city\": ");
    }

    #[test]
    fn quoted_key_keeps_the_quote_style() {
        let f = fields();
        let t = "{ 'addr";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["address", "address.city", "address.zip"]);
        assert_eq!(apply(t, &cs[1]).0, "{ 'address.city': ");
        // Existing closing quote and colon are reused.
        let t = "{ 'addr': 1 }";
        let cs = complete(t, 7, &f, Kind::Filter);
        assert_eq!(apply(t, &cs[0]).0, "{ 'address': 1 }");
    }

    #[test]
    fn operators_after_dollar_and_inside_operator_docs() {
        let f = fields();
        let t = "{ age: { $g";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["$gt", "$gte", "$geoWithin", "$geoIntersects"]);
        assert_eq!(apply(t, &cs[0]).0, "{ age: { $gt: ");
        let t = "{ age: { ";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(cs[0].label, "$eq");
        assert!(cs.iter().any(|c| c.label == "name"));
    }

    #[test]
    fn value_position_offers_constructors() {
        let f = fields();
        let t = "{ _id: Ob";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["ObjectId(\"…\")"]);
        assert_eq!(apply(t, &cs[0]), ("{ _id: ObjectId(\"\")".to_string(), 17));
        let t = "{ _id: ";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(cs[0].label, "ObjectId(\"…\")");
        assert!(cs.iter().any(|c| c.label == "ISODate(\"…\")"));
    }

    #[test]
    fn value_position_field_references() {
        let f = fields();
        let t = "{ $expr: { $gt: [\"$ag";
        let cs = complete(t, t.len(), &f, Kind::Filter);
        assert_eq!(labels(&cs), ["\"$age\""]);
        assert_eq!(apply(t, &cs[0]).0, "{ $expr: { $gt: [\"$age\"");
    }

    #[test]
    fn nothing_inside_calls_or_strings() {
        let f = fields();
        let t = "{ _id: ObjectId(\"5";
        assert!(complete(t, t.len(), &f, Kind::Filter).is_empty());
        let t = "{ name: 'An";
        assert!(complete(t, t.len(), &f, Kind::Filter).is_empty());
        let t = "{ name: \"a b\", ";
        assert!(!complete(t, t.len(), &f, Kind::Filter).is_empty());
    }

    #[test]
    fn exact_match_is_dropped() {
        let f = fields();
        let t = "{ name";
        assert!(complete(t, t.len(), &f, Kind::Filter).is_empty());
    }

    #[test]
    fn fields_kind_has_no_values_or_operators() {
        let f = fields();
        assert!(complete("{ name: ", 8, &f, Kind::Fields).is_empty());
        assert!(complete("{ $", 3, &f, Kind::Fields).is_empty());
        let cs = complete("{ ag", 4, &f, Kind::Fields);
        assert_eq!(labels(&cs), ["age"]);
    }

    #[test]
    fn caret_in_the_middle_replaces_only_the_token() {
        let f = fields();
        let t = "{ na, age: 1 }";
        let cs = complete(t, 4, &f, Kind::Filter);
        assert_eq!(labels(&cs), ["name"]);
        assert_eq!(apply(t, &cs[0]).0, "{ name: , age: 1 }");
    }
}
