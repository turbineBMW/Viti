//! Import JSON (array, single document or one per line) and CSV files. CSV
//! columns get a type each (guessed from a preview, editable), dotted headers
//! become nested documents, empty cells can be dropped, and errors either stop
//! the import or are collected in the report. Conversion is pure and tested;
//! `run` inserts in batches on tokio, reporting progress.
use super::ejson;
use super::ops::Namespace;
use anyhow::{Context, Result};
use bson::{Bson, Document};
use mongodb::Client;
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub const BATCH: usize = 1000;
/// Errors kept in the report; the rest are only counted.
const MAX_ERRORS: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
}

/// By extension: `.csv`/`.tsv` are CSV, everything else JSON.
pub fn format_for_path(path: &Path) -> Format {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("csv") | Some("tsv") => Format::Csv,
        _ => Format::Json,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldType {
    #[default]
    String,
    /// Int32 when it fits, else Int64, else Double.
    Number,
    Int32,
    Int64,
    Double,
    Decimal128,
    Boolean,
    Date,
    ObjectId,
    Null,
    /// The cell is Extended JSON (mongosh syntax accepted).
    Json,
    /// Per cell: number, boolean, null or date when it looks like one, else string.
    Mixed,
}

impl FieldType {
    pub const ALL: [FieldType; 12] = [
        FieldType::String,
        FieldType::Number,
        FieldType::Int32,
        FieldType::Int64,
        FieldType::Double,
        FieldType::Decimal128,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::ObjectId,
        FieldType::Null,
        FieldType::Json,
        FieldType::Mixed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FieldType::String => "String",
            FieldType::Number => "Number",
            FieldType::Int32 => "Int32",
            FieldType::Int64 => "Int64",
            FieldType::Double => "Double",
            FieldType::Decimal128 => "Decimal128",
            FieldType::Boolean => "Boolean",
            FieldType::Date => "Date",
            FieldType::ObjectId => "ObjectId",
            FieldType::Null => "Null",
            FieldType::Json => "JSON",
            FieldType::Mixed => "Mixed",
        }
    }

    pub fn index(self) -> u32 {
        FieldType::ALL.iter().position(|t| *t == self).unwrap_or(0) as u32
    }

    pub fn from_index(i: u32) -> FieldType {
        FieldType::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    pub field_type: FieldType,
    pub include: bool,
}

#[derive(Clone, Debug)]
pub struct CsvOptions {
    pub delimiter: u8,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub path: PathBuf,
    pub format: Format,
    pub csv: Option<CsvOptions>,
    /// Drop empty CSV cells instead of importing them as `""`.
    pub ignore_empty: bool,
    /// Abort on the first bad row or rejected insert.
    pub stop_on_error: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub inserted: u64,
    pub failed: u64,
    /// The first few problems, `row N: …`.
    pub errors: Vec<String>,
    /// Stopped early because of `stop_on_error`.
    pub stopped: bool,
}

impl Report {
    fn push_error(&mut self, msg: String) {
        self.failed += 1;
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(msg);
        }
    }
}

// ----- sniffing ---------------------------------------------------------------

/// The delimiter that splits the header into the most columns.
pub fn sniff_delimiter(header: &str) -> u8 {
    // Reversed so a comma wins ties (`max_by_key` keeps the last maximum).
    b"|\t;,"
        .iter()
        .copied()
        .max_by_key(|d| header.matches(*d as char).count())
        .unwrap_or(b',')
}

#[derive(Clone, Debug, Default)]
pub struct Preview {
    pub delimiter: u8,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub guessed: Vec<FieldType>,
}

