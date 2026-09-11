//! Filter / pipeline → driver code in eight languages (Compass's "Export to
//! language"). BSON values become the driver's literal types; with
//! `driver: true` the values are wrapped in a runnable connect + find /
//! aggregate snippet. Pure; golden-tested.
use crate::mongo::ops::Namespace;
use bson::{Bson, Document};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    CSharp,
    Go,
    Java,
    Node,
    Php,
    Python,
    Ruby,
    Rust,
}

/// (language, label, GtkSourceView language id)
pub const LANGS: &[(Lang, &str, &str)] = &[
    (Lang::CSharp, "C#", "c-sharp"),
    (Lang::Go, "Go", "go"),
    (Lang::Java, "Java", "java"),
    (Lang::Node, "Node.js", "js"),
    (Lang::Php, "PHP", "php"),
    (Lang::Python, "Python", "python3"),
    (Lang::Ruby, "Ruby", "ruby"),
    (Lang::Rust, "Rust", "rust"),
];

#[derive(Clone, Debug, Default)]
pub struct FindInput {
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub collation: Option<Document>,
    pub skip: u64,
    pub limit: u64,
}

#[derive(Clone, Debug)]
pub enum Input {
    Find(Box<FindInput>),
    Pipeline(Vec<Document>),
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Wrap the literal in connect + find/aggregate code.
    pub driver: bool,
    pub uri: String,
    pub ns: Namespace,
}

