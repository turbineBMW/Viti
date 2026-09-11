//! The aggregation pipeline as the UI edits it: a list of stages, each an
//! operator plus the *text* of its value, so the user's formatting survives
//! round-trips through saved pipelines and the external editor. Parsed to BSON
//! only when run. Pure; tested.
use super::ejson::{self, ParseError};
use bson::{Bson, Document};
use serde::{Deserialize, Serialize};

/// One stage card. `body` is the operator's value as loose EJSON text.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Stage {
    pub operator: String,
    pub body: String,
    pub enabled: bool,
}

impl Default for Stage {
    fn default() -> Self {
        Stage::new("$match")
    }
}

/// A known stage operator with a one-line description and a starting body.
pub struct StageInfo {
    pub name: &'static str,
    pub help: &'static str,
    pub template: &'static str,
}

const DOC: &str = "{\n  \n}";

/// Every stage the dropdown offers, alphabetical like Compass.
pub const STAGES: &[StageInfo] = &[
    StageInfo {
        name: "$addFields",
        help: "Adds new fields to documents",
        template: DOC,
    },
    StageInfo {
        name: "$bucket",
        help: "Groups documents into buckets by boundaries",
        template: "{\n  groupBy: \"$field\",\n  boundaries: [0, 10, 20],\n  default: \"other\",\n  output: {\n    count: { $sum: 1 }\n  }\n}",
    },
    StageInfo {
        name: "$bucketAuto",
        help: "Groups documents into a number of buckets",
        template: "{\n  groupBy: \"$field\",\n  buckets: 5\n}",
    },
    StageInfo {
        name: "$collStats",
        help: "Collection statistics",
        template: "{\n  latencyStats: { histograms: false },\n  storageStats: {},\n  count: {}\n}",
    },
    StageInfo {
        name: "$count",
        help: "Counts documents into one field",
        template: "\"count\"",
    },
    StageInfo {
        name: "$densify",
        help: "Fills gaps in a sequence",
        template: "{\n  field: \"field\",\n  range: { step: 1, bounds: \"full\" }\n}",
    },
    StageInfo {
        name: "$documents",
        help: "Literal documents as input",
        template: "[\n  {  }\n]",
    },
    StageInfo {
        name: "$facet",
        help: "Several sub-pipelines in one stage",
        template: "{\n  facet1: [\n    { $match: {} }\n  ]\n}",
    },
    StageInfo {
        name: "$fill",
        help: "Fills null or missing values",
        template: "{\n  output: {\n    field: { value: 0 }\n  }\n}",
    },
    StageInfo {
        name: "$geoNear",
        help: "Sorts by distance from a point",
        template: "{\n  near: { type: \"Point\", coordinates: [0, 0] },\n  distanceField: \"distance\",\n  spherical: true\n}",
    },
    StageInfo {
        name: "$graphLookup",
        help: "Recursive search on a collection",
        template: "{\n  from: \"collection\",\n  startWith: \"$field\",\n  connectFromField: \"field\",\n  connectToField: \"field\",\n  as: \"result\"\n}",
    },
    StageInfo {
        name: "$group",
        help: "Groups by an expression",
        template: "{\n  _id: \"$field\",\n  count: { $sum: 1 }\n}",
    },
    StageInfo {
        name: "$indexStats",
        help: "Index usage statistics",
        template: "{}",
    },
    StageInfo {
        name: "$limit",
        help: "Passes the first n documents",
        template: "10",
    },
    StageInfo {
        name: "$lookup",
        help: "Left outer join with another collection",
        template: "{\n  from: \"collection\",\n  localField: \"field\",\n  foreignField: \"field\",\n  as: \"result\"\n}",
    },
    StageInfo {
        name: "$match",
        help: "Filters documents",
        template: DOC,
    },
    StageInfo {
        name: "$merge",
        help: "Writes the results into a collection (merge)",
        template: "{\n  into: \"collection\",\n  on: \"_id\",\n  whenMatched: \"merge\",\n  whenNotMatched: \"insert\"\n}",
    },
    StageInfo {
        name: "$out",
        help: "Writes the results into a collection (replace)",
        template: "\"collection\"",
    },
    StageInfo {
        name: "$project",
        help: "Includes, excludes or computes fields",
        template: "{\n  _id: 0,\n  field: 1\n}",
    },
    StageInfo {
        name: "$redact",
        help: "Restricts document contents by expression",
        template: "{\n  $cond: {\n    if: { $eq: [\"$level\", 5] },\n    then: \"$$PRUNE\",\n    else: \"$$DESCEND\"\n  }\n}",
    },
    StageInfo {
        name: "$replaceRoot",
        help: "Promotes an embedded document",
        template: "{\n  newRoot: \"$field\"\n}",
    },
    StageInfo {
        name: "$replaceWith",
        help: "Replaces each document with an expression",
        template: "\"$field\"",
    },
    StageInfo {
        name: "$sample",
        help: "Random documents",
        template: "{\n  size: 10\n}",
    },
    StageInfo {
        name: "$search",
        help: "Atlas Search",
        template: "{\n  index: \"default\",\n  text: {\n    query: \"\",\n    path: { wildcard: \"*\" }\n  }\n}",
    },
    StageInfo {
        name: "$searchMeta",
        help: "Atlas Search metadata",
        template: "{\n  index: \"default\",\n  count: { type: \"total\" }\n}",
    },
    StageInfo {
        name: "$set",
        help: "Adds or overwrites fields",
        template: DOC,
    },
    StageInfo {
        name: "$setWindowFields",
        help: "Window functions over partitions",
        template: "{\n  partitionBy: \"$field\",\n  sortBy: { field: 1 },\n  output: {\n    total: { $sum: \"$value\", window: { documents: [\"unbounded\", \"current\"] } }\n  }\n}",
    },
    StageInfo {
        name: "$skip",
        help: "Skips the first n documents",
        template: "10",
    },
    StageInfo {
        name: "$sort",
        help: "Sorts documents",
        template: "{\n  field: -1\n}",
    },
    StageInfo {
        name: "$sortByCount",
        help: "Groups and counts, sorted by count",
        template: "\"$field\"",
    },
    StageInfo {
        name: "$unionWith",
        help: "Appends another collection's documents",
        template: "{\n  coll: \"collection\",\n  pipeline: []\n}",
    },
    StageInfo {
        name: "$unset",
        help: "Removes fields",
        template: "[\n  \"field\"\n]",
    },
    StageInfo {
        name: "$unwind",
        help: "One document per array element",
        template: "{\n  path: \"$array\",\n  preserveNullAndEmptyArrays: false\n}",
    },
    StageInfo {
        name: "$vectorSearch",
        help: "Atlas Vector Search",
        template: "{\n  index: \"vector_index\",\n  path: \"embedding\",\n  queryVector: [],\n  numCandidates: 100,\n  limit: 10\n}",
    },
];