/// The header, the first `rows` records and a type guess per column.
pub fn preview_csv(path: &Path, delimiter: Option<u8>, rows: usize) -> Result<Preview> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first).context("read header")?;
    let delimiter = delimiter.unwrap_or_else(|| sniff_delimiter(&first));
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(file);
    let headers: Vec<String> = rdr
        .headers()
        .context("read CSV header")?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    let mut out: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records().take(rows) {
        let rec = rec.context("read CSV row")?;
        out.push(rec.iter().map(str::to_string).collect());
    }
    let guessed = (0..headers.len())
        .map(|i| guess_type(out.iter().filter_map(|r| r.get(i).map(String::as_str))))
        .collect();
    Ok(Preview {
        delimiter,
        headers,
        rows: out,
        guessed,
    })
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "t" => Some(true),
        "false" | "no" | "n" | "0" | "f" => Some(false),
        _ => None,
    }
}

/// ISO 8601 / RFC 3339 (`2024-01-02`, `2024-01-02T03:04:05Z`, with offset or
/// fractional seconds, `T` or space) or milliseconds since the epoch.
pub fn parse_date(s: &str) -> Option<bson::DateTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(bson::DateTime::from_millis(dt.timestamp_millis()));
    }
    let with_t = s.replacen(' ', "T", 1);
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&with_t) {
        return Some(bson::DateTime::from_millis(dt.timestamp_millis()));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&with_t, fmt) {
            return Some(bson::DateTime::from_millis(
                naive.and_utc().timestamp_millis(),
            ));
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(bson::DateTime::from_millis(
            d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis(),
        ));
    }
    if s.len() >= 12
        && let Ok(ms) = s.parse::<i64>()
    {
        return Some(bson::DateTime::from_millis(ms));
    }
    None
}

fn parse_number(s: &str) -> Option<Bson> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i32>() {
        return Some(Bson::Int32(i));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Bson::Int64(i));
    }
    if let Ok(f) = s.parse::<f64>()
        && f.is_finite()
        && !s.eq_ignore_ascii_case("nan")
    {
        return Some(Bson::Double(f));
    }
    None
}

/// The narrowest type every non-empty value fits; `String` when they disagree.
pub fn guess_type<'a>(values: impl Iterator<Item = &'a str>) -> FieldType {
    let mut seen = false;
    let mut candidates = vec![
        FieldType::Int32,
        FieldType::Int64,
        FieldType::Double,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::ObjectId,
        FieldType::Json,
    ];
    for v in values {
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        seen = true;
        candidates.retain(|t| convert(v, *t).is_ok());
        if candidates.is_empty() {
            break;
        }
    }
    if !seen {
        return FieldType::String;
    }
    // Prefer a scalar type over JSON (`1` parses as both).
    candidates
        .iter()
        .find(|t| **t != FieldType::Json)
        .or(candidates.first())
        .copied()
        .unwrap_or(FieldType::String)
}

/// Convert one cell. Errors name what was expected.
pub fn convert(value: &str, t: FieldType) -> Result<Bson, String> {
    let v = value.trim();
    match t {
        FieldType::String => Ok(Bson::String(value.to_string())),
        FieldType::Number => parse_number(v).ok_or_else(|| format!("`{v}` is not a number")),
        FieldType::Int32 => v
            .parse::<i32>()
            .map(Bson::Int32)
            .map_err(|_| format!("`{v}` is not a 32-bit integer")),
        FieldType::Int64 => v
            .parse::<i64>()
            .map(Bson::Int64)
            .map_err(|_| format!("`{v}` is not a 64-bit integer")),
        FieldType::Double => v
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(Bson::Double)
            .ok_or_else(|| format!("`{v}` is not a number")),
        FieldType::Decimal128 => v
            .parse::<bson::Decimal128>()
            .map(Bson::Decimal128)
            .map_err(|_| format!("`{v}` is not a decimal")),
        FieldType::Boolean => parse_bool(v)
            .map(Bson::Boolean)
            .ok_or_else(|| format!("`{v}` is not a boolean")),
        FieldType::Date => parse_date(v)
            .map(Bson::DateTime)
            .ok_or_else(|| format!("`{v}` is not an ISO 8601 date")),
        FieldType::ObjectId => bson::oid::ObjectId::parse_str(v)
            .map(Bson::ObjectId)
            .map_err(|_| format!("`{v}` is not a 24-hex-digit ObjectId")),
        FieldType::Null => Ok(Bson::Null),
        FieldType::Json => {
            if !v.starts_with(['{', '[']) {
                return Err(format!("`{v}` is not a JSON object or array"));
            }
            ejson::parse_value(v).map_err(|e| e.to_string())
        }
        FieldType::Mixed => {
            if v.is_empty() {
                return Ok(Bson::String(String::new()));
            }
            if matches!(v, "true" | "false" | "TRUE" | "FALSE" | "True" | "False") {
                return Ok(Bson::Boolean(v.eq_ignore_ascii_case("true")));
            }
            if v.eq_ignore_ascii_case("null") {
                return Ok(Bson::Null);
            }
            if let Some(n) = parse_number(v) {
                return Ok(n);
            }
            if v.len() >= 10
                && v.as_bytes()[4] == b'-'
                && let Some(d) = parse_date(v)
            {
                return Ok(Bson::DateTime(d));
            }
            Ok(Bson::String(value.to_string()))
        }
    }
}

