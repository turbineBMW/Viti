//! Explain output → a plan tree plus the headline numbers Compass shows.
//! Understands the classic find shape (`queryPlanner` / `executionStats`),
//! the SBE shape (`winningPlan.queryPlan` + `planNodeId`s), the aggregation
//! shape (`stages: [{ $cursor }, { $group }…]`) and both sharded forms.
//! Pure; tested.
use bson::{Bson, Document};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    pub stage: String,
    pub index_name: Option<String>,
    pub n_returned: Option<i64>,
    pub exec_ms: Option<i64>,
    pub docs_examined: Option<i64>,
    pub keys_examined: Option<i64>,
    /// Set on the per-shard roots of a sharded plan.
    pub shard: Option<String>,
    /// The node's own fields, children removed.
    pub details: Document,
    pub children: Vec<PlanNode>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub namespace: Option<String>,
    pub n_returned: Option<i64>,
    pub exec_ms: Option<i64>,
    pub docs_examined: Option<i64>,
    pub keys_examined: Option<i64>,
    pub indexes: Vec<String>,
    pub collscan: bool,
    pub sort_in_memory: bool,
    pub sort_spilled: bool,
    pub rejected_plans: usize,
    pub shards: usize,
    /// False for `queryPlanner` verbosity: no timings, no counts.
    pub has_execution_stats: bool,
    pub root: Option<PlanNode>,
}

fn num(b: Option<&Bson>) -> Option<i64> {
    match b? {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(f) => Some(*f as i64),
        _ => None,
    }
}

const CHILD_KEYS: &[&str] = &[
    "inputStage",
    "inputStages",
    "shards",
    "executionStages",
    "queryPlan",
    "thenStage",
    "elseStage",
    "innerStage",
    "outerStage",
    "child",
    "children",
];

/// SBE execution stats keyed by `planNodeId`, to decorate the query plan.
type Stats = HashMap<i64, (Option<i64>, Option<i64>, Option<i64>, Option<i64>)>;

fn collect_sbe_stats(d: &Document, into: &mut Stats) {
    if let Some(id) = num(d.get("planNodeId")) {
        let e = into.entry(id).or_default();
        // Several SBE stages share a plan node; keep the outermost numbers.
        if e.0.is_none() {
            e.0 = num(d.get("nReturned"));
        }
        if e.1.is_none() {
            e.1 = num(d.get("executionTimeMillisEstimate"));
        }
        if e.2.is_none() {
            e.2 = num(d.get("docsExamined")).or_else(|| num(d.get("numReads")));
        }
        if e.3.is_none() {
            e.3 = num(d.get("keysExamined")).or_else(|| num(d.get("keysExamined")));
        }
    }
    for k in CHILD_KEYS {
        match d.get(k) {
            Some(Bson::Document(c)) => collect_sbe_stats(c, into),
            Some(Bson::Array(a)) => {
                for c in a.iter().filter_map(|b| b.as_document()) {
                    collect_sbe_stats(c, into);
                }
            }
            _ => {}
        }
    }
}

fn plan_node(d: &Document, stats: Option<&Stats>) -> PlanNode {
    let mut details = d.clone();
    for k in CHILD_KEYS {
        details.remove(k);
    }
    let mut node = PlanNode {
        stage: d.get_str("stage").unwrap_or("?").to_string(),
        index_name: d.get_str("indexName").ok().map(str::to_string),
        n_returned: num(d.get("nReturned")),
        exec_ms: num(d.get("executionTimeMillisEstimate"))
            .or_else(|| num(d.get("executionTimeMillis"))),
        docs_examined: num(d.get("docsExamined")),
        keys_examined: num(d.get("keysExamined")),
        shard: d.get_str("shardName").ok().map(str::to_string),
        details,
        children: Vec::new(),
    };
    if let (Some(stats), Some(id)) = (stats, num(d.get("planNodeId")))
        && let Some((n, ms, docs, keys)) = stats.get(&id)
    {
        node.n_returned = node.n_returned.or(*n);
        node.exec_ms = node.exec_ms.or(*ms);
        node.docs_examined = node.docs_examined.or(*docs);
        node.keys_examined = node.keys_examined.or(*keys);
    }
    // A shard entry carries its plan under executionStages / winningPlan.
    if let Some(Bson::Document(p)) = d.get("queryPlan") {
        let mut inner = plan_node(p, stats);
        inner.shard = node.shard.take().or(inner.shard);
        if node.stage == "?" {
            return inner;
        }
        node.children.push(inner);
    }
    for k in [
        "inputStage",
        "thenStage",
        "elseStage",
        "innerStage",
        "outerStage",
        "child",
    ] {
        if let Some(Bson::Document(c)) = d.get(k) {
            node.children.push(plan_node(c, stats));
        }
    }
    for k in ["inputStages", "children"] {
        if let Some(Bson::Array(a)) = d.get(k) {
            node.children.extend(
                a.iter()
                    .filter_map(|b| b.as_document())
                    .map(|c| plan_node(c, stats)),
            );
        }
    }
    if let Some(Bson::Array(shards)) = d.get("shards") {
        for s in shards.iter().filter_map(|b| b.as_document()) {
            let name = s.get_str("shardName").ok().map(str::to_string);
            let mut child = match (s.get("executionStages"), s.get("winningPlan")) {
                (Some(Bson::Document(e)), _) => plan_node(e, stats),
                (_, Some(Bson::Document(w))) => plan_node(w, stats),
                _ => plan_node(s, stats),
            };
            child.shard = name;
            node.children.push(child);
        }
    }
    if node.stage == "?"
        && let Some(Bson::Document(e)) = d.get("executionStages")
    {
        return plan_node(e, stats);
    }
    node
}

