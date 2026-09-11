//! Schema analysis over a sample of documents, the way Compass's Schema tab
//! does it: per field path, how often it is present, which BSON types it takes
//! and what its values look like (numbers, dates and strings keep enough of
//! their values to draw a chart and to build a click-to-filter query).
//! Pure: the UI hands in the sampled documents.
use super::ejson::{self, type_name};
use bson::{Bson, Document, doc};
use std::collections::{BTreeMap, HashMap};

/// Distinct string values kept per field/type; more are counted, not stored.
const MAX_DISTINCT_STRINGS: usize = 2000;
/// Numeric / date values kept per field/type for histograms.
const MAX_VALUES: usize = 50_000;
/// Bars in a categorical chart.
pub const TOP_VALUES: usize = 20;
/// Bins in a range histogram.
pub const BINS: usize = 20;

#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub sampled: usize,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Default)]
pub struct Field {
    /// Dotted path from the root (`address.city`).
    pub path: String,
    pub name: String,
    pub depth: usize,
    /// Parent documents analysed (root: the sample size; nested: how many
    /// parent values were documents, counting documents inside arrays).
    pub parent_count: usize,
    /// Parents in which the field is present.
    pub count: usize,
    /// Sorted by count, descending.
    pub types: Vec<TypeStats>,
    /// Fields of nested documents (also documents inside arrays).
    pub children: Vec<Field>,
}

#[derive(Clone, Debug, Default)]
pub struct TypeStats {
    /// As `ejson::type_name` names it (`String`, `Int32`, `Object`, …).
    pub name: &'static str,
    pub count: usize,
    pub values: Values,
    /// Arrays only: the element types, sorted by count.
    pub elements: Vec<TypeStats>,
    /// Arrays only: every length seen.
    pub lengths: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub enum Values {
    #[default]
    None,
    Numbers(Vec<f64>),
    /// value -> occurrences
    Strings(HashMap<String, usize>),
    /// Milliseconds since the epoch.
    Dates(Vec<i64>),
    Booleans {
        t: usize,
        f: usize,
    },
}

impl Field {
    /// Presence in its parent documents, 0..=100.
    pub fn presence_pct(&self) -> f64 {
        if self.parent_count == 0 {
            0.0
        } else {
            self.count as f64 * 100.0 / self.parent_count as f64
        }
    }

    /// `String (80%), Int32 (20%)`; the share is of the parents, so a missing
    /// field shows as `undefined`.
    pub fn types_text(&self) -> String {
        let mut parts: Vec<String> = self
            .types
            .iter()
            .map(|t| format!("{} ({:.0}%)", t.name, self.type_pct(t)))
            .collect();
        let missing = self.parent_count.saturating_sub(self.count);
        if missing > 0 && self.parent_count > 0 {
            parts.push(format!(
                "undefined ({:.0}%)",
                missing as f64 * 100.0 / self.parent_count as f64
            ));
        }
        parts.join(", ")
    }

    pub fn type_pct(&self, t: &TypeStats) -> f64 {
        if self.parent_count == 0 {
            0.0
        } else {
            t.count as f64 * 100.0 / self.parent_count as f64
        }
    }