/// Set `a.b.c` in a document, creating intermediate documents.
pub fn insert_path(doc: &mut Document, path: &str, value: Bson) {
    let mut parts = path.split('.').filter(|p| !p.is_empty()).peekable();
    let mut cur = doc;
    while let Some(seg) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(seg, value);
            return;
        }
        if !matches!(cur.get(seg), Some(Bson::Document(_))) {
            cur.insert(seg, Document::new());
        }
        cur = cur.get_document_mut(seg).expect("just inserted");
    }
}

/// One CSV record to a document. Excluded columns are skipped; empty cells
/// are dropped when `ignore_empty`, else imported per their type (`""` for
/// strings, an error for most others).
pub fn row_to_document(
    columns: &[Column],
    row: &csv::StringRecord,
    ignore_empty: bool,
) -> Result<Document, String> {
    let mut doc = Document::new();
    for (i, col) in columns.iter().enumerate() {
        if !col.include || col.name.is_empty() {
            continue;
        }
        let Some(cell) = row.get(i) else { continue };
        if cell.trim().is_empty() {
            if ignore_empty {
                continue;
            }
            if matches!(
                col.field_type,
                FieldType::String | FieldType::Mixed | FieldType::Null
            ) {
                insert_path(
                    &mut doc,
                    &col.name,
                    if col.field_type == FieldType::Null {
                        Bson::Null
                    } else {
                        Bson::String(String::new())
                    },
                );
                continue;
            }
            return Err(format!("{}: empty", col.name));
        }
        let v = convert(cell, col.field_type).map_err(|e| format!("{}: {e}", col.name))?;
        insert_path(&mut doc, &col.name, v);
    }
    Ok(doc)
}

/// A JSON file's documents: an array, one document per line, or a single
/// (possibly multi-line) document. mongosh syntax is accepted throughout.
pub fn json_documents(text: &str) -> Result<Vec<Document>, String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        return ejson::parse_documents(trimmed).map_err(|e| e.to_string());
    }
    let lines: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > 1
        && lines
            .iter()
            .all(|l| l.starts_with('{') && l.trim_end_matches(',').ends_with('}'))
    {
        return lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                ejson::parse_document(l.trim_end_matches(','))
                    .map_err(|e| format!("line {}: {}", i + 1, e.msg))
            })
            .collect();
    }
    ejson::parse_documents(trimmed).map_err(|e| e.to_string())
}

// ----- running ----------------------------------------------------------------

