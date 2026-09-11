//! Text <-> BSON. Two directions:
//!
//! * Output: pretty Extended JSON (relaxed or canonical) for editors and export.
//! * Input: a *loose* reader that accepts what people type in Compass and
//!   mongosh — unquoted keys, single quotes, trailing commas, `ObjectId("…")`,
//!   `ISODate("…")`/`new Date(…)`, `NumberLong(…)`, `/regex/flags`, comments —
//!   as well as strict Extended JSON. It is rewritten to strict JSON and then
//!   parsed by serde_json + bson's Extended JSON rules.
use bson::{Bson, Document};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Relaxed,
    Canonical,
}

pub fn pretty(doc: &Document, mode: Mode) -> String {
    let value = match mode {
        Mode::Relaxed => Bson::Document(doc.clone()).into_relaxed_extjson(),
        Mode::Canonical => Bson::Document(doc.clone()).into_canonical_extjson(),
    };
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

pub fn pretty_many(docs: &[Document], mode: Mode) -> String {
    let values: Vec<serde_json::Value> = docs
        .iter()
        .map(|d| match mode {
            Mode::Relaxed => Bson::Document(d.clone()).into_relaxed_extjson(),
            Mode::Canonical => Bson::Document(d.clone()).into_canonical_extjson(),
        })
        .collect();
    serde_json::to_string_pretty(&values).unwrap_or_default()
}

pub fn compact(doc: &Document, mode: Mode) -> String {
    let value = match mode {
        Mode::Relaxed => Bson::Document(doc.clone()).into_relaxed_extjson(),
        Mode::Canonical => Bson::Document(doc.clone()).into_canonical_extjson(),
    };
    serde_json::to_string(&value).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, col {}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

/// Parse loose or strict text into any BSON value.
pub fn parse_value(text: &str) -> Result<Bson, ParseError> {
    let strict = Rewriter::new(text).run()?;
    let value: serde_json::Value = serde_json::from_str(&strict).map_err(|e| {
        // Positions refer to the rewritten text; map them back as well as possible.
        let (line, col) = Rewriter::new(text)
            .position_for(e.line(), e.column())
            .unwrap_or((e.line(), e.column()));
        ParseError {
            line,
            col,
            msg: e
                .to_string()
                .split(" at line")
                .next()
                .unwrap_or("")
                .to_string(),
        }
    })?;
    Bson::try_from(value).map_err(|e| ParseError {
        line: 1,
        col: 1,
        msg: e.to_string(),
    })
}

pub fn parse_document(text: &str) -> Result<Document, ParseError> {
    match parse_value(text)? {
        Bson::Document(d) => Ok(d),
        other => Err(ParseError {
            line: 1,
            col: 1,
            msg: format!("expected a document, got {}", type_name(&other)),
        }),
    }
}

/// An empty/blank string is an empty document; otherwise parse.
pub fn parse_document_or_empty(text: &str) -> Result<Document, ParseError> {
    if text.trim().is_empty() {
        Ok(Document::new())
    } else {
        parse_document(text)
    }
}

pub fn parse_documents(text: &str) -> Result<Vec<Document>, ParseError> {
    match parse_value(text)? {
        Bson::Array(items) => items
            .into_iter()
            .map(|b| match b {
                Bson::Document(d) => Ok(d),
                other => Err(ParseError {
                    line: 1,
                    col: 1,
                    msg: format!("expected documents in the array, got {}", type_name(&other)),
                }),
            })
            .collect(),
        Bson::Document(d) => Ok(vec![d]),
        other => Err(ParseError {
            line: 1,
            col: 1,
            msg: format!("expected an array of documents, got {}", type_name(&other)),
        }),
    }
}

/// Human BSON type name, as Compass shows it.
pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "Double",
        Bson::String(_) => "String",
        Bson::Array(_) => "Array",
        Bson::Document(_) => "Object",
        Bson::Boolean(_) => "Boolean",
        Bson::Null => "Null",
        Bson::RegularExpression(_) => "RegExp",
        Bson::JavaScriptCode(_) => "Code",
        Bson::JavaScriptCodeWithScope(_) => "CodeWScope",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Timestamp(_) => "Timestamp",
        Bson::Binary(_) => "Binary",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Symbol(_) => "Symbol",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Undefined => "Undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

/// CSS class for colouring a value by type.
pub fn type_class(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => "number",
        Bson::String(_) | Bson::Symbol(_) => "string",
        Bson::Boolean(_) => "boolean",
        Bson::Null | Bson::Undefined => "null",
        Bson::ObjectId(_) => "objectid",
        Bson::DateTime(_) | Bson::Timestamp(_) => "date",
        _ => "other",
    }
}

/// One-line rendering of a value for cards and table cells.
pub fn summary(b: &Bson, max: usize) -> String {
    let s = match b {
        Bson::String(s) => format!("\"{s}\""),
        Bson::Document(d) => {
            if d.is_empty() {
                "{}".into()
            } else {
                format!("{{ {} fields }}", d.len())
            }
        }
        Bson::Array(a) => format!("[ {} elements ]", a.len()),
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(d) => d
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| d.timestamp_millis().to_string()),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Boolean(v) => v.to_string(),
        Bson::Int32(v) => v.to_string(),
        Bson::Int64(v) => v.to_string(),
        Bson::Double(v) => {
            if v.fract() == 0.0 && v.is_finite() {
                format!("{v:.1}")
            } else {
                v.to_string()
            }
        }
        Bson::Decimal128(v) => v.to_string(),
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        Bson::Binary(bin) => format!(
            "Binary({} bytes, subtype {:?})",
            bin.bytes.len(),
            bin.subtype
        ),
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        other => compact_value(other),
    };
    truncate(&s, max)
}