pub fn stage_info(name: &str) -> Option<&'static StageInfo> {
    STAGES.iter().find(|s| s.name == name)
}

pub fn stage_index(name: &str) -> Option<usize> {
    STAGES.iter().position(|s| s.name == name)
}

/// The stages that write to a collection; never previewed, confirmed on run.
pub fn is_write_operator(op: &str) -> bool {
    matches!(op, "$out" | "$merge")
}

/// The operator of a stage document (its single top-level key).
pub fn operator_of(doc: &Document) -> Option<&str> {
    let mut keys = doc.keys();
    let first = keys.next()?;
    if keys.next().is_some() {
        return None;
    }
    Some(first.as_str())
}

/// Loose pretty text for any value, indented by two spaces per level.
pub fn value_text(value: &Bson) -> String {
    serde_json::to_string_pretty(&value.clone().into_relaxed_extjson()).unwrap_or_default()
}

impl Stage {
    pub fn new(operator: &str) -> Self {
        Stage {
            operator: operator.to_string(),
            body: stage_info(operator)
                .map(|s| s.template.to_string())
                .unwrap_or_else(|| DOC.to_string()),
            enabled: true,
        }
    }

    /// From a parsed stage document; `None` if it is not `{ $op: value }`.
    pub fn from_document(doc: &Document) -> Option<Self> {
        let op = operator_of(doc)?;
        if !op.starts_with('$') {
            return None;
        }
        Some(Stage {
            operator: op.to_string(),
            body: value_text(doc.get(op)?),
            enabled: true,
        })
    }

    pub fn is_write(&self) -> bool {
        is_write_operator(&self.operator)
    }

    /// The `{ $op: value }` document; parse errors carry the body's position.
    pub fn parse(&self) -> Result<Document, ParseError> {
        let value = ejson::parse_value(if self.body.trim().is_empty() {
            "{}"
        } else {
            &self.body
        })?;
        let mut doc = Document::new();
        doc.insert(self.operator.clone(), value);
        Ok(doc)
    }

    /// The stage as text for the pipeline array, indented to nest inside it.
    fn text(&self, indent: &str) -> String {
        let body = self.body.trim();
        let inner = format!("{indent}  ");
        let mut out = format!("{indent}{{\n{inner}{}: ", self.operator);
        for (i, line) in body.lines().enumerate() {
            if i > 0 {
                out.push('\n');
                out.push_str(&inner);
            }
            out.push_str(line);
        }
        out.push('\n');
        out.push_str(indent);
        out.push('}');
        out
    }
}

/// The whole pipeline: the card list plus text-mode conversions.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Pipeline {
    pub stages: Vec<Stage>,
}