/// Insert one batch; returns (inserted, errors). With `ordered` the first
/// failure stops the batch, otherwise every document is attempted.
async fn insert_batch(
    coll: &mongodb::Collection<Document>,
    docs: Vec<Document>,
    ordered: bool,
    offset: u64,
) -> (u64, Vec<String>) {
    let total = docs.len();
    match coll.insert_many(docs).ordered(ordered).await {
        Ok(r) => (r.inserted_ids.len() as u64, Vec::new()),
        Err(e) => match *e.kind {
            mongodb::error::ErrorKind::InsertMany(ref im) => {
                let write_errors = im.write_errors.as_deref().unwrap_or_default();
                let mut errors: Vec<String> = write_errors
                    .iter()
                    .map(|w| format!("row {}: {}", offset + w.index as u64 + 1, w.message))
                    .collect();
                if let Some(wc) = &im.write_concern_error {
                    errors.push(format!("write concern: {}", wc.message));
                }
                // The driver keeps the inserted ids private: ordered inserts
                // stop at the first failure, unordered ones skip only the
                // failed documents.
                let inserted = if ordered {
                    write_errors.iter().map(|w| w.index).min().unwrap_or(total)
                } else {
                    total.saturating_sub(write_errors.len())
                };
                (inserted as u64, errors)
            }
            _ => (0, vec![e.to_string()]),
        },
    }
}