    /// The most common type.
    pub fn main_type(&self) -> Option<&TypeStats> {
        self.types.first()
    }
}

impl Schema {
    /// Every field, depth first, for a flat list with indentation.
    pub fn flatten(&self) -> Vec<&Field> {
        fn walk<'a>(fields: &'a [Field], out: &mut Vec<&'a Field>) {
            for f in fields {
                out.push(f);
                walk(&f.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.fields, &mut out);
        out
    }

    /// `path: Type (80%), Type (20%)` per field, at most `max` lines: the
    /// compact summary handed to AI prompts.
    pub fn summary_lines(&self, max: usize) -> Vec<String> {
        self.flatten()
            .into_iter()
            .take(max)
            .map(|f| format!("{}: {}", f.path, f.types_text()))
            .collect()
    }
}

// ----- accumulation ----------------------------------------------------------

#[derive(Default)]
struct DocAcc {
    parents: usize,
    fields: BTreeMap<String, FieldAcc>,
}

#[derive(Default)]
struct FieldAcc {
    count: usize,
    types: BTreeMap<&'static str, TypeAcc>,
    /// Nested documents (direct values and array elements alike).
    sub: DocAcc,
    /// Array element types.
    elements: BTreeMap<&'static str, TypeAcc>,
    lengths: Vec<usize>,
}

#[derive(Default)]
struct TypeAcc {
    count: usize,
    numbers: Vec<f64>,
    strings: HashMap<String, usize>,
    dates: Vec<i64>,
    bools: (usize, usize),
}

impl TypeAcc {
    fn add(&mut self, b: &Bson) {
        self.count += 1;
        match b {
            Bson::Int32(v) => self.push_number(*v as f64),
            Bson::Int64(v) => self.push_number(*v as f64),
            Bson::Double(v) => self.push_number(*v),
            Bson::Decimal128(v) => {
                if let Ok(f) = v.to_string().parse::<f64>() {
                    self.push_number(f);
                }
            }
            Bson::String(s) | Bson::Symbol(s) => {
                if let Some(n) = self.strings.get_mut(s) {
                    *n += 1;
                } else if self.strings.len() < MAX_DISTINCT_STRINGS {
                    self.strings.insert(s.clone(), 1);
                }
            }
            Bson::DateTime(d) => {
                if self.dates.len() < MAX_VALUES {
                    self.dates.push(d.timestamp_millis());
                }
            }
            Bson::Boolean(true) => self.bools.0 += 1,
            Bson::Boolean(false) => self.bools.1 += 1,
            _ => {}
        }
    }
    fn push_number(&mut self, v: f64) {
        if v.is_finite() && self.numbers.len() < MAX_VALUES {
            self.numbers.push(v);
        }
    }
    fn finish(self, name: &'static str) -> TypeStats {
        let values = match name {
            "Int32" | "Int64" | "Double" | "Decimal128" => Values::Numbers(self.numbers),
            "String" | "Symbol" => Values::Strings(self.strings),
            "Date" => Values::Dates(self.dates),
            "Boolean" => Values::Booleans {
                t: self.bools.0,
                f: self.bools.1,
            },
            _ => Values::None,
        };
        TypeStats {
            name,
            count: self.count,
            values,
            elements: Vec::new(),
            lengths: Vec::new(),
        }
    }
}

impl DocAcc {
    fn add_doc(&mut self, d: &Document) {
        self.parents += 1;
        for (k, v) in d {
            let f = self.fields.entry(k.clone()).or_default();
            f.count += 1;
            f.add_value(v);
        }
    }

