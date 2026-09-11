//! Export documents to a file: JSON in three Extended JSON flavours (as an
//! array or one document per line) or CSV with flattened field paths. The
//! formatting is pure and tested; `run` streams a find or an aggregation
//! through a writer on tokio, reporting progress.
use super::ejson;
use super::ops::{self, AggOpts, FindSpec, Namespace, OpCtx};
use anyhow::{Context, Result};
use bson::{Bson, Document};
use futures_util::TryStreamExt;
use mongodb::Client;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum JsonMode {
    /// Relaxed, except 64-bit integers keep `$numberLong` so nothing is lost
    /// to double precision in JavaScript readers.
    #[default]
    Default,
    Relaxed,
    Canonical,
}

impl JsonMode {
    pub const ALL: [JsonMode; 3] = [JsonMode::Default, JsonMode::Relaxed, JsonMode::Canonical];

    pub fn label(self) -> &'static str {
        match self {
            JsonMode::Default => "Default Extended JSON",
            JsonMode::Relaxed => "Relaxed Extended JSON",
            JsonMode::Canonical => "Canonical Extended JSON",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            JsonMode::Default => "Plain numbers and ISO dates; Int64 keeps $numberLong",
            JsonMode::Relaxed => {
                "Plain numbers and ISO dates everywhere; loses Int32/Int64/Double distinctions"
            }
            JsonMode::Canonical => {
                "Every type wrapped ($numberInt, $numberDouble, …); round-trips exactly"
            }
        }
    }

    pub fn from_index(i: u32) -> JsonMode {
        JsonMode::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

fn default_value(b: &Bson) -> Value {
    match b {
        Bson::Int64(i) => serde_json::json!({ "$numberLong": i.to_string() }),
        Bson::Document(d) => Value::Object(
            d.iter()
                .map(|(k, v)| (k.clone(), default_value(v)))
                .collect(),
        ),
        Bson::Array(a) => Value::Array(a.iter().map(default_value).collect()),
        other => other.clone().into_relaxed_extjson(),
    }
}

pub fn json_value(doc: &Document, mode: JsonMode) -> Value {
    match mode {
        JsonMode::Default => default_value(&Bson::Document(doc.clone())),
        JsonMode::Relaxed => Bson::Document(doc.clone()).into_relaxed_extjson(),
        JsonMode::Canonical => Bson::Document(doc.clone()).into_canonical_extjson(),
    }
}

pub fn json_text(doc: &Document, mode: JsonMode, pretty: bool) -> String {
    let v = json_value(doc, mode);
    if pretty {
        serde_json::to_string_pretty(&v).unwrap_or_default()
    } else {
        serde_json::to_string(&v).unwrap_or_default()
    }
}

/// Writes `[\n  doc,\n  doc\n]` (pretty, each document indented) or one
/// compact document per line.
pub struct JsonWriter<W: Write> {
    w: W,
    mode: JsonMode,
    ndjson: bool,
    count: u64,
}

impl<W: Write> JsonWriter<W> {
    pub fn new(w: W, mode: JsonMode, ndjson: bool) -> Self {
        Self {
            w,
            mode,
            ndjson,
            count: 0,
        }
    }

    pub fn write(&mut self, doc: &Document) -> std::io::Result<()> {
        if self.ndjson {
            self.w
                .write_all(json_text(doc, self.mode, false).as_bytes())?;
            self.w.write_all(b"\n")?;
        } else {
            if self.count == 0 {
                self.w.write_all(b"[\n")?;
            } else {
                self.w.write_all(b",\n")?;
            }
            let text = json_text(doc, self.mode, true);
            for (i, line) in text.lines().enumerate() {
                if i > 0 {
                    self.w.write_all(b"\n")?;
                }
                self.w.write_all(b"  ")?;
                self.w.write_all(line.as_bytes())?;
            }
        }
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<W> {
        if !self.ndjson {
            if self.count == 0 {
                self.w.write_all(b"[")?;
            }
            self.w.write_all(b"\n]\n")?;
        }
        self.w.flush()?;
        Ok(self.w)
    }
}

// ----- CSV --------------------------------------------------------------------

fn leaves(prefix: &str, b: &Bson, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    let push = |p: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if seen.insert(p.to_string()) {
            out.push(p.to_string());
        }
    };
    match b {
        Bson::Document(d) if !d.is_empty() => {
            for (k, v) in d {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaves(&p, v, out, seen);
            }
        }
        Bson::Array(a) if !a.is_empty() => {
            for (i, v) in a.iter().enumerate() {
                leaves(&format!("{prefix}.{i}"), v, out, seen);
            }
        }
        _ => push(prefix, out, seen),
    }
}

/// Every leaf path across the documents, flattened with dots (`address.city`,
/// `tags.0`), in first-seen order.
pub fn csv_fields(docs: &[Document]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for d in docs {
        leaves("", &Bson::Document(d.clone()), &mut out, &mut seen);
    }
    out
}

/// Walk a dotted path through documents and arrays.
pub fn lookup<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut cur: Option<&Bson> = None;
    let mut container: Option<&Document> = Some(doc);
    for seg in path.split('.') {
        let next = match (container, cur) {
            (Some(d), _) => d.get(seg),
            (None, Some(Bson::Array(a))) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => None,
        }?;
        cur = Some(next);
        container = match next {
            Bson::Document(d) => Some(d),
            _ => None,
        };
    }
    cur
}