/// The plan tree of one find-shaped explain: execution stages when present,
/// else the winning plan (SBE plans unwrap `queryPlan`).
fn find_root(d: &Document) -> Option<PlanNode> {
    let planner = d.get_document("queryPlanner").ok();
    let exec = d
        .get_document("executionStats")
        .ok()
        .and_then(|e| e.get_document("executionStages").ok());
    let winning = planner.and_then(|p| p.get_document("winningPlan").ok());
    let sbe_plan = winning.and_then(|w| w.get_document("queryPlan").ok());
    match (exec, sbe_plan, winning) {
        (Some(e), Some(plan), _) if !e.contains_key("queryPlan") => {
            // SBE: readable stage names live in the plan; numbers in the
            // execution tree, joined by planNodeId.
            let mut stats = Stats::new();
            collect_sbe_stats(e, &mut stats);
            let mut root = plan_node(plan, Some(&stats));
            if root.n_returned.is_none() {
                root.n_returned = num(e.get("nReturned"));
            }
            if root.exec_ms.is_none() {
                root.exec_ms = num(e.get("executionTimeMillisEstimate"));
            }
            Some(root)
        }
        (Some(e), _, _) => Some(plan_node(e, None)),
        (None, Some(plan), _) => Some(plan_node(plan, None)),
        (None, None, Some(w)) => Some(plan_node(w, None)),
        _ => None,
    }
}

fn pipeline_operator(stage: &Document) -> Option<&str> {
    stage
        .keys()
        .map(String::as_str)
        .find(|k| k.starts_with('$'))
}

/// One `stages` array as a chain: the last stage is the root, each stage
/// has the previous one as its only child.
fn stages_root(stages: &[Document]) -> Option<PlanNode> {
    let mut chain: Option<PlanNode> = None;
    for s in stages {
        let op = pipeline_operator(s).unwrap_or("?");
        let mut node = if op == "$cursor" {
            let inner = s.get_document("$cursor").ok();
            let mut n = inner.and_then(find_root).unwrap_or_default();
            if n.stage.is_empty() {
                n.stage = "$cursor".into();
            }
            if let Some(i) = inner {
                let exec = i.get_document("executionStats").ok();
                n.n_returned = n
                    .n_returned
                    .or_else(|| exec.and_then(|e| num(e.get("nReturned"))));
                n.exec_ms = n
                    .exec_ms
                    .or_else(|| exec.and_then(|e| num(e.get("executionTimeMillis"))));
                n.docs_examined = n
                    .docs_examined
                    .or_else(|| exec.and_then(|e| num(e.get("totalDocsExamined"))));
                n.keys_examined = n
                    .keys_examined
                    .or_else(|| exec.and_then(|e| num(e.get("totalKeysExamined"))));
            }
            n
        } else {
            let mut details = s.clone();
            for k in ["nReturned", "executionTimeMillisEstimate"] {
                details.remove(k);
            }
            PlanNode {
                stage: op.to_string(),
                n_returned: num(s.get("nReturned")),
                exec_ms: num(s.get("executionTimeMillisEstimate")),
                details,
                ..Default::default()
            }
        };
        if let Some(prev) = chain.take() {
            node.children.push(prev);
        }
        chain = Some(node);
    }
    chain
}

fn any_root(d: &Document) -> Option<PlanNode> {
    if let Ok(stages) = d.get_array("stages") {
        let docs: Vec<Document> = stages
            .iter()
            .filter_map(|b| b.as_document().cloned())
            .collect();
        return stages_root(&docs);
    }
    if d.contains_key("queryPlanner") {
        return find_root(d);
    }
    // Sharded aggregation: { shards: { name: { stages | queryPlanner } }, mergeType }
    if let Ok(shards) = d.get_document("shards") {
        let children: Vec<PlanNode> = shards
            .iter()
            .filter_map(|(name, v)| {
                let mut n = any_root(v.as_document()?)?;
                n.shard = Some(name.clone());
                Some(n)
            })
            .collect();
        let merge = d
            .get_str("mergeType")
            .map(|m| format!("SHARD_MERGE ({m})"))
            .unwrap_or_else(|_| "SHARD_MERGE".into());
        let mut details = d.clone();
        details.remove("shards");
        return Some(PlanNode {
            stage: merge,
            children,
            details,
            ..Default::default()
        });
    }
    None
}