impl Pipeline {
    pub fn from_documents(docs: &[Document]) -> Result<Self, String> {
        let stages = docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                Stage::from_document(d)
                    .ok_or_else(|| format!("stage {} is not a {{ $operator: … }} document", i + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Pipeline { stages })
    }

    /// Parse text-mode input: an array of stage documents (or a lone stage).
    pub fn from_text(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Pipeline::default());
        }
        let docs = ejson::parse_documents(text).map_err(|e| e.to_string())?;
        Self::from_documents(&docs)
    }

    /// Text mode / external editor: the enabled stages as a mongosh-style array.
    pub fn to_text(&self) -> String {
        let enabled: Vec<&Stage> = self.stages.iter().filter(|s| s.enabled).collect();
        if enabled.is_empty() {
            return "[\n  \n]".into();
        }
        let mut out = String::from("[\n");
        for (i, s) in enabled.iter().enumerate() {
            out.push_str(&s.text("  "));
            if i + 1 < enabled.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push(']');
        out
    }

    /// The enabled stages as BSON; the error names the failing card (0-based).
    pub fn documents(&self) -> Result<Vec<Document>, (usize, ParseError)> {
        self.stages
            .iter()
            .enumerate()
            .filter(|(_, s)| s.enabled)
            .map(|(i, s)| s.parse().map_err(|e| (i, e)))
            .collect()
    }

    /// The enabled stages up to and including card `upto`, for previews.
    pub fn documents_upto(&self, upto: usize) -> Result<Vec<Document>, (usize, ParseError)> {
        self.stages
            .iter()
            .enumerate()
            .take(upto + 1)
            .filter(|(_, s)| s.enabled)
            .map(|(i, s)| s.parse().map_err(|e| (i, e)))
            .collect()
    }

    pub fn has_write_stage(&self) -> bool {
        self.stages.iter().any(|s| s.enabled && s.is_write())
    }

    /// Where the enabled write stage writes: "db.coll" or the `$out` target.
    pub fn write_target(&self) -> Option<String> {
        let s = self.stages.iter().find(|s| s.enabled && s.is_write())?;
        let target = match s.parse().ok()?.get(&s.operator)? {
            Bson::String(c) => c.clone(),
            Bson::Document(d) => match d.get("into").or_else(|| d.get("coll")) {
                Some(Bson::String(c)) => c.clone(),
                Some(Bson::Document(t)) => format!(
                    "{}.{}",
                    t.get_str("db").unwrap_or("?"),
                    t.get_str("coll").unwrap_or("?")
                ),
                _ => "?".into(),
            },
            _ => "?".into(),
        };
        Some(format!("{} {target}", s.operator))
    }

    pub fn disabled_count(&self) -> usize {
        self.stages.iter().filter(|s| !s.enabled).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn stage_round_trips_through_text() {
        let p = Pipeline::from_text(
            "[ { $match: { age: { $gte: 18 } } }, { $group: { _id: '$city', n: { $sum: 1 } } }, { $limit: 5 } ]",
        )
        .unwrap();
        assert_eq!(p.stages.len(), 3);
        assert_eq!(p.stages[0].operator, "$match");
        assert_eq!(p.stages[2].body, "5");
        let text = p.to_text();
        assert!(text.starts_with("[\n  {\n    $match: {\n"));
        let back = Pipeline::from_text(&text).unwrap();
        assert_eq!(back, p);
        let docs = p.documents().unwrap();
        assert_eq!(docs[2], doc! { "$limit": 5 });
    }

    #[test]
    fn disabled_stages_are_skipped() {
        let mut p = Pipeline::from_text("[{ $match: {} }, { $skip: 1 }, { $limit: 2 }]").unwrap();
        p.stages[1].enabled = false;
        assert_eq!(p.documents().unwrap().len(), 2);
        assert_eq!(p.documents_upto(1).unwrap().len(), 1);
        assert!(!p.to_text().contains("$skip"));
        assert_eq!(p.disabled_count(), 1);
    }

    #[test]
    fn errors_name_the_stage() {
        let mut p = Pipeline::from_text("[{ $match: {} }, { $limit: 2 }]").unwrap();
        p.stages[1].body = "{ oops".into();
        let (idx, _) = p.documents().unwrap_err();
        assert_eq!(idx, 1);
        assert!(Pipeline::from_text("[{ a: 1, b: 2 }]").is_err());
        assert!(Pipeline::from_text("[{ notop: 1 }]").is_err());
        assert!(Pipeline::from_text("   ").unwrap().stages.is_empty());
    }

    #[test]
    fn write_stages() {
        let p = Pipeline::from_text("[{ $match: {} }, { $out: 'copy' }]").unwrap();
        assert!(p.has_write_stage());
        assert_eq!(p.write_target().as_deref(), Some("$out copy"));
        let p = Pipeline::from_text("[{ $merge: { into: { db: 'a', coll: 'b' } } }]").unwrap();
        assert_eq!(p.write_target().as_deref(), Some("$merge a.b"));
        assert!(
            !Pipeline::from_text("[{ $match: {} }]")
                .unwrap()
                .has_write_stage()
        );
        assert!(Stage::new("$limit").body == "10");
        assert!(Stage::new("$bogus").body == DOC);
    }
}