fn compact_value(b: &Bson) -> String {
    serde_json::to_string(&b.clone().into_relaxed_extjson()).unwrap_or_default()
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Display an `_id` the way mongosh prints it.
pub fn id_display(b: &Bson) -> String {
    summary(b, 80)
}

// ----------------------------------------------------------------------------
// Loose -> strict rewriter

struct Rewriter<'a> {
    src: &'a str,
    chars: Vec<char>,
    pos: usize,
    out: String,
    /// (output byte offset, input char index) checkpoints for error mapping.
    marks: Vec<(usize, usize)>,
}

impl<'a> Rewriter<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            chars: src.chars().collect(),
            pos: 0,
            out: String::with_capacity(src.len() + 16),
            marks: Vec::new(),
        }
    }

    fn run(mut self) -> Result<String, ParseError> {
        self.skip_ws();
        if self.pos >= self.chars.len() {
            return Err(self.error("empty input"));
        }
        self.value()?;
        self.skip_ws();
        if self.pos < self.chars.len() {
            return Err(self.error("unexpected trailing characters"));
        }
        Ok(self.out)
    }

    /// Map a (line, col) in the strict output back to the loose input.
    fn position_for(self, line: usize, col: usize) -> Option<(usize, usize)> {
        let me = Rewriter::new(self.src);
        let strict = match me.run_with_marks() {
            Ok(v) => v,
            Err(_) => return None,
        };
        let (out, marks) = strict;
        // byte offset of (line, col) in out
        let mut offset = 0;
        for (i, l) in out.split('\n').enumerate() {
            if i + 1 == line {
                offset += col.saturating_sub(1).min(l.len());
                break;
            }
            offset += l.len() + 1;
        }
        let (_, in_idx) = marks
            .iter()
            .rev()
            .find(|(o, _)| *o <= offset)
            .copied()
            .unwrap_or((0, 0));
        Some(line_col(&self.chars_of(), in_idx))
    }

    fn chars_of(&self) -> Vec<char> {
        self.src.chars().collect()
    }

    fn run_with_marks(mut self) -> Result<(String, Vec<(usize, usize)>), ParseError> {
        self.skip_ws();
        self.value()?;
        Ok((self.out, self.marks))
    }

    fn error(&self, msg: &str) -> ParseError {
        let (line, col) = line_col(&self.chars, self.pos);
        ParseError {
            line,
            col,
            msg: msg.to_string(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn mark(&mut self) {
        self.marks.push((self.out.len(), self.pos));
    }

    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some('/') if self.chars.get(self.pos + 1) == Some(&'/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                Some('/') if self.chars.get(self.pos + 1) == Some(&'*') => {
                    self.pos += 2;
                    while self.pos < self.chars.len() {
                        if self.peek() == Some('*') && self.chars.get(self.pos + 1) == Some(&'/') {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn value(&mut self) -> Result<(), ParseError> {
        self.mark();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') | Some('\'') => {
                let s = self.string()?;
                self.push_str(&s);
                Ok(())
            }
            Some('/') => self.regex(),
            Some(c) if c == '-' || c == '+' || c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_alphabetic() || c == '_' || c == '$' => self.word(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn push_str(&mut self, s: &str) {
        self.out
            .push_str(&serde_json::to_string(s).unwrap_or_default());
    }

    fn object(&mut self) -> Result<(), ParseError> {
        self.pos += 1;
        self.out.push('{');
        let mut first = true;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    self.out.push('}');
                    return Ok(());
                }
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                None => return Err(self.error("unterminated object")),
                _ => {}
            }
            if !first {
                self.out.push(',');
            }
            first = false;
            self.mark();
            let key = match self.peek() {
                Some('"') | Some('\'') => self.string()?,
                Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => self.ident(),
                _ => return Err(self.error("expected a key")),
            };
            self.push_str(&key);
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(self.error("expected ':' after key"));
            }
            self.pos += 1;
            self.out.push(':');
            self.skip_ws();
            self.value()?;
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some('}') => {}
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self) -> Result<(), ParseError> {
        self.pos += 1;
        self.out.push('[');
        let mut first = true;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.pos += 1;
                    self.out.push(']');
                    return Ok(());
                }
                Some(',') => {
                    self.pos += 1;
                    continue;
                }
                None => return Err(self.error("unterminated array")),
                _ => {}
            }
            if !first {
                self.out.push(',');
            }
            first = false;
            self.value()?;
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                }
                Some(']') => {}
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '$' || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let quote = self.peek().unwrap();
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(s);
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('b') => s.push('\u{8}'),
                        Some('f') => s.push('\u{c}'),
                        Some('u') => {
                            let hex: String = self.chars
                                [self.pos + 1..(self.pos + 5).min(self.chars.len())]
                                .iter()
                                .collect();
                            match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                Some(ch) => {
                                    s.push(ch);
                                    self.pos += 4;
                                }
                                None => return Err(self.error("bad \\u escape")),
                            }
                        }
                        Some(c) => s.push(c),
                        None => return Err(self.error("unterminated string")),
                    }
                    self.pos += 1;
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn regex(&mut self) -> Result<(), ParseError> {
        self.pos += 1;
        let mut pat = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated regex")),
                Some('\\') => {
                    pat.push('\\');
                    self.pos += 1;
                    if let Some(c) = self.peek() {
                        pat.push(c);
                        self.pos += 1;
                    }
                }
                Some('/') => {
                    self.pos += 1;
                    break;
                }
                Some(c) => {
                    pat.push(c);
                    self.pos += 1;
                }
            }
        }
        let mut flags = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() {
                flags.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        self.out.push_str("{\"$regularExpression\":{\"pattern\":");
        self.push_str(&pat);
        self.out.push_str(",\"options\":");
        self.push_str(&flags);
        self.out.push_str("}}");
        Ok(())
    }

    fn number(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        if matches!(self.peek(), Some('-') | Some('+')) {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        let raw = raw.trim_start_matches('+');
        // Leading zeros and trailing dots are not JSON; normalise through i64/f64.
        if let Ok(i) = raw.parse::<i64>() {
            self.out.push_str(&i.to_string());
            return Ok(());
        }
        match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => {
                self.out
                    .push_str(&serde_json::to_string(&v).unwrap_or_else(|_| "null".into()));
                Ok(())
            }
            _ => Err(self.error("bad number")),
        }
    }

    /// Bare words: literals, or a mongosh constructor call.
    fn word(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let mut w = self.ident();
        if w == "new" {
            self.skip_ws();
            w = self.ident();
        }
        match w.as_str() {
            "true" | "false" | "null" => {
                self.out.push_str(&w);
                return Ok(());
            }
            "undefined" => {
                self.out.push_str("{\"$undefined\":true}");
                return Ok(());
            }
            "NaN" => {
                self.out.push_str("{\"$numberDouble\":\"NaN\"}");
                return Ok(());
            }
            "Infinity" => {
                self.out.push_str("{\"$numberDouble\":\"Infinity\"}");
                return Ok(());
            }
            "MinKey" | "MaxKey" => {
                self.skip_call_parens();
                self.out.push_str(if w == "MinKey" {
                    "{\"$minKey\":1}"
                } else {
                    "{\"$maxKey\":1}"
                });
                return Ok(());
            }
            _ => {}
        }
        self.skip_ws();
        if self.peek() != Some('(') {
            self.pos = start;
            return Err(self.error(&format!("unknown identifier `{w}`")));
        }
        self.pos += 1;
        let args = self.call_args()?;
        let arg0 = args.first().cloned();
        let text = match (w.as_str(), arg0) {
            ("ObjectId", Some(Arg::Str(s))) => format!("{{\"$oid\":{}}}", json(&s)),
            ("ObjectId", None) => format!(
                "{{\"$oid\":{}}}",
                json(&bson::oid::ObjectId::new().to_hex())
            ),
            ("ISODate" | "Date", Some(Arg::Str(s))) => {
                format!("{{\"$date\":{}}}", json(&normalise_date(&s)))
            }
            ("ISODate" | "Date", Some(Arg::Num(n))) => {
                format!("{{\"$date\":{{\"$numberLong\":\"{}\"}}}}", n as i64)
            }
            ("ISODate" | "Date", None) => {
                format!(
                    "{{\"$date\":{}}}",
                    json(&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                )
            }
            ("NumberLong" | "Long", Some(a)) => format!("{{\"$numberLong\":{}}}", json(&a.text())),
            ("NumberInt" | "Int32", Some(a)) => format!("{{\"$numberInt\":{}}}", json(&a.text())),
            ("NumberDecimal" | "Decimal128", Some(a)) => {
                format!("{{\"$numberDecimal\":{}}}", json(&a.text()))
            }
            ("Double", Some(a)) => format!("{{\"$numberDouble\":{}}}", json(&a.text())),
            ("Timestamp", Some(Arg::Num(t))) => {
                let i = match args.get(1) {
                    Some(Arg::Num(i)) => *i as u32,
                    _ => 0,
                };
                format!("{{\"$timestamp\":{{\"t\":{},\"i\":{i}}}}}", t as u32)
            }
            ("UUID", Some(Arg::Str(s))) => {
                let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                let bytes: Vec<u8> = (0..hex.len() / 2)
                    .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
                    .collect();
                format!(
                    "{{\"$binary\":{{\"base64\":{},\"subType\":\"04\"}}}}",
                    json(&base64(&bytes))
                )
            }
            ("BinData", Some(Arg::Num(sub))) => {
                let b64 = match args.get(1) {
                    Some(Arg::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                format!(
                    "{{\"$binary\":{{\"base64\":{},\"subType\":\"{:02x}\"}}}}",
                    json(&b64),
                    sub as u8
                )
            }
            ("RegExp", Some(Arg::Str(p))) => {
                let flags = match args.get(1) {
                    Some(Arg::Str(f)) => f.clone(),
                    _ => String::new(),
                };
                format!(
                    "{{\"$regularExpression\":{{\"pattern\":{},\"options\":{}}}}}",
                    json(&p),
                    json(&flags)
                )
            }
            _ => {
                self.pos = start;
                return Err(self.error(&format!("unsupported call `{w}(...)`")));
            }
        };
        self.out.push_str(&text);
        Ok(())
    }

    fn skip_call_parens(&mut self) {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.pos += 1;
            self.skip_ws();
            if self.peek() == Some(')') {
                self.pos += 1;
            }
        }
    }

    /// Arguments of a constructor call: strings or numbers only; consumes the `)`.
    fn call_args(&mut self) -> Result<Vec<Arg>, ParseError> {
        let mut args = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(')') => {
                    self.pos += 1;
                    return Ok(args);
                }
                Some(',') => {
                    self.pos += 1;
                }
                Some('"') | Some('\'') => args.push(Arg::Str(self.string()?)),
                Some(c) if c == '-' || c.is_ascii_digit() => {
                    let start = self.pos;
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let raw: String = self.chars[start..self.pos].iter().collect();
                    args.push(Arg::Num(raw.parse().map_err(|_| self.error("bad number"))?));
                }
                _ => return Err(self.error("bad constructor argument")),
            }
        }
    }
}