fn walk(node: &PlanNode, s: &mut Summary) {
    if let Some(i) = &node.index_name
        && !s.indexes.contains(i)
    {
        s.indexes.push(i.clone());
    }
    if node.stage == "COLLSCAN" {
        s.collscan = true;
    }
    if node.stage == "SORT" || node.stage == "$sort" {
        s.sort_in_memory = true;
        if matches!(node.details.get("usedDisk"), Some(Bson::Boolean(true))) {
            s.sort_spilled = true;
        }
    }
    if node.shard.is_some() {
        s.shards += 1;
    }
    for c in &node.children {
        walk(c, s);
    }
}

pub fn parse(explain: &Document) -> Summary {
    let mut s = Summary::default();
    // The command echo names the namespace for every shape.
    if let Ok(cmd) = explain.get_document("command") {
        let coll = cmd
            .get_str("find")
            .or_else(|_| cmd.get_str("aggregate"))
            .ok();
        if let (Some(c), Ok(db)) = (coll, cmd.get_str("$db")) {
            s.namespace = Some(format!("{db}.{c}"));
        }
    }
    if let Ok(p) = explain.get_document("queryPlanner") {
        if s.namespace.is_none() {
            s.namespace = p.get_str("namespace").ok().map(str::to_string);
        }
        s.rejected_plans = p.get_array("rejectedPlans").map(|a| a.len()).unwrap_or(0);
    }
    if let Ok(e) = explain.get_document("executionStats") {
        s.has_execution_stats = true;
        s.n_returned = num(e.get("nReturned"));
        s.exec_ms = num(e.get("executionTimeMillis"));
        s.docs_examined = num(e.get("totalDocsExamined"));
        s.keys_examined = num(e.get("totalKeysExamined"));
    }
    s.root = any_root(explain);
    if let Some(root) = s.root.clone() {
        walk(&root, &mut s);
        // Pipeline shape: totals come from the chain.
        if s.n_returned.is_none() {
            s.n_returned = root.n_returned;
        }
        if s.exec_ms.is_none() {
            s.exec_ms = root.exec_ms;
        }
        if s.docs_examined.is_none() || s.keys_examined.is_none() {
            let (mut docs, mut keys, mut any) = (0, 0, false);
            fn sum(n: &PlanNode, docs: &mut i64, keys: &mut i64, any: &mut bool) {
                if let Some(d) = n.docs_examined {
                    *docs += d;
                    *any = true;
                }
                if let Some(k) = n.keys_examined {
                    *keys += k;
                    *any = true;
                }
                for c in &n.children {
                    sum(c, docs, keys, any);
                }
            }
            sum(&root, &mut docs, &mut keys, &mut any);
            if any {
                s.docs_examined.get_or_insert(docs);
                s.keys_examined.get_or_insert(keys);
            }
        }
        if !s.has_execution_stats {
            s.has_execution_stats = root.n_returned.is_some() || root.exec_ms.is_some();
        }
    }
    s
}

impl PlanNode {
    /// "IXSCAN email_1", "$group", "COLLSCAN"…
    pub fn label(&self) -> String {
        match &self.index_name {
            Some(i) => format!("{} {i}", self.stage),
            None => self.stage.clone(),
        }
    }