    fn finish(self, prefix: &str, depth: usize) -> Vec<Field> {
        let parents = self.parents;
        let mut out: Vec<Field> = self
            .fields
            .into_iter()
            .map(|(name, acc)| {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                acc.finish(path, name, depth, parents)
            })
            .collect();
        // `_id` first, then by name (the map is already name-sorted).
        out.sort_by_key(|f| f.name != "_id");
        out
    }
}

impl FieldAcc {
    fn add_value(&mut self, v: &Bson) {
        let name = type_name(v);
        self.types.entry(name).or_default().add(v);
        match v {
            Bson::Document(d) => self.sub.add_doc(d),
            Bson::Array(items) => {
                self.lengths.push(items.len());
                for item in items {
                    self.elements.entry(type_name(item)).or_default().add(item);
                    if let Bson::Document(d) = item {
                        self.sub.add_doc(d);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(self, path: String, name: String, depth: usize, parent_count: usize) -> Field {
        let mut elements: Vec<TypeStats> = self
            .elements
            .into_iter()
            .map(|(n, acc)| acc.finish(n))
            .collect();
        elements.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(b.name)));
        let mut types: Vec<TypeStats> = self
            .types
            .into_iter()
            .map(|(n, acc)| {
                let mut t = acc.finish(n);
                if n == "Array" {
                    t.elements = elements.clone();
                    t.lengths = self.lengths.clone();
                }
                t
            })
            .collect();
        types.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(b.name)));
        let children = if self.sub.parents > 0 {
            self.sub.finish(&path, depth + 1)
        } else {
            Vec::new()
        };
        Field {
            path,
            name,
            depth,
            parent_count,
            count: self.count,
            types,
            children,
        }
    }
}

pub fn analyze(docs: &[Document]) -> Schema {
    let mut root = DocAcc::default();
    for d in docs {
        root.add_doc(d);
    }
    Schema {
        sampled: docs.len(),
        fields: root.finish("", 0),
    }
}

// ----- charts -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub label: String,
    pub count: usize,
    /// The exact value, for `{ field: value }`.
    pub value: Bson,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bin {
    pub lo: f64,
    pub hi: f64,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Number,
    Date,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Chart {
    /// Discrete values: strings, booleans, numbers with few distinct values.
    Bars(Vec<Bar>),
    /// Equal-width bins between the min and max; the last bin is inclusive.
    Histogram {
        axis: Axis,
        bins: Vec<Bin>,
    },
    None,
}

fn number_bson(type_name: &str, v: f64) -> Bson {
    match type_name {
        "Int32" => Bson::Int32(v as i32),
        "Int64" => Bson::Int64(v as i64),
        _ => Bson::Double(v),
    }
}

/// A short label for a number: integers without decimals.
pub fn number_label(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn date_label(ms: i64) -> String {
    bson::DateTime::from_millis(ms)
        .try_to_rfc3339_string()
        .map(|s| s[..s.len().min(10)].to_string())
        .unwrap_or_else(|_| ms.to_string())
}

fn histogram(values: &[f64], axis: Axis) -> Chart {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return Chart::None;
    }
    let n = if hi > lo { BINS } else { 1 };
    let width = if n == 1 { 1.0 } else { (hi - lo) / n as f64 };
    let mut bins: Vec<Bin> = (0..n)
        .map(|i| Bin {
            lo: lo + width * i as f64,
            hi: if i + 1 == n {
                hi
            } else {
                lo + width * (i + 1) as f64
            },
            count: 0,
        })
        .collect();
    for v in values {
        let mut i = ((v - lo) / width).floor() as usize;
        if i >= n {
            i = n - 1;
        }
        bins[i].count += 1;
    }
    Chart::Histogram { axis, bins }
}

/// The chart for one type of one field.
pub fn chart(t: &TypeStats) -> Chart {
    match &t.values {
        Values::Numbers(values) if values.is_empty() => Chart::None,
        Values::Numbers(values) => {
            let mut distinct: BTreeMap<u64, (f64, usize)> = BTreeMap::new();
            for v in values {
                let e = distinct.entry(v.to_bits()).or_insert((*v, 0));
                e.1 += 1;
                if distinct.len() > TOP_VALUES {
                    break;
                }
            }
            if distinct.len() <= TOP_VALUES {
                let mut bars: Vec<Bar> = distinct
                    .into_values()
                    .map(|(v, count)| Bar {
                        label: number_label(v),
                        count,
                        value: number_bson(t.name, v),
                    })
                    .collect();
                bars.sort_by(|a, b| {
                    a.value
                        .as_f64()
                        .or(a.value.as_i64().map(|i| i as f64))
                        .or(a.value.as_i32().map(|i| i as f64))
                        .partial_cmp(
                            &b.value
                                .as_f64()
                                .or(b.value.as_i64().map(|i| i as f64))
                                .or(b.value.as_i32().map(|i| i as f64)),
                        )
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                Chart::Bars(bars)
            } else {
                histogram(values, Axis::Number)
            }
        }
        Values::Strings(map) if map.is_empty() => Chart::None,
        Values::Strings(map) => {
            let mut items: Vec<(&String, &usize)> = map.iter().collect();
            items.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            Chart::Bars(
                items
                    .into_iter()
                    .take(TOP_VALUES)
                    .map(|(s, n)| Bar {
                        label: ejson::truncate(s, 40),
                        count: *n,
                        value: Bson::String(s.clone()),
                    })
                    .collect(),
            )
        }
        Values::Dates(values) if values.is_empty() => Chart::None,
        Values::Dates(values) => {
            let as_f: Vec<f64> = values.iter().map(|v| *v as f64).collect();
            histogram(&as_f, Axis::Date)
        }
        Values::Booleans { t: yes, f: no } => Chart::Bars(vec![
            Bar {
                label: "true".into(),
                count: *yes,
                value: Bson::Boolean(true),
            },
            Bar {
                label: "false".into(),
                count: *no,
                value: Bson::Boolean(false),
            },
        ]),
        Values::None => Chart::None,
    }
}

/// The filter for clicking a bar: `{ path: value }`.
pub fn value_filter(path: &str, value: &Bson) -> Document {
    doc! { path: value.clone() }
}

/// The filter for clicking a histogram bin: a half-open range, closed on the
/// last bin so the maximum is included.
pub fn range_filter(path: &str, axis: Axis, bin: &Bin, last: bool) -> Document {
    let (lo, hi) = match axis {
        Axis::Number => (Bson::Double(bin.lo), Bson::Double(bin.hi)),
        Axis::Date => (
            Bson::DateTime(bson::DateTime::from_millis(bin.lo as i64)),
            Bson::DateTime(bson::DateTime::from_millis(bin.hi as i64)),
        ),
    };
    let upper = if last { "$lte" } else { "$lt" };
    doc! { path: { "$gte": lo, upper: hi } }
}

/// Bin label: `18 – 24` or `2024-01-01 – 2024-02-01`.
pub fn bin_label(axis: Axis, bin: &Bin) -> String {
    match axis {
        Axis::Number => format!("{} – {}", number_label(bin.lo), number_label(bin.hi)),
        Axis::Date => format!(
            "{} – {}",
            date_label(bin.lo as i64),
            date_label(bin.hi as i64)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;

    fn sample() -> Vec<Document> {
        let mut docs = Vec::new();
        for i in 0..10i32 {
            let mut d = doc! {
                "_id": ObjectId::new(),
                "name": format!("n{}", i % 3),
                "age": i,
                "active": i % 2 == 0,
                "tags": ["a", "b"],
                "address": { "city": "Oslo", "zip": i.to_string() },
                "created": bson::DateTime::from_millis(1_700_000_000_000 + i as i64 * 86_400_000),
            };
            if i % 5 == 0 {
                d.insert("score", 1.5f64);
            }
            if i == 3 {
                d.insert("age", "three");
            }
            docs.push(d);
        }
        docs
    }

    #[test]
    fn presence_and_types() {
        let s = analyze(&sample());
        assert_eq!(s.sampled, 10);
        assert_eq!(s.fields[0].name, "_id");
        let age = s.fields.iter().find(|f| f.name == "age").unwrap();
        assert_eq!(age.count, 10);
        assert_eq!(age.types[0].name, "Int32");
        assert_eq!(age.types[0].count, 9);
        assert_eq!(age.types[1].name, "String");
        assert_eq!(age.types_text(), "Int32 (90%), String (10%)");
        let score = s.fields.iter().find(|f| f.name == "score").unwrap();
        assert_eq!(score.count, 2);
        assert_eq!(score.presence_pct(), 20.0);
        assert_eq!(score.types_text(), "Double (20%), undefined (80%)");
    }

    #[test]
    fn nested_and_arrays() {
        let s = analyze(&sample());
        let addr = s.fields.iter().find(|f| f.name == "address").unwrap();
        assert_eq!(addr.types[0].name, "Object");
        assert_eq!(addr.children.len(), 2);
        assert_eq!(addr.children[0].path, "address.city");
        assert_eq!(addr.children[0].depth, 1);
        assert_eq!(addr.children[0].parent_count, 10);
        let tags = s.fields.iter().find(|f| f.name == "tags").unwrap();
        let arr = &tags.types[0];
        assert_eq!(arr.name, "Array");
        assert_eq!(arr.elements[0].name, "String");
        assert_eq!(arr.elements[0].count, 20);
        assert_eq!(arr.lengths, vec![2; 10]);
        let flat = s.flatten();
        let paths: Vec<&str> = flat.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"address.zip"));
        assert_eq!(paths[0], "_id");
        let lines = s.summary_lines(3);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("_id: ObjectId (100%)"));
    }

    #[test]
    fn docs_in_arrays_contribute_children() {
        let docs = vec![
            doc! { "items": [ { "sku": "a", "qty": 1 }, { "sku": "b" } ] },
            doc! { "items": { "sku": "c" } },
        ];
        let s = analyze(&docs);
        let items = &s.fields[0];
        assert_eq!(items.children.len(), 2);
        let sku = items.children.iter().find(|f| f.name == "sku").unwrap();
        assert_eq!(sku.parent_count, 3);
        assert_eq!(sku.count, 3);
        let qty = items.children.iter().find(|f| f.name == "qty").unwrap();
        assert_eq!(qty.count, 1);
    }

    #[test]
    fn charts() {
        let s = analyze(&sample());
        let age = s.fields.iter().find(|f| f.name == "age").unwrap();
        match chart(&age.types[0]) {
            Chart::Bars(bars) => {
                assert_eq!(bars.len(), 9);
                assert_eq!(bars[0].label, "0");
                assert_eq!(bars[0].value, Bson::Int32(0));
            }
            other => panic!("{other:?}"),
        }
        let name = s.fields.iter().find(|f| f.name == "name").unwrap();
        match chart(&name.types[0]) {
            Chart::Bars(bars) => {
                assert_eq!(bars[0].label, "n0");
                assert_eq!(bars[0].count, 4);
                assert_eq!(bars.len(), 3);
            }
            other => panic!("{other:?}"),
        }
        let active = s.fields.iter().find(|f| f.name == "active").unwrap();
        match chart(&active.types[0]) {
            Chart::Bars(bars) => assert_eq!((bars[0].count, bars[1].count), (5, 5)),
            other => panic!("{other:?}"),
        }
        let created = s.fields.iter().find(|f| f.name == "created").unwrap();
        match chart(&created.types[0]) {
            Chart::Histogram { axis, bins } => {
                assert_eq!(axis, Axis::Date);
                assert_eq!(bins.len(), BINS);
                assert_eq!(bins.iter().map(|b| b.count).sum::<usize>(), 10);
                assert_eq!(bins.last().unwrap().count, 1);
                let f = range_filter("created", axis, &bins[0], false);
                assert!(f.get_document("created").unwrap().contains_key("$lt"));
            }
            other => panic!("{other:?}"),
        }
        // Many distinct numbers -> histogram.
        let many: Vec<Document> = (0..100).map(|i| doc! { "v": i as f64 * 1.5 }).collect();
        let s = analyze(&many);
        match chart(&s.fields[0].types[0]) {
            Chart::Histogram { axis, bins } => {
                assert_eq!(axis, Axis::Number);
                assert_eq!(bins.len(), BINS);
                assert_eq!(bins.iter().map(|b| b.count).sum::<usize>(), 100);
                assert_eq!(bin_label(axis, &bins[0]), "0 – 7.42");
                let last = range_filter("v", axis, bins.last().unwrap(), true);
                assert!(last.get_document("v").unwrap().contains_key("$lte"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            value_filter("name", &Bson::String("x".into())),
            doc! { "name": "x" }
        );
    }

    #[test]
    fn labels() {
        assert_eq!(number_label(3.0), "3");
        assert_eq!(number_label(1.23456), "1.23");
        assert_eq!(number_label(-2.5), "-2.5");
        assert_eq!(date_label(0), "1970-01-01");
    }
}