struct Writer {
    lang: Lang,
    /// Driver types referenced, for import lines.
    used: BTreeSet<&'static str>,
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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

fn escape(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn double(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

impl Writer {
    fn new(lang: Lang) -> Self {
        Writer {
            lang,
            used: BTreeSet::new(),
        }
    }

    fn indent_unit(&self) -> &'static str {
        match self.lang {
            Lang::Node | Lang::Ruby => "  ",
            Lang::Go => "\t",
            _ => "    ",
        }
    }

    fn ind(&self, depth: usize) -> String {
        self.indent_unit().repeat(depth)
    }

    fn string(&self, s: &str) -> String {
        match self.lang {
            Lang::Python | Lang::Node | Lang::Php | Lang::Ruby => escape(s, '\''),
            _ => escape(s, '"'),
        }
    }

    fn key(&self, k: &str) -> String {
        self.string(k)
    }

    fn scalar(&mut self, b: &Bson) -> String {
        use Lang::*;
        let lang = self.lang;
        match b {
            Bson::String(s) | Bson::Symbol(s) => self.string(s),
            Bson::Int32(n) => n.to_string(),
            Bson::Int64(n) => {
                self.used.insert("Int64");
                match lang {
                    Python => format!("Int64({n})"),
                    Node => format!("Long('{n}')"),
                    Java | CSharp => format!("{n}L"),
                    Go => format!("int64({n})"),
                    Rust => format!("{n}i64"),
                    Php | Ruby => n.to_string(),
                }
            }
            Bson::Double(f) => double(*f),
            Bson::Boolean(v) => match lang {
                Python => if *v { "True" } else { "False" }.into(),
                _ => v.to_string(),
            },
            Bson::Null | Bson::Undefined => match lang {
                Python => "None",
                CSharp => "BsonNull.Value",
                Go => "nil",
                Ruby => "nil",
                Rust => "Bson::Null",
                _ => "null",
            }
            .into(),
            Bson::ObjectId(o) => {
                self.used.insert("ObjectId");
                let h = o.to_hex();
                match lang {
                    Python => format!("ObjectId('{h}')"),
                    Node => format!("new ObjectId('{h}')"),
                    Java | CSharp => format!("new ObjectId(\"{h}\")"),
                    Go => format!(
                        "func() primitive.ObjectID {{ oid, _ := primitive.ObjectIDFromHex(\"{h}\"); return oid }}()"
                    ),
                    Php => format!("new ObjectId('{h}')"),
                    Ruby => format!("BSON::ObjectId('{h}')"),
                    Rust => format!("ObjectId::parse_str(\"{h}\")?"),
                }
            }
            Bson::DateTime(d) => {
                self.used.insert("DateTime");
                let ms = d.timestamp_millis();
                let t = d.to_chrono();
                let iso = t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                match lang {
                    Python => {
                        use chrono::{Datelike, Timelike};
                        let us = t.nanosecond() / 1000;
                        let frac = if us > 0 {
                            format!(", {us}")
                        } else {
                            String::new()
                        };
                        format!(
                            "datetime.datetime({}, {}, {}, {}, {}, {}{frac}, tzinfo=datetime.timezone.utc)",
                            t.year(),
                            t.month(),
                            t.day(),
                            t.hour(),
                            t.minute(),
                            t.second()
                        )
                    }
                    Node => format!("new Date('{iso}')"),
                    Java => format!("new java.util.Date({ms}L)"),
                    CSharp => format!("DateTime.Parse(\"{iso}\")"),
                    Go => format!("time.UnixMilli({ms}).UTC()"),
                    Php => format!("new UTCDateTime({ms})"),
                    Ruby => {
                        use chrono::{Datelike, Timelike};
                        let ms_part = t.nanosecond() / 1_000_000;
                        let sec = if ms_part > 0 {
                            format!("{}.{ms_part:03}", t.second())
                        } else {
                            t.second().to_string()
                        };
                        format!(
                            "Time.utc({}, {}, {}, {}, {}, {sec})",
                            t.year(),
                            t.month(),
                            t.day(),
                            t.hour(),
                            t.minute()
                        )
                    }
                    Rust => format!("DateTime::from_millis({ms})"),
                }
            }
            Bson::RegularExpression(r) => {
                self.used.insert("Regex");
                let p = r.pattern.as_str();
                let o = r.options.as_str();
                match lang {
                    Python => {
                        let flags: Vec<&str> = o
                            .chars()
                            .filter_map(|c| match c {
                                'i' => Some("re.IGNORECASE"),
                                'm' => Some("re.MULTILINE"),
                                's' => Some("re.DOTALL"),
                                'x' => Some("re.VERBOSE"),
                                _ => None,
                            })
                            .collect();
                        let pat = format!("r{}", escape(p, '\''));
                        if flags.is_empty() {
                            format!("re.compile({pat})")
                        } else {
                            format!("re.compile({pat}, {})", flags.join(" | "))
                        }
                    }
                    Node | Ruby => format!("/{}/{o}", p.replace('/', "\\/")),
                    Java | CSharp => {
                        format!(
                            "new BsonRegularExpression({}, {})",
                            escape(p, '"'),
                            escape(o, '"')
                        )
                    }
                    Go => format!(
                        "primitive.Regex{{Pattern: {}, Options: {}}}",
                        escape(p, '"'),
                        escape(o, '"')
                    ),
                    Php => format!("new Regex({}, {})", escape(p, '\''), escape(o, '\'')),
                    Rust => format!(
                        "Regex {{ pattern: {}.into(), options: {}.into() }}",
                        escape(p, '"'),
                        escape(o, '"')
                    ),
                }
            }
            Bson::Decimal128(d) => {
                self.used.insert("Decimal128");
                let s = d.to_string();
                match lang {
                    Python => format!("Decimal128('{s}')"),
                    Node => format!("Decimal128.fromString('{s}')"),
                    Java => format!("Decimal128.parse(\"{s}\")"),
                    CSharp => format!("Decimal128.Parse(\"{s}\")"),
                    Go => format!(
                        "func() primitive.Decimal128 {{ d, _ := primitive.ParseDecimal128(\"{s}\"); return d }}()"
                    ),
                    Php => format!("new Decimal128('{s}')"),
                    Ruby => format!("BSON::Decimal128.new('{s}')"),
                    Rust => format!("\"{s}\".parse::<Decimal128>()?"),
                }
            }
            Bson::Binary(bin) => {
                self.used.insert("Binary");
                let b64 = base64(&bin.bytes);
                let sub: u8 = bin.subtype.into();
                match lang {
                    Python => format!("Binary(base64.b64decode('{b64}'), {sub})"),
                    Node => format!("Binary.createFromBase64('{b64}', {sub})"),
                    Java => format!(
                        "new Binary((byte) {sub}, java.util.Base64.getDecoder().decode(\"{b64}\"))"
                    ),
                    CSharp => {
                        let kind = match sub {
                            1 => "Function",
                            2 => "OldBinary",
                            3 => "UuidLegacy",
                            4 => "UuidStandard",
                            5 => "MD5",
                            6 => "Encrypted",
                            7 => "Column",
                            0x80.. => "UserDefined",
                            _ => "Binary",
                        };
                        format!(
                            "new BsonBinaryData(Convert.FromBase64String(\"{b64}\"), BsonBinarySubType.{kind})"
                        )
                    }
                    Go => format!(
                        "primitive.Binary{{Subtype: {sub}, Data: func() []byte {{ b, _ := base64.StdEncoding.DecodeString(\"{b64}\"); return b }}()}}"
                    ),
                    Php => format!("new Binary(base64_decode('{b64}'), {sub})"),
                    Ruby => {
                        let kind = match sub {
                            1 => ":function",
                            2 => ":old",
                            3 => ":uuid_old",
                            4 => ":uuid",
                            5 => ":md5",
                            0x80.. => ":user",
                            _ => ":generic",
                        };
                        format!("BSON::Binary.new(Base64.decode64('{b64}'), {kind})")
                    }
                    Rust => {
                        let kind = match sub {
                            1 => "Function".to_string(),
                            2 => "BinaryOld".into(),
                            3 => "UuidOld".into(),
                            4 => "Uuid".into(),
                            5 => "Md5".into(),
                            6 => "Encrypted".into(),
                            7 => "Column".into(),
                            0x80.. => format!("UserDefined({sub})"),
                            _ => "Generic".into(),
                        };
                        format!(
                            "Binary {{ subtype: BinarySubtype::{kind}, bytes: base64::decode(\"{b64}\")? }}"
                        )
                    }
                }
            }
            Bson::Timestamp(ts) => {
                self.used.insert("Timestamp");
                let (t, i) = (ts.time, ts.increment);
                match lang {
                    Python => format!("Timestamp({t}, {i})"),
                    Node => format!("new Timestamp({{ t: {t}, i: {i} }})"),
                    Java | CSharp => format!("new BsonTimestamp({t}, {i})"),
                    Go => format!("primitive.Timestamp{{T: {t}, I: {i}}}"),
                    Php => format!("new Timestamp({i}, {t})"),
                    Ruby => format!("BSON::Timestamp.new({t}, {i})"),
                    Rust => format!("Timestamp {{ time: {t}, increment: {i} }}"),
                }
            }
            Bson::MinKey | Bson::MaxKey => {
                let name = if matches!(b, Bson::MinKey) {
                    "MinKey"
                } else {
                    "MaxKey"
                };
                self.used
                    .insert(if name == "MinKey" { "MinKey" } else { "MaxKey" });
                match lang {
                    Python => format!("{name}()"),
                    Node | Java | Php => format!("new {name}()"),
                    CSharp => format!("Bson{name}.Value"),
                    Go => format!("primitive.{name}{{}}"),
                    Ruby => format!("BSON::{name}.new"),
                    Rust => format!("Bson::{name}"),
                }
            }
            Bson::JavaScriptCode(c) => {
                self.used.insert("Code");
                match lang {
                    Python => format!("Code({})", escape(c, '\'')),
                    Node => format!("new Code({})", escape(c, '\'')),
                    Java => format!("new Code({})", escape(c, '"')),
                    CSharp => format!("new BsonJavaScript({})", escape(c, '"')),
                    Go => format!("primitive.JavaScript({})", escape(c, '"')),
                    Php => format!("new Javascript({})", escape(c, '\'')),
                    Ruby => format!("BSON::Code.new({})", escape(c, '\'')),
                    Rust => format!("Bson::JavaScriptCode({}.into())", escape(c, '"')),
                }
            }
            Bson::JavaScriptCodeWithScope(c) => self.scalar(&Bson::JavaScriptCode(c.code.clone())),
            Bson::DbPointer(_) => self.scalar(&Bson::Null),
            Bson::Document(_) | Bson::Array(_) => unreachable!(),
        }
    }

    fn value(&mut self, b: &Bson, depth: usize, top: bool) -> String {
        match b {
            Bson::Document(d) => self.document(d, depth, top),
            Bson::Array(a) => self.array(a, depth),
            other => self.scalar(other),
        }
    }

    fn array(&mut self, items: &[Bson], depth: usize) -> String {
        use Lang::*;
        let inline = items
            .iter()
            .all(|i| !matches!(i, Bson::Document(_) | Bson::Array(_)));
        let rendered: Vec<String> = items
            .iter()
            .map(|i| self.value(i, depth + 1, false))
            .collect();
        let joined = rendered.join(", ");
        if items.is_empty() {
            return match self.lang {
                Java => "Arrays.asList()".into(),
                CSharp => "new BsonArray()".into(),
                Go => "bson.A{}".into(),
                _ => "[]".into(),
            };
        }
        if inline && joined.len() <= 60 {
            return match self.lang {
                Java => format!("Arrays.asList({joined})"),
                CSharp => format!("new BsonArray {{ {joined} }}"),
                Go => format!("bson.A{{{joined}}}"),
                _ => format!("[{joined}]"),
            };
        }
        let inner = self.ind(depth + 1);
        let outer = self.ind(depth);
        let body = |sep: &str, trailing: bool| {
            let mut s = String::new();
            for (i, r) in rendered.iter().enumerate() {
                s.push_str(&inner);
                s.push_str(r);
                if i + 1 < rendered.len() || trailing {
                    s.push_str(sep);
                }
                s.push('\n');
            }
            s
        };
        match self.lang {
            Java => format!("Arrays.asList(\n{}{outer})", body(",", false)),
            CSharp => format!("new BsonArray\n{outer}{{\n{}{outer}}}", body(",", false)),
            Go => format!("bson.A{{\n{}{outer}}}", body(",", true)),
            _ => format!("[\n{}{outer}]", body(",", false)),
        }
    }

    fn document(&mut self, d: &Document, depth: usize, top: bool) -> String {
        use Lang::*;
        if d.is_empty() {
            return match self.lang {
                Java => "new Document()".into(),
                CSharp => "new BsonDocument()".into(),
                Go => "bson.D{}".into(),
                Php => "(object) []".into(),
                Rust if top => "doc! {}".into(),
                _ => "{}".into(),
            };
        }
        let entries: Vec<(String, String)> = d
            .iter()
            .map(|(k, v)| (self.key(k), self.value(v, depth + 1, false)))
            .collect();
        let inner = self.ind(depth + 1);
        let outer = self.ind(depth);
        let lines = |f: &dyn Fn(&str, &str) -> String, sep: &str, trailing: bool| {
            let mut s = String::new();
            for (i, (k, v)) in entries.iter().enumerate() {
                s.push_str(&inner);
                s.push_str(&f(k, v));
                if i + 1 < entries.len() || trailing {
                    s.push_str(sep);
                }
                s.push('\n');
            }
            s
        };
        match self.lang {
            Python | Node => format!(
                "{{\n{}{outer}}}",
                lines(&|k, v| format!("{k}: {v}"), ",", false)
            ),
            Ruby | Php => {
                let (open, close) = if self.lang == Ruby {
                    ("{", "}")
                } else {
                    ("[", "]")
                };
                format!(
                    "{open}\n{}{outer}{close}",
                    lines(&|k, v| format!("{k} => {v}"), ",", false)
                )
            }
            Rust => format!(
                "{}{{\n{}{outer}}}",
                if top { "doc! " } else { "" },
                lines(&|k, v| format!("{k}: {v}"), ",", false)
            ),
            Go => format!(
                "bson.D{{\n{}{outer}}}",
                lines(&|k, v| format!("{{{k}, {v}}}"), ",", true)
            ),
            Java => {
                let mut s = format!("new Document({}, {})", entries[0].0, entries[0].1);
                for (k, v) in &entries[1..] {
                    s.push_str(&format!("\n{inner}.append({k}, {v})"));
                }
                s
            }
            CSharp => {
                if entries.len() == 1 {
                    format!("new BsonDocument({}, {})", entries[0].0, entries[0].1)
                } else {
                    format!(
                        "new BsonDocument\n{outer}{{\n{}{outer}}}",
                        lines(&|k, v| format!("{{ {k}, {v} }}"), ",", false)
                    )
                }
            }
        }
    }

    fn imports(&self) -> String {
        use Lang::*;
        let has = |t: &str| self.used.contains(t);
        let mut lines: Vec<String> = Vec::new();
        match self.lang {
            Python => {
                lines.push("from pymongo import MongoClient".into());
                if has("ObjectId") {
                    lines.push("from bson import ObjectId".into());
                }
                if has("Int64") {
                    lines.push("from bson.int64 import Int64".into());
                }
                if has("Decimal128") {
                    lines.push("from bson.decimal128 import Decimal128".into());
                }
                if has("Binary") {
                    lines.push("from bson.binary import Binary".into());
                    lines.push("import base64".into());
                }
                if has("Timestamp") {
                    lines.push("from bson.timestamp import Timestamp".into());
                }
                if has("MinKey") {
                    lines.push("from bson.min_key import MinKey".into());
                }
                if has("MaxKey") {
                    lines.push("from bson.max_key import MaxKey".into());
                }
                if has("Code") {
                    lines.push("from bson.code import Code".into());
                }
                if has("DateTime") {
                    lines.push("import datetime".into());
                }
                if has("Regex") {
                    lines.push("import re".into());
                }
            }
            Node => {
                let mut names = vec!["MongoClient"];
                for t in [
                    "ObjectId",
                    "Long",
                    "Decimal128",
                    "Binary",
                    "Timestamp",
                    "MinKey",
                    "MaxKey",
                    "Code",
                ] {
                    let key = if t == "Long" { "Int64" } else { t };
                    if has(key) {
                        names.push(t);
                    }
                }
                lines.push(format!(
                    "const {{ {} }} = require('mongodb');",
                    names.join(", ")
                ));
            }
            Java => {
                lines.push("import com.mongodb.client.*;".into());
                lines.push("import org.bson.Document;".into());
                lines.push("import org.bson.conversions.Bson;".into());
                lines.push("import java.util.Arrays;".into());
                if has("ObjectId") {
                    lines.push("import org.bson.types.ObjectId;".into());
                }
                if has("Decimal128") {
                    lines.push("import org.bson.types.Decimal128;".into());
                }
                if has("Binary") {
                    lines.push("import org.bson.types.Binary;".into());
                }
                if has("Regex") {
                    lines.push("import org.bson.BsonRegularExpression;".into());
                }
                if has("Timestamp") {
                    lines.push("import org.bson.BsonTimestamp;".into());
                }
                if has("MinKey") {
                    lines.push("import org.bson.types.MinKey;".into());
                }
                if has("MaxKey") {
                    lines.push("import org.bson.types.MaxKey;".into());
                }
                if has("Code") {
                    lines.push("import org.bson.types.Code;".into());
                }
            }
            CSharp => {
                lines.push("using MongoDB.Bson;".into());
                lines.push("using MongoDB.Driver;".into());
                if has("DateTime") {
                    lines.push("using System;".into());
                }
            }
            Go => {
                lines.push("import (".into());
                lines.push("\t\"context\"".into());
                lines.push("\t\"log\"".into());
                if has("Binary") {
                    lines.push("\t\"encoding/base64\"".into());
                }
                if has("DateTime") {
                    lines.push("\t\"time\"".into());
                }
                lines.push(String::new());
                lines.push("\t\"go.mongodb.org/mongo-driver/bson\"".into());
                if [
                    "ObjectId",
                    "Regex",
                    "Decimal128",
                    "Binary",
                    "Timestamp",
                    "MinKey",
                    "MaxKey",
                    "Code",
                ]
                .iter()
                .any(|t| has(t))
                {
                    lines.push("\t\"go.mongodb.org/mongo-driver/bson/primitive\"".into());
                }
                lines.push("\t\"go.mongodb.org/mongo-driver/mongo\"".into());
                lines.push("\t\"go.mongodb.org/mongo-driver/mongo/options\"".into());
                lines.push(")".into());
            }
            Php => {
                lines.push("require 'vendor/autoload.php';".into());
                for (t, class) in [
                    ("ObjectId", "ObjectId"),
                    ("DateTime", "UTCDateTime"),
                    ("Regex", "Regex"),
                    ("Decimal128", "Decimal128"),
                    ("Binary", "Binary"),
                    ("Timestamp", "Timestamp"),
                    ("MinKey", "MinKey"),
                    ("MaxKey", "MaxKey"),
                    ("Code", "Javascript"),
                ] {
                    if has(t) {
                        lines.push(format!("use MongoDB\\BSON\\{class};"));
                    }
                }
            }
            Ruby => {
                lines.push("require 'mongo'".into());
                if has("Binary") {
                    lines.push("require 'base64'".into());
                }
            }
            Rust => {
                let mut types = vec!["doc", "Document"];
                for t in [
                    "Bson",
                    "ObjectId",
                    "DateTime",
                    "Regex",
                    "Decimal128",
                    "Timestamp",
                ] {
                    if has(t) || (t == "Bson" && (has("MinKey") || has("MaxKey") || has("Code"))) {
                        types.push(t);
                    }
                }
                if has("Binary") {
                    types.push("Binary");
                    types.push("spec::BinarySubtype");
                }
                lines.push(format!(
                    "use mongodb::{{bson::{{{}}}, Client, Collection}};",
                    types.join(", ")
                ));
            }
        }
        lines.join("\n")
    }
}

/// The literal alone (no driver code).
pub fn literal(value: &Bson, lang: Lang) -> String {
    Writer::new(lang).value(value, 0, true)
}

pub fn export(input: &Input, lang: Lang, opts: &Options) -> String {
    use Lang::*;
    let mut w = Writer::new(lang);
    let db = w.string(&opts.ns.db);
    let coll = w.string(&opts.ns.coll);
    let uri = w.string(&opts.uri);
    match input {
        Input::Find(f) => {
            let filter = w.document(&f.filter, 0, true);
            let projection = f.projection.as_ref().map(|d| w.document(d, 0, true));
            let sort = f.sort.as_ref().map(|d| w.document(d, 0, true));
            let collation = f.collation.as_ref().map(|d| w.document(d, 0, true));
            if !opts.driver {
                return filter;
            }
            let imports = w.imports();
            let mut out = String::new();
            match lang {
                Python => {
                    out.push_str(&format!(
                        "{imports}\n\nclient = MongoClient({uri})\nfilter = {filter}\n"
                    ));
                    let mut args = vec!["filter=filter".to_string()];
                    if let Some(p) = projection {
                        out.push_str(&format!("projection = {p}\n"));
                        args.push("projection=projection".into());
                    }
                    if let Some(s) = &f.sort {
                        let pairs: Vec<String> = s
                            .iter()
                            .map(|(k, v)| format!("({}, {})", w.string(k), w.value(v, 0, false)))
                            .collect();
                        out.push_str(&format!("sort = [{}]\n", pairs.join(", ")));
                        args.push("sort=sort".into());
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("collation = {c}\n"));
                        args.push("collation=collation".into());
                    }
                    if f.skip > 0 {
                        args.push(format!("skip={}", f.skip));
                    }
                    if f.limit > 0 {
                        args.push(format!("limit={}", f.limit));
                    }
                    out.push_str(&format!(
                        "\nresult = client[{db}][{coll}].find(\n    {}\n)\n",
                        args.join(",\n    ")
                    ));
                }
                Node => {
                    out.push_str(&format!(
                        "{imports}\n\nconst client = await MongoClient.connect({uri});\nconst coll = client.db({db}).collection({coll});\n\nconst filter = {filter};\n"
                    ));
                    let mut o: Vec<String> = Vec::new();
                    if let Some(p) = projection {
                        out.push_str(&format!("const projection = {p};\n"));
                        o.push("projection".into());
                    }
                    if let Some(s) = sort {
                        out.push_str(&format!("const sort = {s};\n"));
                        o.push("sort".into());
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("const collation = {c};\n"));
                        o.push("collation".into());
                    }
                    if f.skip > 0 {
                        o.push(format!("skip: {}", f.skip));
                    }
                    if f.limit > 0 {
                        o.push(format!("limit: {}", f.limit));
                    }
                    let options = if o.is_empty() {
                        String::new()
                    } else {
                        format!(", {{ {} }}", o.join(", "))
                    };
                    out.push_str(&format!(
                        "const result = await coll.find(filter{options}).toArray();\nawait client.close();\n"
                    ));
                }
                Java => {
                    out.push_str(&format!(
                        "{imports}\n\nMongoClient mongoClient = MongoClients.create({uri});\nMongoCollection<Document> collection = mongoClient.getDatabase({db}).getCollection({coll});\n\nBson filter = {filter};\n"
                    ));
                    let mut chain = String::from("collection.find(filter)");
                    if let Some(p) = projection {
                        out.push_str(&format!("Bson projection = {p};\n"));
                        chain.push_str("\n    .projection(projection)");
                    }
                    if let Some(s) = sort {
                        out.push_str(&format!("Bson sort = {s};\n"));
                        chain.push_str("\n    .sort(sort)");
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("// collation: {}\n", c.replace('\n', " ")));
                    }
                    if f.skip > 0 {
                        chain.push_str(&format!("\n    .skip({})", f.skip));
                    }
                    if f.limit > 0 {
                        chain.push_str(&format!("\n    .limit({})", f.limit));
                    }
                    out.push_str(&format!("FindIterable<Document> result = {chain};\n"));
                }
                CSharp => {
                    out.push_str(&format!(
                        "{imports}\n\nvar client = new MongoClient({uri});\nvar collection = client.GetDatabase({db}).GetCollection<BsonDocument>({coll});\n\nvar filter = {filter};\n"
                    ));
                    let mut chain = String::from("collection.Find(filter)");
                    if let Some(p) = projection {
                        out.push_str(&format!("var projection = {p};\n"));
                        chain.push_str("\n    .Project(projection)");
                    }
                    if let Some(s) = sort {
                        out.push_str(&format!("var sort = {s};\n"));
                        chain.push_str("\n    .Sort(sort)");
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("// collation: {}\n", c.replace('\n', " ")));
                    }
                    if f.skip > 0 {
                        chain.push_str(&format!("\n    .Skip({})", f.skip));
                    }
                    if f.limit > 0 {
                        chain.push_str(&format!("\n    .Limit({})", f.limit));
                    }
                    out.push_str(&format!("var result = {chain}\n    .ToList();\n"));
                }
                Go => {
                    let indent = |s: &str| s.replace('\n', "\n\t");
                    out.push_str(&format!(
                        "package main\n\n{imports}\n\nfunc main() {{\n\tctx := context.TODO()\n\tclient, err := mongo.Connect(ctx, options.Client().ApplyURI({uri}))\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tdefer client.Disconnect(ctx)\n\tcoll := client.Database({db}).Collection({coll})\n\n\tfilter := {}\n",
                        indent(&filter)
                    ));
                    let mut o = String::from("options.Find()");
                    if let Some(p) = projection {
                        out.push_str(&format!("\tprojection := {}\n", indent(&p)));
                        o.push_str(".SetProjection(projection)");
                    }
                    if let Some(s) = sort {
                        out.push_str(&format!("\tsort := {}\n", indent(&s)));
                        o.push_str(".SetSort(sort)");
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("\t// collation: {}\n", c.replace('\n', " ")));
                    }
                    if f.skip > 0 {
                        o.push_str(&format!(".SetSkip({})", f.skip));
                    }
                    if f.limit > 0 {
                        o.push_str(&format!(".SetLimit({})", f.limit));
                    }
                    out.push_str(&format!(
                        "\topts := {o}\n\tcursor, err := coll.Find(ctx, filter, opts)\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tvar results []bson.M\n\tif err = cursor.All(ctx, &results); err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n}}\n"
                    ));
                }
                Php => {
                    out.push_str(&format!(
                        "<?php\n{imports}\n\n$client = new MongoDB\\Client({uri});\n$collection = $client->selectCollection({db}, {coll});\n\n$filter = {filter};\n"
                    ));
                    let mut o: Vec<String> = Vec::new();
                    if let Some(p) = projection {
                        o.push(format!("'projection' => {}", p.replace('\n', "\n    ")));
                    }
                    if let Some(s) = sort {
                        o.push(format!("'sort' => {}", s.replace('\n', "\n    ")));
                    }
                    if let Some(c) = collation {
                        o.push(format!("'collation' => {}", c.replace('\n', "\n    ")));
                    }
                    if f.skip > 0 {
                        o.push(format!("'skip' => {}", f.skip));
                    }
                    if f.limit > 0 {
                        o.push(format!("'limit' => {}", f.limit));
                    }
                    if o.is_empty() {
                        out.push_str("$result = $collection->find($filter);\n");
                    } else {
                        out.push_str(&format!(
                            "$options = [\n    {},\n];\n$result = $collection->find($filter, $options);\n",
                            o.join(",\n    ")
                        ));
                    }
                }
                Ruby => {
                    out.push_str(&format!(
                        "{imports}\n\nclient = Mongo::Client.new({uri}, database: {db})\nfilter = {filter}\n"
                    ));
                    let mut chain = format!("client[{coll}].find(filter)");
                    if let Some(p) = projection {
                        out.push_str(&format!("projection = {p}\n"));
                        chain.push_str("\n  .projection(projection)");
                    }
                    if let Some(s) = sort {
                        out.push_str(&format!("sort = {s}\n"));
                        chain.push_str("\n  .sort(sort)");
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("collation = {c}\n"));
                        chain.push_str("\n  .collation(collation)");
                    }
                    if f.skip > 0 {
                        chain.push_str(&format!("\n  .skip({})", f.skip));
                    }
                    if f.limit > 0 {
                        chain.push_str(&format!("\n  .limit({})", f.limit));
                    }
                    out.push_str(&format!("result = {chain}\n"));
                }
                Rust => {
                    out.push_str(&format!(
                        "{imports}\n\nlet client = Client::with_uri_str({uri}).await?;\nlet coll: Collection<Document> = client.database({db}).collection({coll});\n\nlet filter = {filter};\n"
                    ));
                    let mut chain = String::from("coll.find(filter)");
                    if let Some(p) = projection {
                        chain
                            .push_str(&format!("\n    .projection({})", p.replace('\n', "\n    ")));
                    }
                    if let Some(s) = sort {
                        chain.push_str(&format!("\n    .sort({})", s.replace('\n', "\n    ")));
                    }
                    if let Some(c) = collation {
                        out.push_str(&format!("// collation: {}\n", c.replace('\n', " ")));
                    }
                    if f.skip > 0 {
                        chain.push_str(&format!("\n    .skip({})", f.skip));
                    }
                    if f.limit > 0 {
                        chain.push_str(&format!("\n    .limit({})", f.limit));
                    }
                    out.push_str(&format!("let mut cursor = {chain}\n    .await?;\n"));
                }
            }
            out
        }
        Input::Pipeline(stages) => {
            let rendered: Vec<String> = stages.iter().map(|s| w.document(s, 1, true)).collect();
            let unit = w.indent_unit();
            let items = |sep: &str, trailing: bool| {
                let mut s = String::new();
                for (i, r) in rendered.iter().enumerate() {
                    s.push_str(unit);
                    s.push_str(r);
                    if i + 1 < rendered.len() || trailing {
                        s.push_str(sep);
                    }
                    s.push('\n');
                }
                s
            };
            let pipeline = match lang {
                Java => format!("Arrays.asList(\n{})", items(",", false)),
                CSharp => format!("new BsonDocument[]\n{{\n{}}}", items(",", false)),
                Go => format!("mongo.Pipeline{{\n{}}}", items(",", true)),
                Rust => format!("vec![\n{}]", items(",", false)),
                _ => format!("[\n{}]", items(",", false)),
            };
            if !opts.driver {
                return pipeline;
            }
            let imports = w.imports();
            match lang {
                Python => format!(
                    "{imports}\n\nclient = MongoClient({uri})\npipeline = {pipeline}\n\nresult = client[{db}][{coll}].aggregate(pipeline)\n"
                ),
                Node => format!(
                    "{imports}\n\nconst client = await MongoClient.connect({uri});\nconst coll = client.db({db}).collection({coll});\n\nconst pipeline = {pipeline};\nconst result = await coll.aggregate(pipeline).toArray();\nawait client.close();\n"
                ),
                Java => format!(
                    "{imports}\n\nMongoClient mongoClient = MongoClients.create({uri});\nMongoCollection<Document> collection = mongoClient.getDatabase({db}).getCollection({coll});\n\nAggregateIterable<Document> result = collection.aggregate({pipeline});\n"
                ),
                CSharp => format!(
                    "{imports}\n\nvar client = new MongoClient({uri});\nvar collection = client.GetDatabase({db}).GetCollection<BsonDocument>({coll});\n\nvar pipeline = {pipeline};\nvar result = collection.Aggregate<BsonDocument>(pipeline).ToList();\n"
                ),
                Go => format!(
                    "package main\n\n{imports}\n\nfunc main() {{\n\tctx := context.TODO()\n\tclient, err := mongo.Connect(ctx, options.Client().ApplyURI({uri}))\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tdefer client.Disconnect(ctx)\n\tcoll := client.Database({db}).Collection({coll})\n\n\tpipeline := {}\n\tcursor, err := coll.Aggregate(ctx, pipeline)\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tvar results []bson.M\n\tif err = cursor.All(ctx, &results); err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n}}\n",
                    pipeline.replace('\n', "\n\t")
                ),
                Php => format!(
                    "<?php\n{imports}\n\n$client = new MongoDB\\Client({uri});\n$collection = $client->selectCollection({db}, {coll});\n\n$pipeline = {pipeline};\n$result = $collection->aggregate($pipeline);\n"
                ),
                Ruby => format!(
                    "{imports}\n\nclient = Mongo::Client.new({uri}, database: {db})\npipeline = {pipeline}\nresult = client[{coll}].aggregate(pipeline)\n"
                ),
                Rust => format!(
                    "{imports}\n\nlet client = Client::with_uri_str({uri}).await?;\nlet coll: Collection<Document> = client.database({db}).collection({coll});\n\nlet pipeline = {pipeline};\nlet mut cursor = coll.aggregate(pipeline).await?;\n"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId};

    fn sample() -> Document {
        doc! {
            "age": { "$gte": 18 },
            "tags": { "$in": ["vip", "beta"] },
            "_id": ObjectId::parse_str("5d505646cf6d4fe581014ab2").unwrap(),
            "created": { "$lt": bson::DateTime::from_millis(1_577_836_800_000) },
            "active": true,
            "note": Bson::Null,
            "big": 5_i64,
            "ratio": 1.5,
            "pat": bson::Regex { pattern: "^a".try_into().unwrap(), options: "i".try_into().unwrap() },
        }
    }

    fn lit(lang: Lang) -> String {
        literal(&Bson::Document(sample()), lang)
    }

    #[test]
    fn python() {
        assert_eq!(
            lit(Lang::Python),
            "{\n    'age': {\n        '$gte': 18\n    },\n    'tags': {\n        '$in': ['vip', 'beta']\n    },\n    '_id': ObjectId('5d505646cf6d4fe581014ab2'),\n    'created': {\n        '$lt': datetime.datetime(2020, 1, 1, 0, 0, 0, tzinfo=datetime.timezone.utc)\n    },\n    'active': True,\n    'note': None,\n    'big': Int64(5),\n    'ratio': 1.5,\n    'pat': re.compile(r'^a', re.IGNORECASE)\n}"
        );
    }

    #[test]
    fn node() {
        assert_eq!(
            lit(Lang::Node),
            "{\n  'age': {\n    '$gte': 18\n  },\n  'tags': {\n    '$in': ['vip', 'beta']\n  },\n  '_id': new ObjectId('5d505646cf6d4fe581014ab2'),\n  'created': {\n    '$lt': new Date('2020-01-01T00:00:00.000Z')\n  },\n  'active': true,\n  'note': null,\n  'big': Long('5'),\n  'ratio': 1.5,\n  'pat': /^a/i\n}"
        );
    }

    #[test]
    fn java() {
        assert_eq!(
            lit(Lang::Java),
            "new Document(\"age\", new Document(\"$gte\", 18))\n    .append(\"tags\", new Document(\"$in\", Arrays.asList(\"vip\", \"beta\")))\n    .append(\"_id\", new ObjectId(\"5d505646cf6d4fe581014ab2\"))\n    .append(\"created\", new Document(\"$lt\", new java.util.Date(1577836800000L)))\n    .append(\"active\", true)\n    .append(\"note\", null)\n    .append(\"big\", 5L)\n    .append(\"ratio\", 1.5)\n    .append(\"pat\", new BsonRegularExpression(\"^a\", \"i\"))"
        );
    }

    #[test]
    fn csharp() {
        assert_eq!(
            lit(Lang::CSharp),
            "new BsonDocument\n{\n    { \"age\", new BsonDocument(\"$gte\", 18) },\n    { \"tags\", new BsonDocument(\"$in\", new BsonArray { \"vip\", \"beta\" }) },\n    { \"_id\", new ObjectId(\"5d505646cf6d4fe581014ab2\") },\n    { \"created\", new BsonDocument(\"$lt\", DateTime.Parse(\"2020-01-01T00:00:00.000Z\")) },\n    { \"active\", true },\n    { \"note\", BsonNull.Value },\n    { \"big\", 5L },\n    { \"ratio\", 1.5 },\n    { \"pat\", new BsonRegularExpression(\"^a\", \"i\") }\n}"
        );
    }

    #[test]
    fn go() {
        assert_eq!(
            lit(Lang::Go),
            "bson.D{\n\t{\"age\", bson.D{\n\t\t{\"$gte\", 18},\n\t}},\n\t{\"tags\", bson.D{\n\t\t{\"$in\", bson.A{\"vip\", \"beta\"}},\n\t}},\n\t{\"_id\", func() primitive.ObjectID { oid, _ := primitive.ObjectIDFromHex(\"5d505646cf6d4fe581014ab2\"); return oid }()},\n\t{\"created\", bson.D{\n\t\t{\"$lt\", time.UnixMilli(1577836800000).UTC()},\n\t}},\n\t{\"active\", true},\n\t{\"note\", nil},\n\t{\"big\", int64(5)},\n\t{\"ratio\", 1.5},\n\t{\"pat\", primitive.Regex{Pattern: \"^a\", Options: \"i\"}},\n}"
        );
    }

    #[test]
    fn php() {
        assert_eq!(
            lit(Lang::Php),
            "[\n    'age' => [\n        '$gte' => 18\n    ],\n    'tags' => [\n        '$in' => ['vip', 'beta']\n    ],\n    '_id' => new ObjectId('5d505646cf6d4fe581014ab2'),\n    'created' => [\n        '$lt' => new UTCDateTime(1577836800000)\n    ],\n    'active' => true,\n    'note' => null,\n    'big' => 5,\n    'ratio' => 1.5,\n    'pat' => new Regex('^a', 'i')\n]"
        );
    }

    #[test]
    fn ruby() {
        assert_eq!(
            lit(Lang::Ruby),
            "{\n  'age' => {\n    '$gte' => 18\n  },\n  'tags' => {\n    '$in' => ['vip', 'beta']\n  },\n  '_id' => BSON::ObjectId('5d505646cf6d4fe581014ab2'),\n  'created' => {\n    '$lt' => Time.utc(2020, 1, 1, 0, 0, 0)\n  },\n  'active' => true,\n  'note' => nil,\n  'big' => 5,\n  'ratio' => 1.5,\n  'pat' => /^a/i\n}"
        );
    }

    #[test]
    fn rust() {
        assert_eq!(
            lit(Lang::Rust),
            "doc! {\n    \"age\": {\n        \"$gte\": 18\n    },\n    \"tags\": {\n        \"$in\": [\"vip\", \"beta\"]\n    },\n    \"_id\": ObjectId::parse_str(\"5d505646cf6d4fe581014ab2\")?,\n    \"created\": {\n        \"$lt\": DateTime::from_millis(1577836800000)\n    },\n    \"active\": true,\n    \"note\": Bson::Null,\n    \"big\": 5i64,\n    \"ratio\": 1.5,\n    \"pat\": Regex { pattern: \"^a\".into(), options: \"i\".into() }\n}"
        );
    }

    #[test]
    fn driver_snippets() {
        let opts = Options {
            driver: true,
            uri: "mongodb://localhost:27017".into(),
            ns: Namespace::new("db", "coll"),
        };
        let find = Input::Find(Box::new(FindInput {
            filter: doc! { "a": 1 },
            sort: Some(doc! { "a": -1 }),
            skip: 5,
            limit: 10,
            ..Default::default()
        }));
        assert_eq!(
            export(&find, Lang::Python, &opts),
            "from pymongo import MongoClient\n\nclient = MongoClient('mongodb://localhost:27017')\nfilter = {\n    'a': 1\n}\nsort = [('a', -1)]\n\nresult = client['db']['coll'].find(\n    filter=filter,\n    sort=sort,\n    skip=5,\n    limit=10\n)\n"
        );
        let node = export(&find, Lang::Node, &opts);
        assert!(node.contains(
            "const result = await coll.find(filter, { sort, skip: 5, limit: 10 }).toArray();"
        ));
        let pipeline = Input::Pipeline(vec![doc! { "$match": { "a": 1 } }, doc! { "$limit": 2 }]);
        assert_eq!(
            export(
                &pipeline,
                Lang::Ruby,
                &Options {
                    driver: false,
                    ..opts.clone()
                }
            ),
            "[\n  {\n    '$match' => {\n      'a' => 1\n    }\n  },\n  {\n    '$limit' => 2\n  }\n]"
        );
        let rust = export(&pipeline, Lang::Rust, &opts);
        assert!(rust.starts_with("use mongodb::{bson::{doc, Document}, Client, Collection};"));
        assert!(rust.contains("let pipeline = vec![\n    doc! {\n        \"$match\": {\n            \"a\": 1\n        }\n    },\n    doc! {\n        \"$limit\": 2\n    }\n];"));
        let go = export(&pipeline, Lang::Go, &opts);
        assert!(go.contains("\tpipeline := mongo.Pipeline{\n\t\tbson.D{\n\t\t\t{\"$match\", bson.D{\n\t\t\t\t{\"a\", 1},\n\t\t\t}},\n\t\t},\n\t\tbson.D{\n\t\t\t{\"$limit\", 2},\n\t\t},\n\t}\n"));
        for (lang, _, _) in LANGS {
            let s = export(&find, *lang, &opts);
            assert!(s.contains("mongodb://localhost:27017"), "{lang:?}");
        }
    }

    #[test]
    fn exotic_types_and_imports() {
        let d = doc! {
            "b": bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: vec![1, 2, 3] },
            "t": bson::Timestamp { time: 1, increment: 2 },
            "d": "1.5".parse::<bson::Decimal128>().unwrap(),
            "min": Bson::MinKey,
            "code": Bson::JavaScriptCode("function() {}".into()),
            "empty": {},
            "list": [],
            "nested": [{ "x": 1 }],
        };
        let py = literal(&Bson::Document(d.clone()), Lang::Python);
        assert!(py.contains("Binary(base64.b64decode('AQID'), 0)"));
        assert!(py.contains("Timestamp(1, 2)"));
        assert!(py.contains("Decimal128('1.5')"));
        assert!(py.contains("'min': MinKey()"));
        assert!(py.contains("Code('function() {}')"));
        assert!(py.contains("'empty': {}"));
        assert!(py.contains("'list': []"));
        assert!(py.contains("'nested': [\n        {\n            'x': 1\n        }\n    ]"));
        let opts = Options {
            driver: true,
            uri: "u".into(),
            ns: Namespace::new("d", "c"),
        };
        let py = export(
            &Input::Find(Box::new(FindInput {
                filter: d.clone(),
                ..Default::default()
            })),
            Lang::Python,
            &opts,
        );
        assert!(py.starts_with("from pymongo import MongoClient\nfrom bson.decimal128 import Decimal128\nfrom bson.binary import Binary\nimport base64\nfrom bson.timestamp import Timestamp\nfrom bson.min_key import MinKey\nfrom bson.code import Code\n"));
        let php = literal(&Bson::Document(d), Lang::Php);
        assert!(php.contains("'empty' => (object) []"));
        assert!(php.contains("new Timestamp(2, 1)"));
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b""), "");
    }
}