/// Import the file. `progress(done, total)` is called per batch; `total` is
/// the number of records when known. The report counts inserted and failed
/// documents; an `Err` is a problem with the file itself.
pub async fn run(
    client: &Client,
    ns: &Namespace,
    spec: ImportSpec,
    progress: impl Fn(u64, Option<u64>) + Send,
) -> Result<Report> {
    let coll = client.database(&ns.db).collection::<Document>(&ns.coll);
    let mut report = Report::default();
    let ordered = spec.stop_on_error;
    let mut batch: Vec<Document> = Vec::with_capacity(BATCH);
    let mut seen = 0u64;

    macro_rules! flush {
        () => {
            if !batch.is_empty() {
                let docs = std::mem::take(&mut batch);
                let offset = seen - docs.len() as u64;
                let (n, errors) = insert_batch(&coll, docs, ordered, offset).await;
                report.inserted += n;
                let had_errors = !errors.is_empty();
                for e in errors {
                    report.push_error(e);
                }
                if had_errors && spec.stop_on_error {
                    report.stopped = true;
                }
            }
        };
    }

    match spec.format {
        Format::Json => {
            let text = std::fs::read_to_string(&spec.path)
                .with_context(|| format!("read {}", spec.path.display()))?;
            let docs = json_documents(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
            let total = docs.len() as u64;
            progress(0, Some(total));
            for d in docs {
                seen += 1;
                batch.push(d);
                if batch.len() >= BATCH {
                    flush!();
                    progress(seen, Some(total));
                    if report.stopped {
                        return Ok(report);
                    }
                }
            }
            flush!();
            progress(seen, Some(total));
        }
        Format::Csv => {
            let csv = spec
                .csv
                .clone()
                .ok_or_else(|| anyhow::anyhow!("CSV options missing"))?;
            let total = {
                let f = std::fs::File::open(&spec.path)
                    .with_context(|| format!("open {}", spec.path.display()))?;
                let lines = std::io::BufReader::new(f).lines().count() as u64;
                Some(lines.saturating_sub(1))
            };
            progress(0, total);
            let f = std::fs::File::open(&spec.path)
                .with_context(|| format!("open {}", spec.path.display()))?;
            let mut rdr = csv::ReaderBuilder::new()
                .delimiter(csv.delimiter)
                .flexible(true)
                .from_reader(std::io::BufReader::new(f));
            for rec in rdr.records() {
                seen += 1;
                let rec = match rec {
                    Ok(r) => r,
                    Err(e) => {
                        report.push_error(format!("row {seen}: {e}"));
                        if spec.stop_on_error {
                            report.stopped = true;
                            break;
                        }
                        continue;
                    }
                };
                match row_to_document(&csv.columns, &rec, spec.ignore_empty) {
                    Ok(d) => batch.push(d),
                    Err(e) => {
                        report.push_error(format!("row {seen}: {e}"));
                        if spec.stop_on_error {
                            report.stopped = true;
                            break;
                        }
                    }
                }
                if batch.len() >= BATCH {
                    flush!();
                    progress(seen, total);
                    if report.stopped {
                        return Ok(report);
                    }
                }
            }
            flush!();
            progress(seen, total);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_format_and_delimiter() {
        assert_eq!(format_for_path(Path::new("a.CSV")), Format::Csv);
        assert_eq!(format_for_path(Path::new("a.tsv")), Format::Csv);
        assert_eq!(format_for_path(Path::new("a.json")), Format::Json);
        assert_eq!(format_for_path(Path::new("a.ndjson")), Format::Json);
        assert_eq!(sniff_delimiter("a,b,c"), b',');
        assert_eq!(sniff_delimiter("a;b;c,d"), b';');
        assert_eq!(sniff_delimiter("a\tb\tc"), b'\t');
        assert_eq!(sniff_delimiter("a"), b',');
    }

    #[test]
    fn converts() {
        assert_eq!(convert("3", FieldType::Number).unwrap(), Bson::Int32(3));
        assert_eq!(
            convert("3000000000", FieldType::Number).unwrap(),
            Bson::Int64(3_000_000_000)
        );
        assert_eq!(
            convert("1.5", FieldType::Number).unwrap(),
            Bson::Double(1.5)
        );
        assert!(convert("x", FieldType::Number).is_err());
        assert_eq!(convert("7", FieldType::Int32).unwrap(), Bson::Int32(7));
        assert!(convert("3000000000", FieldType::Int32).is_err());
        assert_eq!(convert("7", FieldType::Double).unwrap(), Bson::Double(7.0));
        assert_eq!(
            convert("yes", FieldType::Boolean).unwrap(),
            Bson::Boolean(true)
        );
        assert!(convert("maybe", FieldType::Boolean).is_err());
        assert_eq!(
            convert("2024-01-02", FieldType::Date).unwrap(),
            Bson::DateTime(bson::DateTime::from_millis(1_704_153_600_000))
        );
        assert_eq!(
            convert("2024-01-02T03:04:05Z", FieldType::Date).unwrap(),
            Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_000))
        );
        assert_eq!(
            convert("2024-01-02 03:04:05", FieldType::Date).unwrap(),
            Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_000))
        );
        assert_eq!(
            convert("1704164645000", FieldType::Date).unwrap(),
            Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_000))
        );
        assert!(convert("yesterday", FieldType::Date).is_err());
        assert!(matches!(
            convert("65f000000000000000000001", FieldType::ObjectId).unwrap(),
            Bson::ObjectId(_)
        ));
        assert!(convert("nope", FieldType::ObjectId).is_err());
        assert_eq!(convert("anything", FieldType::Null).unwrap(), Bson::Null);
        assert_eq!(
            convert("{ a: 1 }", FieldType::Json).unwrap(),
            Bson::Document(bson::doc! { "a": 1 })
        );
        assert!(convert("1", FieldType::Json).is_err());
        assert_eq!(
            convert("12.5", FieldType::Decimal128).unwrap(),
            Bson::Decimal128("12.5".parse().unwrap())
        );
        // Mixed
        assert_eq!(
            convert("true", FieldType::Mixed).unwrap(),
            Bson::Boolean(true)
        );
        assert_eq!(convert("1", FieldType::Mixed).unwrap(), Bson::Int32(1));
        assert_eq!(convert("null", FieldType::Mixed).unwrap(), Bson::Null);
        assert!(matches!(
            convert("2024-01-02", FieldType::Mixed).unwrap(),
            Bson::DateTime(_)
        ));
        assert_eq!(
            convert("yes", FieldType::Mixed).unwrap(),
            Bson::String("yes".into())
        );
        assert_eq!(
            convert("hello", FieldType::Mixed).unwrap(),
            Bson::String("hello".into())
        );
    }

    #[test]
    fn guesses() {
        assert_eq!(guess_type(["1", "2", ""].into_iter()), FieldType::Int32);
        assert_eq!(
            guess_type(["1", "3000000000"].into_iter()),
            FieldType::Int64
        );
        assert_eq!(guess_type(["1", "2.5"].into_iter()), FieldType::Double);
        assert_eq!(guess_type(["true", "no"].into_iter()), FieldType::Boolean);
        assert_eq!(
            guess_type(["2024-01-02", "2024-02-03T00:00:00Z"].into_iter()),
            FieldType::Date
        );
        assert_eq!(
            guess_type(["65f000000000000000000001"].into_iter()),
            FieldType::ObjectId
        );
        assert_eq!(
            guess_type(["{\"a\":1}", "[1]"].into_iter()),
            FieldType::Json
        );
        assert_eq!(guess_type(["1", "x"].into_iter()), FieldType::String);
        assert_eq!(guess_type(["", ""].into_iter()), FieldType::String);
        assert_eq!(guess_type(std::iter::empty()), FieldType::String);
    }

    #[test]
    fn rows_to_documents() {
        let columns = vec![
            Column {
                name: "name".into(),
                field_type: FieldType::String,
                include: true,
            },
            Column {
                name: "age".into(),
                field_type: FieldType::Int32,
                include: true,
            },
            Column {
                name: "address.city".into(),
                field_type: FieldType::String,
                include: true,
            },
            Column {
                name: "secret".into(),
                field_type: FieldType::String,
                include: false,
            },
        ];
        let rec = csv::StringRecord::from(vec!["Ann", "3", "Oslo", "x"]);
        let d = row_to_document(&columns, &rec, true).unwrap();
        assert_eq!(
            d,
            bson::doc! { "name": "Ann", "age": 3, "address": { "city": "Oslo" } }
        );
        let rec = csv::StringRecord::from(vec!["Ann", "", "", "x"]);
        let d = row_to_document(&columns, &rec, true).unwrap();
        assert_eq!(d, bson::doc! { "name": "Ann" });
        let d = row_to_document(&columns, &rec, false);
        assert_eq!(d.unwrap_err(), "age: empty");
        let rec = csv::StringRecord::from(vec!["Ann", "old", "", "x"]);
        assert!(
            row_to_document(&columns, &rec, true)
                .unwrap_err()
                .starts_with("age: `old`")
        );
        // Short rows are fine (flexible CSV).
        let rec = csv::StringRecord::from(vec!["Ann"]);
        assert_eq!(
            row_to_document(&columns, &rec, true).unwrap(),
            bson::doc! { "name": "Ann" }
        );
        let mut d = Document::new();
        insert_path(&mut d, "a.b.c", Bson::Int32(1));
        insert_path(&mut d, "a.b.d", Bson::Int32(2));
        insert_path(&mut d, "a.x", Bson::Int32(3));
        assert_eq!(d, bson::doc! { "a": { "b": { "c": 1, "d": 2 }, "x": 3 } });
    }

    #[test]
    fn json_shapes() {
        assert_eq!(json_documents("[{a:1},{a:2}]").unwrap().len(), 2);
        assert_eq!(
            json_documents("{\"a\":1}\n{\"a\":2}\n\n{a: ObjectId(\"65f000000000000000000001\")}")
                .unwrap()
                .len(),
            3
        );
        assert_eq!(json_documents("{\n  \"a\": 1\n}").unwrap().len(), 1);
        assert_eq!(json_documents("\u{feff}[]").unwrap().len(), 0);
        assert_eq!(json_documents("   ").unwrap().len(), 0);
        assert!(
            json_documents("{a:1}\n{a:")
                .unwrap_err()
                .starts_with("line 2")
        );
        assert!(json_documents("42").is_err());
    }

    #[test]
    fn previews_csv() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.csv");
        std::fs::write(&p, "id;name;when\n1;Ann;2024-01-01\n2;Bob;\n").unwrap();
        let pv = preview_csv(&p, None, 10).unwrap();
        assert_eq!(pv.delimiter, b';');
        assert_eq!(pv.headers, vec!["id", "name", "when"]);
        assert_eq!(pv.rows.len(), 2);
        assert_eq!(
            pv.guessed,
            vec![FieldType::Int32, FieldType::String, FieldType::Date]
        );
    }
}