/// A cell's text: strings verbatim, numbers plain, dates ISO 8601, ObjectIds
/// as hex, null empty, anything nested as compact Extended JSON.
pub fn csv_value(b: &Bson) -> String {
    match b {
        Bson::String(s) | Bson::Symbol(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => {
            if d.is_finite() {
                d.to_string()
            } else {
                String::new()
            }
        }
        Bson::Decimal128(d) => d.to_string(),
        Bson::Boolean(v) => v.to_string(),
        Bson::Null | Bson::Undefined => String::new(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::DateTime(d) => d
            .try_to_rfc3339_string()
            .unwrap_or_else(|_| d.timestamp_millis().to_string()),
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        Bson::Timestamp(t) => format!("{}:{}", t.time, t.increment),
        other => ejson::compact(&bson::doc! { "v": other.clone() }, ejson::Mode::Relaxed)
            .strip_prefix("{\"v\":")
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or_default()
            .to_string(),
    }
}

/// Neutralise spreadsheet formula injection: a leading `=`, `+`, `-`, `@`,
/// tab or CR gets a `'` in front.
pub fn escape_formula(s: &str) -> String {
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    }
}

pub fn csv_cell(b: Option<&Bson>, escape_formulas: bool) -> String {
    match b {
        None => String::new(),
        Some(b) => {
            let s = csv_value(b);
            if escape_formulas && matches!(b, Bson::String(_) | Bson::Symbol(_)) {
                escape_formula(&s)
            } else {
                s
            }
        }
    }
}

pub fn csv_row(doc: &Document, fields: &[String], escape_formulas: bool) -> Vec<String> {
    fields
        .iter()
        .map(|f| csv_cell(lookup(doc, f), escape_formulas))
        .collect()
}

pub struct CsvWriter<W: Write> {
    inner: csv::Writer<W>,
    fields: Vec<String>,
    escape_formulas: bool,
}

impl<W: Write> CsvWriter<W> {
    pub fn new(w: W, fields: Vec<String>, delimiter: u8, escape_formulas: bool) -> Result<Self> {
        let mut inner = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(w);
        inner.write_record(&fields).context("write CSV header")?;
        Ok(Self {
            inner,
            fields,
            escape_formulas,
        })
    }

    pub fn write(&mut self, doc: &Document) -> Result<()> {
        self.inner
            .write_record(csv_row(doc, &self.fields, self.escape_formulas))
            .context("write CSV row")
    }

    pub fn finish(mut self) -> Result<()> {
        self.inner.flush().context("flush CSV")
    }
}

pub const DELIMITERS: [(&str, u8); 4] = [
    ("Comma", b','),
    ("Semicolon", b';'),
    ("Tab", b'\t'),
    ("Pipe", b'|'),
];

// ----- running ----------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Source {
    /// Every document, natural order.
    Full,
    /// The query bar as run (its projection, sort, skip and limit apply).
    Find(Box<FindSpec>),
    Aggregate(Vec<Document>, AggOpts),
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Full => "the whole collection",
            Source::Find(_) => "the current query",
            Source::Aggregate(..) => "the aggregation results",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Format {
    Json {
        mode: JsonMode,
        ndjson: bool,
    },
    Csv {
        fields: Vec<String>,
        delimiter: u8,
        escape_formulas: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ExportSpec {
    pub source: Source,
    pub format: Format,
    pub path: PathBuf,
}

async fn cursor(
    client: &Client,
    ns: &Namespace,
    source: &Source,
    ctx: &OpCtx,
) -> Result<mongodb::Cursor<Document>> {
    match source {
        Source::Full => ops::find_cursor(client, ns, &FindSpec::default(), ctx).await,
        Source::Find(spec) => ops::find_cursor(client, ns, spec, ctx).await,
        Source::Aggregate(pipeline, opts) => {
            ops::aggregate_cursor(client, ns, pipeline.clone(), opts, ctx).await
        }
    }
}

/// The first `n` documents of a source: what the CSV field picker sees.
pub async fn sample(
    client: &Client,
    ns: &Namespace,
    source: &Source,
    n: u64,
    ctx: &OpCtx,
) -> Result<Vec<Document>> {
    let source = match source {
        Source::Full => Source::Find(Box::new(FindSpec {
            limit: n,
            ..Default::default()
        })),
        Source::Find(spec) => Source::Find(Box::new(FindSpec {
            limit: if spec.limit > 0 { spec.limit.min(n) } else { n },
            ..(**spec).clone()
        })),
        Source::Aggregate(p, opts) => {
            let mut p = p.clone();
            if !super::pipeline::Pipeline::from_documents(&p)
                .map(|pl| pl.has_write_stage())
                .unwrap_or(false)
            {
                p.push(bson::doc! { "$limit": n as i64 });
            }
            Source::Aggregate(p, opts.clone())
        }
    };
    cursor(client, ns, &source, ctx)
        .await?
        .try_collect()
        .await
        .with_context(|| format!("reading {} failed", ns))
}

/// Stream the source into the file. `progress` is called every few hundred
/// documents with the count so far. Returns the number written.
pub async fn run(
    client: &Client,
    ns: &Namespace,
    spec: ExportSpec,
    ctx: &OpCtx,
    progress: impl Fn(u64) + Send,
) -> Result<u64> {
    let file = std::fs::File::create(&spec.path)
        .with_context(|| format!("create {}", spec.path.display()))?;
    let w = std::io::BufWriter::new(file);
    let mut cur = cursor(client, ns, &spec.source, ctx).await?;
    let mut n = 0u64;
    enum Sink<W: Write> {
        Json(JsonWriter<W>),
        Csv(Box<CsvWriter<W>>),
    }
    let mut sink = match &spec.format {
        Format::Json { mode, ndjson } => Sink::Json(JsonWriter::new(w, *mode, *ndjson)),
        Format::Csv {
            fields,
            delimiter,
            escape_formulas,
        } => Sink::Csv(Box::new(CsvWriter::new(
            w,
            fields.clone(),
            *delimiter,
            *escape_formulas,
        )?)),
    };
    while let Some(doc) = cur
        .try_next()
        .await
        .with_context(|| format!("reading {} failed", ns))?
    {
        match &mut sink {
            Sink::Json(j) => j.write(&doc).context("write JSON")?,
            Sink::Csv(c) => c.write(&doc)?,
        }
        n += 1;
        if n.is_multiple_of(200) {
            progress(n);
        }
    }
    match sink {
        Sink::Json(j) => {
            j.finish().context("write JSON")?;
        }
        Sink::Csv(c) => c.finish()?,
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn doc() -> Document {
        doc! {
            "_id": bson::oid::ObjectId::parse_str("65f000000000000000000001").unwrap(),
            "name": "Ann",
            "n": 3i32,
            "big": 9007199254740993i64,
            "f": 1.5f64,
            "when": bson::DateTime::from_millis(0),
            "tags": ["a", "=b"],
            "addr": { "city": "Oslo", "geo": [1.0, 2.0] },
            "nothing": Bson::Null,
            "empty": {},
        }
    }

    #[test]
    fn json_modes() {
        let d = doc();
        let def = json_text(&d, JsonMode::Default, false);
        assert!(def.contains("\"big\":{\"$numberLong\":\"9007199254740993\"}"));
        assert!(def.contains("\"n\":3,"));
        assert!(def.contains("\"$oid\":\"65f000000000000000000001\""));
        assert!(def.contains("\"$date\":\"1970-01-01T00:00:00Z\""));
        let relaxed = json_text(&d, JsonMode::Relaxed, false);
        assert!(relaxed.contains("\"big\":9007199254740993"));
        let canonical = json_text(&d, JsonMode::Canonical, false);
        assert!(canonical.contains("\"n\":{\"$numberInt\":\"3\"}"));
        assert!(canonical.contains("\"$numberDouble\":\"1.5\""));
        // Nested Int64 inside arrays/documents keeps $numberLong in Default.
        let nested = doc! { "a": [ { "b": 5i64 } ] };
        assert_eq!(
            json_text(&nested, JsonMode::Default, false),
            "{\"a\":[{\"b\":{\"$numberLong\":\"5\"}}]}"
        );
    }

    #[test]
    fn json_writer_array_and_ndjson() {
        let mut w = JsonWriter::new(Vec::new(), JsonMode::Relaxed, false);
        w.write(&doc! { "a": 1 }).unwrap();
        w.write(&doc! { "b": 2 }).unwrap();
        let out = String::from_utf8(w.finish().unwrap()).unwrap();
        assert_eq!(
            out,
            "[\n  {\n    \"a\": 1\n  },\n  {\n    \"b\": 2\n  }\n]\n"
        );
        let parsed: Vec<Document> = crate::mongo::ejson::parse_documents(&out).unwrap();
        assert_eq!(parsed.len(), 2);
        let empty = JsonWriter::new(Vec::new(), JsonMode::Relaxed, false);
        assert_eq!(
            String::from_utf8(empty.finish().unwrap()).unwrap(),
            "[\n]\n"
        );
        let mut nd = JsonWriter::new(Vec::new(), JsonMode::Relaxed, true);
        nd.write(&doc! { "a": 1 }).unwrap();
        nd.write(&doc! { "b": 2 }).unwrap();
        assert_eq!(
            String::from_utf8(nd.finish().unwrap()).unwrap(),
            "{\"a\":1}\n{\"b\":2}\n"
        );
    }

    #[test]
    fn csv_fields_and_lookup() {
        let d = doc();
        let fields = csv_fields(&[d.clone(), doc! { "extra": 1, "name": "x" }]);
        assert_eq!(
            fields,
            vec![
                "_id",
                "name",
                "n",
                "big",
                "f",
                "when",
                "tags.0",
                "tags.1",
                "addr.city",
                "addr.geo.0",
                "addr.geo.1",
                "nothing",
                "empty",
                "extra"
            ]
        );
        assert_eq!(lookup(&d, "addr.city"), Some(&Bson::String("Oslo".into())));
        assert_eq!(lookup(&d, "addr.geo.1"), Some(&Bson::Double(2.0)));
        assert_eq!(lookup(&d, "tags.5"), None);
        assert_eq!(lookup(&d, "name.x"), None);
        assert_eq!(lookup(&d, "missing"), None);
    }

    #[test]
    fn csv_values_and_escaping() {
        let d = doc();
        let fields: Vec<String> = [
            "_id", "name", "n", "big", "f", "when", "tags.1", "nothing", "empty", "addr",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let row = csv_row(&d, &fields, true);
        assert_eq!(
            row,
            vec![
                "65f000000000000000000001",
                "Ann",
                "3",
                "9007199254740993",
                "1.5",
                "1970-01-01T00:00:00Z",
                "'=b",
                "",
                "{}",
                "{\"city\":\"Oslo\",\"geo\":[1.0,2.0]}",
            ]
        );
        let raw = csv_row(&d, &fields, false);
        assert_eq!(raw[6], "=b");
        assert_eq!(escape_formula("+1"), "'+1");
        assert_eq!(escape_formula("-1"), "'-1");
        assert_eq!(escape_formula("@x"), "'@x");
        assert_eq!(escape_formula("\tx"), "'\tx");
        assert_eq!(escape_formula("plain"), "plain");
        // Numbers are never escaped, even negative ones.
        assert_eq!(csv_cell(Some(&Bson::Int32(-5)), true), "-5");
    }

    #[test]
    fn csv_writer_quotes() {
        let fields = vec!["a".to_string(), "b".to_string()];
        let mut w = CsvWriter::new(Vec::new(), fields, b',', true).unwrap();
        w.write(&doc! { "a": "x, y", "b": "say \"hi\"\nthere" })
            .unwrap();
        w.write(&doc! { "a": 1 }).unwrap();
        let inner = w.inner.into_inner().unwrap();
        let out = String::from_utf8(inner).unwrap();
        assert_eq!(out, "a,b\n\"x, y\",\"say \"\"hi\"\"\nthere\"\n1,\n");
        let fields = vec!["a".to_string()];
        let mut w = CsvWriter::new(Vec::new(), fields, b';', false).unwrap();
        w.write(&doc! { "a": "1;2" }).unwrap();
        let out = String::from_utf8(w.inner.into_inner().unwrap()).unwrap();
        assert_eq!(out, "a\n\"1;2\"\n");
    }
}

/// Export → import → validation round-trip against a live server; skipped
/// unless `VITI_TEST_URI` is set (e.g. `mongodb://localhost:27017`).
#[cfg(test)]
mod live {
    use super::*;
    use crate::mongo::import::{self, Column, CsvOptions, ImportSpec};
    use crate::mongo::validation::{self, Validation};
    use bson::doc;

    #[tokio::test]
    async fn export_import_validation_round_trip() {
        let Ok(uri) = std::env::var("VITI_TEST_URI") else {
            eprintln!("VITI_TEST_URI not set; skipping");
            return;
        };
        let client = Client::with_uri_str(&uri).await.unwrap();
        let db = format!("viti_it_{}", uuid::Uuid::new_v4().simple());
        let ns = Namespace::new(&db, "c");
        let ctx = OpCtx::new(10_000);
        let docs: Vec<Document> = (0..2500i32)
            .map(|i| {
                doc! {
                    "_id": i,
                    "name": format!("n{i}"),
                    "big": (i as i64) << 40,
                    "when": bson::DateTime::from_millis(i as i64 * 1000),
                    "addr": { "city": "Oslo" },
                    "tags": ["a", "=b"],
                }
            })
            .collect();
        ops::insert_many(&client, &ns, docs).await.unwrap();
        let dir = tempfile::tempdir().unwrap();

        // Full collection as a Default Extended JSON array…
        let json_path = dir.path().join("out.json");
        let n = run(
            &client,
            &ns,
            ExportSpec {
                source: Source::Full,
                format: Format::Json {
                    mode: JsonMode::Default,
                    ndjson: false,
                },
                path: json_path.clone(),
            },
            &ctx,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(n, 2500);

        // …imported into a second collection with every type intact.
        let ns2 = Namespace::new(&db, "c2");
        let json_spec = ImportSpec {
            path: json_path.clone(),
            format: import::Format::Json,
            csv: None,
            ignore_empty: true,
            stop_on_error: true,
        };
        let report = import::run(&client, &ns2, json_spec.clone(), |_, _| {})
            .await
            .unwrap();
        assert_eq!((report.inserted, report.failed), (2500, 0));
        let d = ops::sample(&client, &ns2, doc! { "_id": 5 }, 1, &ctx)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(d.get("big"), Some(&Bson::Int64(5i64 << 40)));
        assert!(matches!(d.get("when"), Some(Bson::DateTime(_))));
        assert_eq!(
            d.get_document("addr").unwrap().get_str("city").unwrap(),
            "Oslo"
        );

        // Importing again: duplicates are reported, unordered keeps going.
        let report = import::run(
            &client,
            &ns2,
            ImportSpec {
                stop_on_error: false,
                ..json_spec.clone()
            },
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!((report.inserted, report.failed), (0, 2500));
        assert_eq!(report.errors.len(), 25);
        assert!(report.errors[0].starts_with("row 1:"));
        assert!(!report.stopped);
        let report = import::run(&client, &ns2, json_spec, |_, _| {})
            .await
            .unwrap();
        assert!(report.stopped);
        assert_eq!(report.inserted, 0);
        assert_eq!(report.failed, 1);

        // A query as CSV, fields discovered from a sample…
        let csv_path = dir.path().join("out.csv");
        let spec = FindSpec {
            filter: doc! { "_id": { "$lt": 10 } },
            sort: Some(doc! { "_id": 1 }),
            ..Default::default()
        };
        let sampled = sample(
            &client,
            &ns,
            &Source::Find(Box::new(spec.clone())),
            50,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(sampled.len(), 10);
        let fields = csv_fields(&sampled);
        assert!(fields.contains(&"addr.city".to_string()));
        assert!(fields.contains(&"tags.1".to_string()));
        let n = run(
            &client,
            &ns,
            ExportSpec {
                source: Source::Find(Box::new(spec)),
                format: Format::Csv {
                    fields,
                    delimiter: b',',
                    escape_formulas: true,
                },
                path: csv_path.clone(),
            },
            &ctx,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(n, 10);

        // …imported with the guessed column types.
        let pv = import::preview_csv(&csv_path, None, 20).unwrap();
        assert_eq!(pv.delimiter, b',');
        let columns: Vec<Column> = pv
            .headers
            .iter()
            .zip(pv.guessed.iter())
            .map(|(h, t)| Column {
                name: h.clone(),
                field_type: *t,
                include: true,
            })
            .collect();
        let ns3 = Namespace::new(&db, "c3");
        let report = import::run(
            &client,
            &ns3,
            ImportSpec {
                path: csv_path,
                format: import::Format::Csv,
                csv: Some(CsvOptions {
                    delimiter: pv.delimiter,
                    columns,
                }),
                ignore_empty: true,
                stop_on_error: true,
            },
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!((report.inserted, report.failed), (10, 0));
        let d = ops::sample(&client, &ns3, doc! { "_id": 3 }, 1, &ctx)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            d.get_document("addr").unwrap().get_str("city").unwrap(),
            "Oslo"
        );
        assert_eq!(d.get_document("tags").unwrap().get_str("1").unwrap(), "'=b");
        assert_eq!(d.get("big"), Some(&Bson::Int64(3i64 << 40)));
        assert!(matches!(d.get("when"), Some(Bson::DateTime(_))));

        // Aggregation results, one per line.
        let agg_path = dir.path().join("agg.ndjson");
        let n = run(
            &client,
            &ns,
            ExportSpec {
                source: Source::Aggregate(
                    vec![
                        doc! { "$match": { "_id": { "$lt": 3 } } },
                        doc! { "$project": { "name": 1 } },
                    ],
                    AggOpts::default(),
                ),
                format: Format::Json {
                    mode: JsonMode::Relaxed,
                    ndjson: true,
                },
                path: agg_path.clone(),
            },
            &ctx,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            std::fs::read_to_string(&agg_path).unwrap().lines().count(),
            3
        );

        // Random sample, schema analysis, and validation rules.
        let s = ops::sample_random(&client, &ns, Document::new(), 100, &ctx)
            .await
            .unwrap();
        assert_eq!(s.len(), 100);
        let v0 = validation::fetch(&client, &ns).await.unwrap();
        assert!(v0.validator.is_empty());
        let v = Validation {
            validator: doc! { "$jsonSchema": { "bsonType": "object", "required": ["name"] } },
            level: "moderate".into(),
            action: "warn".into(),
        };
        validation::set(&client, &ns, &v).await.unwrap();
        assert_eq!(validation::fetch(&client, &ns).await.unwrap(), v);
        let generated = validation::json_schema(&crate::mongo::schema::analyze(&s));
        validation::set(
            &client,
            &ns,
            &Validation {
                validator: generated.clone(),
                ..v
            },
        )
        .await
        .unwrap();
        // Every document passes its own generated schema.
        let failing = ops::sample(&client, &ns, doc! { "$nor": [generated] }, 1, &ctx)
            .await
            .unwrap();
        assert!(failing.is_empty());

        client.database(&db).drop().await.unwrap();
    }
}