    pub fn count(&self) -> usize {
        1 + self.children.iter().map(PlanNode::count).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn classic_find_with_execution_stats() {
        let e = doc! {
            "queryPlanner": {
                "namespace": "viti_test.people",
                "winningPlan": { "stage": "FETCH", "inputStage": { "stage": "IXSCAN", "indexName": "age_1_name_1" } },
                "rejectedPlans": [ { "stage": "COLLSCAN" } ]
            },
            "executionStats": {
                "nReturned": 25, "executionTimeMillis": 3, "totalKeysExamined": 25, "totalDocsExamined": 25,
                "executionStages": {
                    "stage": "FETCH", "nReturned": 25, "executionTimeMillisEstimate": 2, "docsExamined": 25,
                    "inputStage": { "stage": "IXSCAN", "indexName": "age_1_name_1", "nReturned": 25, "keysExamined": 25 }
                }
            }
        };
        let s = parse(&e);
        assert_eq!(s.namespace.as_deref(), Some("viti_test.people"));
        assert_eq!(s.n_returned, Some(25));
        assert_eq!(s.exec_ms, Some(3));
        assert_eq!(s.indexes, vec!["age_1_name_1"]);
        assert!(!s.collscan);
        assert_eq!(s.rejected_plans, 1);
        let root = s.root.unwrap();
        assert_eq!(root.stage, "FETCH");
        assert_eq!(root.children[0].label(), "IXSCAN age_1_name_1");
        assert_eq!(root.children[0].keys_examined, Some(25));
        assert!(!root.details.contains_key("inputStage"));
    }

    #[test]
    fn planner_only_and_collscan_sort() {
        let e = doc! {
            "queryPlanner": {
                "winningPlan": { "stage": "SORT", "sortPattern": { "age": 1 }, "inputStage": { "stage": "COLLSCAN" } },
                "rejectedPlans": []
            }
        };
        let s = parse(&e);
        assert!(!s.has_execution_stats);
        assert!(s.collscan);
        assert!(s.sort_in_memory);
        assert_eq!(s.root.unwrap().count(), 2);
    }

    #[test]
    fn sbe_plan_gets_numbers_from_execution_tree() {
        let e = doc! {
            "queryPlanner": {
                "winningPlan": {
                    "queryPlan": { "stage": "GROUP", "planNodeId": 2, "inputStage": { "stage": "COLLSCAN", "planNodeId": 1 } },
                    "slotBasedPlan": { "stages": "..." }
                }
            },
            "executionStats": {
                "nReturned": 5, "executionTimeMillis": 9, "totalDocsExamined": 100, "totalKeysExamined": 0,
                "executionStages": {
                    "stage": "group", "planNodeId": 2, "nReturned": 5, "executionTimeMillisEstimate": 8,
                    "inputStage": { "stage": "scan", "planNodeId": 1, "nReturned": 100, "numReads": 100 }
                }
            }
        };
        let s = parse(&e);
        let root = s.root.unwrap();
        assert_eq!(root.stage, "GROUP");
        assert_eq!(root.n_returned, Some(5));
        assert_eq!(root.children[0].stage, "COLLSCAN");
        assert_eq!(root.children[0].n_returned, Some(100));
        assert_eq!(root.children[0].docs_examined, Some(100));
        assert!(s.collscan);
    }

    #[test]
    fn aggregation_stages_chain() {
        let e = doc! {
            "stages": [
                { "$cursor": {
                    "queryPlanner": { "namespace": "db.c", "winningPlan": { "stage": "COLLSCAN" } },
                    "executionStats": { "nReturned": 1000, "executionTimeMillis": 4, "totalDocsExamined": 1000, "totalKeysExamined": 0,
                        "executionStages": { "stage": "COLLSCAN", "nReturned": 1000, "docsExamined": 1000 } }
                } },
                { "$group": { "_id": "$city" }, "nReturned": 5, "executionTimeMillisEstimate": 6 },
                { "$sort": { "sortKey": { "n": -1 } }, "nReturned": 5, "executionTimeMillisEstimate": 7, "usedDisk": true }
            ],
            "command": { "aggregate": "c", "$db": "db" }
        };
        let s = parse(&e);
        assert_eq!(s.namespace.as_deref(), Some("db.c"));
        assert_eq!(s.n_returned, Some(5));
        assert_eq!(s.exec_ms, Some(7));
        assert_eq!(s.docs_examined, Some(1000));
        assert!(s.has_execution_stats);
        assert!(s.sort_spilled);
        let root = s.root.unwrap();
        assert_eq!(root.stage, "$sort");
        assert_eq!(root.children[0].stage, "$group");
        assert_eq!(root.children[0].children[0].stage, "COLLSCAN");
        assert_eq!(root.count(), 3);
    }

    #[test]
    fn sharded_find_and_aggregate() {
        let e = doc! {
            "queryPlanner": { "winningPlan": { "stage": "SHARD_MERGE", "shards": [
                { "shardName": "s1", "winningPlan": { "stage": "COLLSCAN" } },
                { "shardName": "s2", "winningPlan": { "stage": "IXSCAN", "indexName": "x_1" } }
            ] } }
        };
        let s = parse(&e);
        assert_eq!(s.shards, 2);
        assert_eq!(s.indexes, vec!["x_1"]);
        assert!(s.collscan);
        let root = s.root.unwrap();
        assert_eq!(root.children[1].shard.as_deref(), Some("s2"));

        let agg = doc! {
            "mergeType": "mongos",
            "shards": { "s1": { "stages": [ { "$cursor": { "queryPlanner": { "winningPlan": { "stage": "COLLSCAN" } } } } ] } }
        };
        let s = parse(&agg);
        assert_eq!(s.shards, 1);
        assert_eq!(s.root.unwrap().stage, "SHARD_MERGE (mongos)");
    }
}
