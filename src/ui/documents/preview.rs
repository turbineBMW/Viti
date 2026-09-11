//! Display-only BSON snapshots. Bound traversal *before* cloning/serializing;
//! truncating JSON afterwards still walks and allocates the entire document.
//! Originals remain in DocumentsPane for opening, copying, editing and export.
use crate::mongo::ejson;
use bson::{Bson, Document};

const MAX_NODES: usize = 64;
const MAX_CHILDREN: usize = 20;
const MAX_DEPTH: usize = 3;
const MAX_STRING: usize = 256;

#[derive(Default)]
struct Budget {
    remaining: usize,
    truncated: bool,
}

impl Budget {
    fn text(&mut self, text: &str) -> String {
        let out = ejson::truncate(text, MAX_STRING);
        self.truncated |= out != text;
        out
    }

    fn document(&mut self, doc: &Document, depth: usize) -> Document {
        let mut out = Document::new();
        for (key, value) in doc.iter().take(MAX_CHILDREN) {
            if self.remaining == 0 {
                break;
            }
            let key = self.text(key);
            out.insert(key, self.value(value, depth));
        }
        self.truncated |= out.len() < doc.len();
        out
    }

    fn value(&mut self, value: &Bson, depth: usize) -> Bson {
        self.remaining = self.remaining.saturating_sub(1);
        match value {
            Bson::String(s) => Bson::String(self.text(s)),
            Bson::Document(_) | Bson::Array(_) if depth >= MAX_DEPTH => {
                self.truncated = true;
                Bson::String(ejson::summary(value, MAX_STRING))
            }
            Bson::Document(d) => Bson::Document(self.document(d, depth + 1)),
            Bson::Array(a) => {
                let mut out = Vec::new();
                for v in a.iter().take(MAX_CHILDREN) {
                    if self.remaining == 0 {
                        break;
                    }
                    out.push(self.value(v, depth + 1));
                }
                self.truncated |= out.len() < a.len();
                Bson::Array(out)
            }
            Bson::Binary(b) if b.bytes.len() > MAX_STRING => {
                self.truncated = true;
                Bson::String(ejson::summary(value, MAX_STRING))
            }
            Bson::JavaScriptCode(s) => Bson::JavaScriptCode(self.text(s)),
            Bson::Symbol(s) => Bson::Symbol(self.text(s)),
            Bson::RegularExpression(r) => Bson::RegularExpression(bson::Regex {
                pattern: self
                    .text(r.pattern.as_str())
                    .try_into()
                    .expect("regex contains no NUL"),
                options: self
                    .text(r.options.as_str())
                    .try_into()
                    .expect("regex contains no NUL"),
            }),
            Bson::JavaScriptCodeWithScope(_) | Bson::DbPointer(_) => {
                self.truncated = true;
                Bson::String(ejson::summary(value, MAX_STRING))
            }
            other => other.clone(),
        }
    }
}

pub fn document(doc: &Document) -> (Document, bool) {
    let mut budget = Budget {
        remaining: MAX_NODES,
        ..Default::default()
    };
    let out = budget.document(doc, 0);
    (out, budget.truncated)
}

pub fn value(value: &Bson) -> (Bson, bool) {
    let mut budget = Budget {
        remaining: MAX_CHILDREN,
        ..Default::default()
    };
    let out = budget.value(value, 0);
    (out, budget.truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn small_documents_keep_bson_types_and_content() {
        let doc = doc! { "_id": bson::oid::ObjectId::new(), "n": 42i64,
        "nested": { "array": [1, "two", null] }, "date": bson::DateTime::now() };
        let (copy, truncated) = document(&doc);
        assert!(!truncated);
        assert_eq!(copy, doc);
    }

    #[test]
    fn large_payloads_are_bounded_without_changing_originals() {
        let large = "🦀".repeat(1_000_000);
        let doc = doc! { "text": large.clone(), "array": vec![Bson::Int32(1); 100_000],
        "binary": Bson::Binary(bson::Binary { subtype: bson::spec::BinarySubtype::Generic,
            bytes: vec![0; 1_000_000] }) };
        let (copy, truncated) = document(&doc);
        assert!(truncated);
        assert!(ejson::pretty(&copy, ejson::Mode::Relaxed).len() < 5000);
        assert_eq!(doc.get_str("text").unwrap(), large);
        assert_eq!(doc.get_array("array").unwrap().len(), 100_000);
        let json = super::super::json::collapsed_text(&doc);
        assert!(json.len() < 5000);
        assert!(json.contains("Preview shortened"));
        assert!(copy.get_str("text").unwrap().ends_with('…'));
    }

    #[test]
    fn depth_and_width_share_a_fixed_budget() {
        let mut deep = doc! { "leaf": "hidden" };
        for _ in 0..50 {
            deep = doc! { "next": deep };
        }
        let wide: Document = (0..1000)
            .map(|i| (format!("k{i}"), Bson::Document(deep.clone())))
            .collect();
        let (copy, truncated) = document(&wide);
        assert!(truncated);
        assert!(copy.len() <= MAX_CHILDREN);
        assert!(ejson::pretty(&copy, ejson::Mode::Relaxed).len() < 10_000);
    }
}