#[derive(Clone)]
enum Arg {
    Str(String),
    Num(f64),
}

impl Arg {
    fn text(&self) -> String {
        match self {
            Arg::Str(s) => s.clone(),
            Arg::Num(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    n.to_string()
                }
            }
        }
    }
}

fn json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Accept `2024-01-02` and `2024-01-02T10:00:00` (no zone) as UTC.
fn normalise_date(s: &str) -> String {
    if s.len() == 10 && s.as_bytes()[4] == b'-' {
        return format!("{s}T00:00:00Z");
    }
    let has_zone = s.ends_with('Z') || s[10..].contains('+') || s[10..].contains('-');
    if s.len() > 10 && !has_zone {
        return format!("{s}Z");
    }
    s.to_string()
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn line_col(chars: &[char], idx: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for &c in chars.iter().take(idx) {
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn strict_json_round_trips_every_type() {
        let d = doc! {
            "s": "x", "i": 1i32, "l": 2i64, "d": 1.5, "b": true, "n": Bson::Null,
            "oid": bson::oid::ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap(),
            "dt": bson::DateTime::from_millis(1_700_000_000_000),
            "re": bson::Regex { pattern: "a.*".try_into().unwrap(), options: "i".try_into().unwrap() },
            "arr": [1i32, "two"], "sub": { "k": "v" },
            "dec": bson::Decimal128::from_bytes([0u8; 16]),
            "ts": bson::Timestamp { time: 1, increment: 2 },
        };
        for mode in [Mode::Relaxed, Mode::Canonical] {
            let text = pretty(&d, mode);
            let back = parse_document(&text).unwrap();
            assert_eq!(back.get_str("s").unwrap(), "x");
            assert_eq!(
                back.get_object_id("oid").unwrap(),
                d.get_object_id("oid").unwrap()
            );
            assert_eq!(
                back.get_datetime("dt").unwrap(),
                d.get_datetime("dt").unwrap()
            );
            assert_eq!(back.get("re"), d.get("re"));
            assert_eq!(back.get("ts"), d.get("ts"));
            assert_eq!(back.get("sub"), d.get("sub"));
        }
        let canon = parse_document(&pretty(&d, Mode::Canonical)).unwrap();
        assert_eq!(canon.get("l"), Some(&Bson::Int64(2)));
        assert_eq!(canon.get("i"), Some(&Bson::Int32(1)));
    }

    #[test]
    fn loose_syntax() {
        let d = parse_document(
            r#"{ name: 'Ann', "age": {$gt: 30,}, _id: ObjectId("507f1f77bcf86cd799439011"),
                 when: ISODate("2024-01-02"), n: NumberLong(5), tag: /^a/i, 'quoted key': null, // comment
                 nested: { a: [1, 2, ], b: new Date("2020-05-05T10:00:00Z") } }"#,
        )
        .unwrap();
        assert_eq!(d.get_str("name").unwrap(), "Ann");
        assert_eq!(d.get_document("age").unwrap().get_i32("$gt").unwrap(), 30);
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("when"), Some(Bson::DateTime(_))));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert!(
            matches!(d.get("tag"), Some(Bson::RegularExpression(r)) if r.pattern.as_str() == "^a" && r.options.as_str() == "i")
        );
        assert_eq!(d.get("quoted key"), Some(&Bson::Null));
        assert_eq!(
            d.get_document("nested")
                .unwrap()
                .get_array("a")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn errors_carry_positions() {
        let e = parse_document("{ a: 1,\n  b: }").unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.col >= 5, "{e:?}");
        let e = parse_document("{ a: foo }").unwrap_err();
        assert!(e.msg.contains("foo"));
        assert!(parse_document_or_empty("   ").unwrap().is_empty());
    }

    #[test]
    fn numbers() {
        let d = parse_document("{a: -3, b: 2.5, c: 1e3, d: 007}").unwrap();
        assert_eq!(d.get("a"), Some(&Bson::Int32(-3)));
        assert_eq!(d.get("b"), Some(&Bson::Double(2.5)));
        assert_eq!(d.get("c"), Some(&Bson::Double(1000.0)));
        assert_eq!(d.get("d"), Some(&Bson::Int32(7)));
    }

    #[test]
    fn summaries() {
        assert_eq!(summary(&Bson::String("hi".into()), 10), "\"hi\"");
        assert_eq!(summary(&Bson::Double(3.0), 10), "3.0");
        assert_eq!(summary(&Bson::Array(vec![]), 20), "[ 0 elements ]");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn documents_array() {
        assert_eq!(parse_documents("[{a:1},{a:2}]").unwrap().len(), 2);
        assert_eq!(parse_documents("{a:1}").unwrap().len(), 1);
        assert!(parse_documents("[1]").is_err());
    }
}
