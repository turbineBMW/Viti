//! Every server operation the UI performs, as plain async fns over a cloned
//! `Client`. They run on tokio; nothing here touches widgets. Each read takes an
//! `OpCtx` so it carries a `maxTimeMS` and a comment the UI can `killOp` by.
use super::ejson;
use crate::config::Query;
use anyhow::{Context, Result, anyhow};
use bson::{Bson, Document, doc};
use futures_util::TryStreamExt;
use mongodb::Client;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct OpCtx {
    pub max_time_ms: u64,
    /// `viti:<uuid>`; matched against `command.comment` in `$currentOp` to kill.
    pub comment: String,
}

impl OpCtx {
    pub fn new(max_time_ms: u64) -> Self {
        Self {
            max_time_ms,
            comment: format!("viti:{}", uuid::Uuid::new_v4()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Namespace {
    pub db: String,
    pub coll: String,
}

impl Namespace {
    pub fn new(db: &str, coll: &str) -> Self {
        Self {
            db: db.into(),
            coll: coll.into(),
        }
    }
    fn collection(&self, client: &Client) -> mongodb::Collection<Document> {
        client.database(&self.db).collection(&self.coll)
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.db, self.coll)
    }
}

#[derive(Clone, Debug)]
pub struct DbInfo {
    pub name: String,
    pub size_on_disk: u64,
    pub empty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CollKind {
    Collection,
    View,
    TimeSeries,
    Other(String),
}

#[derive(Clone, Debug)]
pub struct CollInfo {
    pub name: String,
    pub kind: CollKind,
}

pub async fn list_databases(client: &Client) -> Result<Vec<DbInfo>> {
    let mut dbs: Vec<DbInfo> = client
        .list_databases()
        .await
        .context("could not list databases")?
        .into_iter()
        .map(|d| DbInfo {
            name: d.name,
            size_on_disk: d.size_on_disk,
            empty: d.empty,
        })
        .collect();
    dbs.sort_by_key(|a| a.name.to_lowercase());
    Ok(dbs)
}

pub async fn list_collections(client: &Client, db: &str) -> Result<Vec<CollInfo>> {
    let specs = client
        .database(db)
        .list_collections()
        .await
        .with_context(|| format!("could not list collections of {db}"))?
        .try_collect::<Vec<_>>()
        .await
        .with_context(|| format!("could not list collections of {db}"))?;
    let mut out: Vec<CollInfo> = specs
        .into_iter()
        .map(|s| CollInfo {
            kind: match s.collection_type {
                mongodb::results::CollectionType::Collection => CollKind::Collection,
                mongodb::results::CollectionType::View => CollKind::View,
                mongodb::results::CollectionType::Timeseries => CollKind::TimeSeries,
                other => CollKind::Other(format!("{other:?}")),
            },
            name: s.name,
        })
        .filter(|c| !c.name.starts_with("system."))
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

/// The query bar, parsed. Built on the GTK side so errors show before any I/O.
#[derive(Clone, Debug, Default)]
pub struct FindSpec {
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub collation: Option<Document>,
    pub skip: u64,
    pub limit: u64,
    pub hint: Option<Bson>,
    pub max_time_ms: Option<u64>,
}

impl FindSpec {
    /// Parse the query bar; the error names the field that failed.
    pub fn from_query(q: &Query, default_sort: &str) -> std::result::Result<Self, String> {
        let field = |name: &str, text: &str| -> std::result::Result<Option<Document>, String> {
            if text.trim().is_empty() {
                return Ok(None);
            }
            ejson::parse_document(text)
                .map(Some)
                .map_err(|e| format!("{name}: {e}"))
        };
        let sort_text = if q.sort.trim().is_empty() {
            default_sort
        } else {
            &q.sort
        };
        let hint = if q.hint.trim().is_empty() {
            None
        } else {
            Some(match ejson::parse_value(&q.hint) {
                Ok(b @ Bson::Document(_)) => b,
                _ => Bson::String(q.hint.trim().trim_matches('"').to_string()),
            })
        };
        Ok(Self {
            filter: ejson::parse_document_or_empty(&q.filter)
                .map_err(|e| format!("filter: {e}"))?,
            projection: field("project", &q.project)?,
            sort: field("sort", sort_text)?,
            collation: field("collation", &q.collation)?,
            skip: q.skip,
            limit: q.limit,
            hint,
            max_time_ms: q.max_time_ms,
        })
    }
}

fn collation_of(d: &Document) -> Result<mongodb::options::Collation> {
    bson::deserialize_from_document(d.clone()).context("collation")
}

fn hint_of(b: &Bson) -> mongodb::options::Hint {
    match b {
        Bson::Document(d) => mongodb::options::Hint::Keys(d.clone()),
        other => mongodb::options::Hint::Name(other.as_str().unwrap_or_default().to_string()),
    }
}

/// One page of a find. `skip`/`limit` are the page window *after* the query
/// bar's own skip; the bar's limit caps how far pages may go.
pub async fn find_page(
    client: &Client,
    ns: &Namespace,
    spec: &FindSpec,
    page_skip: u64,
    page_size: u64,
    ctx: &OpCtx,
) -> Result<Vec<Document>> {
    let coll = ns.collection(client);
    if page_size == 0 {
        return Ok(Vec::new());
    }
    let skip = spec
        .skip
        .checked_add(page_skip)
        .context("page offset is too large")?;
    let mut limit = page_size.min(i64::MAX as u64);
    if spec.limit > 0 {
        if page_skip >= spec.limit {
            return Ok(Vec::new());
        }
        limit = limit.min(spec.limit - page_skip);
    }
    let mut find = coll
        .find(spec.filter.clone())
        .skip(skip)
        .limit(limit as i64)
        .batch_size(limit.min(u32::MAX as u64) as u32)
        .max_time(Duration::from_millis(
            spec.max_time_ms
                .unwrap_or(ctx.max_time_ms)
                .min(ctx.max_time_ms)
                .max(1),
        ))
        .comment(Bson::String(ctx.comment.clone()));
    if let Some(p) = &spec.projection {
        find = find.projection(p.clone());
    }
    if let Some(s) = &spec.sort {
        find = find.sort(s.clone());
    }
    if let Some(c) = &spec.collation {
        find = find.collation(collation_of(c)?);
    }
    if let Some(h) = &spec.hint {
        find = find.hint(hint_of(h));
    }
    let cursor = find
        .await
        .with_context(|| format!("find on {} failed", ns))?;
    cursor
        .try_collect()
        .await
        .with_context(|| format!("reading {} failed", ns))
}

/// Matching-document count for the pager: exact when there is a filter,
/// estimated (collection stats) when there isn't. Its own short timeout, so a
/// slow count never holds the page back; `None` means "unknown".
pub async fn count(client: &Client, ns: &Namespace, spec: &FindSpec, ctx: &OpCtx) -> Option<u64> {
    let coll = ns.collection(client);
    let max = Duration::from_millis(ctx.max_time_ms.clamp(1, 5000));
    let r = if spec.filter.is_empty() && spec.collation.is_none() {
        coll.estimated_document_count().max_time(max).await
    } else {
        let mut c = coll
            .count_documents(spec.filter.clone())
            .max_time(max)
            .comment(Bson::String(ctx.comment.clone()));
        if let Some(col) = spec.collation.as_ref().and_then(|d| collation_of(d).ok()) {
            c = c.collation(col);
        }
        if let Some(h) = &spec.hint {
            c = c.hint(hint_of(h));
        }
        c.await
    };
    match r {
        Ok(n) => {
            let n = n.saturating_sub(spec.skip);
            Some(if spec.limit > 0 { n.min(spec.limit) } else { n })
        }
        Err(e) => {
            tracing::debug!("count on {}: {e}", ns.to_string());
            None
        }
    }
}

pub async fn insert_one(client: &Client, ns: &Namespace, doc: Document) -> Result<Bson> {
    let r = ns
        .collection(client)
        .insert_one(doc)
        .await
        .with_context(|| format!("insert into {} failed", ns))?;
    Ok(r.inserted_id)
}

pub async fn insert_many(client: &Client, ns: &Namespace, docs: Vec<Document>) -> Result<usize> {
    let r = ns
        .collection(client)
        .insert_many(docs)
        .await
        .with_context(|| format!("insert into {} failed", ns))?;
    Ok(r.inserted_ids.len())
}

/// Replace the document with `_id == id`; errors if it no longer exists.
pub async fn replace_by_id(
    client: &Client,
    ns: &Namespace,
    id: &Bson,
    doc: Document,
) -> Result<()> {
    let r = ns
        .collection(client)
        .replace_one(doc! { "_id": id.clone() }, doc)
        .await
        .with_context(|| format!("update in {} failed", ns))?;
    if r.matched_count == 0 {
        return Err(anyhow!(
            "document {} no longer exists in {}",
            ejson::id_display(id),
            ns
        ));
    }
    Ok(())
}

pub async fn delete_by_ids(client: &Client, ns: &Namespace, ids: Vec<Bson>) -> Result<u64> {
    let r = ns
        .collection(client)
        .delete_many(doc! { "_id": { "$in": ids } })
        .await
        .with_context(|| format!("delete from {} failed", ns))?;
    Ok(r.deleted_count)
}

#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    pub version: String,
    pub host: String,
    pub is_writable_primary: bool,
    pub topology: String,
}

pub async fn server_info(client: &Client) -> Result<ServerInfo> {
    let admin = client.database("admin");
    let hello = admin
        .run_command(doc! { "hello": 1 })
        .await
        .context("hello failed")?;
    let build = admin
        .run_command(doc! { "buildInfo": 1 })
        .await
        .unwrap_or_default();
    let topology = if hello.contains_key("setName") {
        format!("replica set {}", hello.get_str("setName").unwrap_or(""))
    } else if hello.get_str("msg").ok() == Some("isdbgrid") {
        "sharded".into()
    } else {
        "standalone".into()
    };
    Ok(ServerInfo {
        version: build.get_str("version").unwrap_or("?").to_string(),
        host: hello.get_str("me").unwrap_or("").to_string(),
        is_writable_primary: hello.get_bool("isWritablePrimary").unwrap_or(true),
        topology,
    })
}

/// Kill every operation tagged with `comment` (see `OpCtx`). Best effort: needs
/// the `killop`/`inprog` privileges, and the op may already have finished.
pub async fn kill_by_comment(client: &Client, comment: &str) -> Result<u32> {
    let admin = client.database("admin");
    let ops: Vec<Document> = admin
        .aggregate(vec![
            doc! { "$currentOp": { "allUsers": true } },
            doc! { "$match": { "command.comment": comment } },
            doc! { "$project": { "opid": 1 } },
        ])
        .await
        .context("$currentOp failed")?
        .try_collect()
        .await?;
    let mut killed = 0;
    for op in ops {
        if let Some(opid) = op.get("opid")
            && admin
                .run_command(doc! { "killOp": 1, "op": opid.clone() })
                .await
                .is_ok()
        {
            killed += 1;
        }
    }
    Ok(killed)
}

// ----- databases & collections ----------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeSeriesSpec {
    pub time_field: String,
    pub meta_field: Option<String>,
    /// "seconds" | "minutes" | "hours"
    pub granularity: Option<String>,
    pub expire_after_seconds: Option<u64>,
}

/// What the create-collection dialog collects. Only one of `capped`,
/// `time_series`, `clustered`, `view` is expected to be set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreateCollSpec {
    /// (size in bytes, max documents)
    pub capped: Option<(u64, Option<u64>)>,
    pub time_series: Option<TimeSeriesSpec>,
    pub clustered: bool,
    pub collation: Option<Document>,
    /// (source collection, pipeline)
    pub view: Option<(String, Vec<Document>)>,
    /// `expireAfterSeconds` for a clustered collection's TTL on `_id`.
    pub expire_after_seconds: Option<u64>,
}

/// The `create` command document (pure, tested).
pub fn create_collection_command(name: &str, spec: &CreateCollSpec) -> Document {
    let mut cmd = doc! { "create": name };
    if let Some((size, max)) = spec.capped {
        cmd.insert("capped", true);
        cmd.insert("size", size as i64);
        if let Some(m) = max {
            cmd.insert("max", m as i64);
        }
    }
    if let Some(ts) = &spec.time_series {
        let mut t = doc! { "timeField": ts.time_field.clone() };
        if let Some(m) = &ts.meta_field
            && !m.is_empty()
        {
            t.insert("metaField", m.clone());
        }
        if let Some(g) = &ts.granularity
            && !g.is_empty()
        {
            t.insert("granularity", g.clone());
        }
        cmd.insert("timeseries", t);
        if let Some(e) = ts.expire_after_seconds {
            cmd.insert("expireAfterSeconds", e as i64);
        }
    }
    if spec.clustered {
        cmd.insert(
            "clusteredIndex",
            doc! { "key": { "_id": 1 }, "unique": true, "name": format!("{name} clustered key") },
        );
        if let Some(e) = spec.expire_after_seconds {
            cmd.insert("expireAfterSeconds", e as i64);
        }
    }
    if let Some(c) = &spec.collation {
        cmd.insert("collation", c.clone());
    }
    if let Some((on, pipeline)) = &spec.view {
        cmd.insert("viewOn", on.clone());
        cmd.insert(
            "pipeline",
            Bson::Array(pipeline.iter().cloned().map(Bson::Document).collect()),
        );
    }
    cmd
}

pub async fn create_collection(
    client: &Client,
    db: &str,
    name: &str,
    spec: &CreateCollSpec,
) -> Result<()> {
    client
        .database(db)
        .run_command(create_collection_command(name, spec))
        .await
        .with_context(|| format!("create {db}.{name} failed"))?;
    Ok(())
}

pub async fn drop_collection(client: &Client, ns: &Namespace) -> Result<()> {
    ns.collection(client)
        .drop()
        .await
        .with_context(|| format!("drop {} failed", ns))
}

pub async fn drop_database(client: &Client, db: &str) -> Result<()> {
    client
        .database(db)
        .drop()
        .await
        .with_context(|| format!("drop database {db} failed"))
}

pub async fn rename_collection(
    client: &Client,
    ns: &Namespace,
    new_name: &str,
    drop_target: bool,
) -> Result<()> {
    client
        .database("admin")
        .run_command(doc! {
            "renameCollection": ns.to_string(),
            "to": format!("{}.{new_name}", ns.db),
            "dropTarget": drop_target,
        })
        .await
        .with_context(|| format!("rename {} failed", ns))?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct CollStats {
    pub count: Option<u64>,
    pub size: Option<u64>,
    pub storage_size: Option<u64>,
    pub avg_obj_size: Option<u64>,
    pub index_sizes: Vec<(String, u64)>,
    pub total_index_size: Option<u64>,
}

fn as_u64(b: Option<&Bson>) -> Option<u64> {
    match b? {
        Bson::Int32(i) => Some((*i).max(0) as u64),
        Bson::Int64(i) => Some((*i).max(0) as u64),
        Bson::Double(d) => Some(d.max(0.0) as u64),
        _ => None,
    }
}

/// `$collStats` storage numbers; views and unsupported servers give an error,
/// callers treat it as "unknown".
pub async fn coll_stats(client: &Client, ns: &Namespace) -> Result<CollStats> {
    let docs: Vec<Document> = ns
        .collection(client)
        .aggregate(vec![doc! { "$collStats": { "storageStats": {} } }])
        .await
        .with_context(|| format!("$collStats on {} failed", ns))?
        .try_collect()
        .await?;
    let Some(first) = docs.first() else {
        return Ok(CollStats::default());
    };
    let ss = first
        .get_document("storageStats")
        .cloned()
        .unwrap_or_default();
    let mut index_sizes: Vec<(String, u64)> = ss
        .get_document("indexSizes")
        .map(|d| {
            d.iter()
                .filter_map(|(k, v)| as_u64(Some(v)).map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();
    index_sizes.sort();
    Ok(CollStats {
        count: as_u64(ss.get("count")),
        size: as_u64(ss.get("size")),
        storage_size: as_u64(ss.get("storageSize")),
        avg_obj_size: as_u64(ss.get("avgObjSize")),
        total_index_size: as_u64(ss.get("totalIndexSize")),
        index_sizes,
    })
}

// ----- indexes --------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    pub keys: Document,
    /// Everything except `key`, `name`, `v`, `ns`: unique, sparse,
    /// expireAfterSeconds, partialFilterExpression, collation, hidden…
    pub options: Document,
    pub usage_ops: Option<i64>,
    pub usage_since: Option<bson::DateTime>,
    pub size: Option<u64>,
}

impl IndexInfo {
    /// Compass-style type label from the key spec.
    pub fn kind(&self) -> &'static str {
        let mut kind = "regular";
        for (k, v) in &self.keys {
            match v {
                Bson::String(s) => {
                    return match s.as_str() {
                        "text" => "text",
                        "2dsphere" => "geospatial",
                        "2d" => "geospatial",
                        "hashed" => "hashed",
                        "columnstore" => "columnstore",
                        _ => "regular",
                    };
                }
                _ if k.ends_with("$**") => kind = "wildcard",
                _ => {}
            }
        }
        if self.options.contains_key("expireAfterSeconds") {
            return "ttl";
        }
        kind
    }

    /// Property badges: unique, sparse, partial, ttl, hidden, collation, compound.
    pub fn properties(&self) -> Vec<&'static str> {
        let mut p = Vec::new();
        let is_text = self.keys.get_str("_fts").ok() == Some("text");
        let n_fields = if is_text {
            self.options
                .get_document("weights")
                .map(|w| w.len())
                .unwrap_or(1)
                + self
                    .keys
                    .iter()
                    .filter(|(k, _)| !k.starts_with("_fts"))
                    .count()
        } else {
            self.keys.len()
        };
        if n_fields > 1 {
            p.push("compound");
        }
        if self.options.get_bool("unique").unwrap_or(false) {
            p.push("unique");
        }
        if self.options.get_bool("sparse").unwrap_or(false) {
            p.push("sparse");
        }
        if self.options.contains_key("partialFilterExpression") {
            p.push("partial");
        }
        if self.options.contains_key("expireAfterSeconds") {
            p.push("ttl");
        }
        if self.options.get_bool("hidden").unwrap_or(false) {
            p.push("hidden");
        }
        if self.options.contains_key("collation") {
            p.push("collation");
        }
        p
    }

    pub fn is_hidden(&self) -> bool {
        self.options.get_bool("hidden").unwrap_or(false)
    }
}

/// `listIndexes` plus best-effort `$indexStats` (usage) and `$collStats` (sizes).
pub async fn list_indexes(client: &Client, ns: &Namespace) -> Result<Vec<IndexInfo>> {
    let raw: Vec<Document> = client
        .database(&ns.db)
        .run_cursor_command(doc! { "listIndexes": ns.coll.clone() })
        .await
        .with_context(|| format!("listIndexes on {} failed", ns))?
        .try_collect()
        .await
        .with_context(|| format!("listIndexes on {} failed", ns))?;
    let mut out: Vec<IndexInfo> = raw
        .into_iter()
        .map(|mut d| {
            let name = d.get_str("name").unwrap_or("").to_string();
            let keys = d.get_document("key").cloned().unwrap_or_default();
            for k in ["key", "name", "v", "ns"] {
                d.remove(k);
            }
            IndexInfo {
                name,
                keys,
                options: d,
                ..Default::default()
            }
        })
        .collect();
    let (stats, sizes) = tokio::join!(
        async {
            let r: Result<Vec<Document>> = async {
                Ok(ns
                    .collection(client)
                    .aggregate(vec![doc! { "$indexStats": {} }])
                    .await?
                    .try_collect()
                    .await?)
            }
            .await;
            r
        },
        coll_stats(client, ns)
    );
    match stats {
        Ok(stats) => {
            for s in stats {
                let Ok(name) = s.get_str("name") else {
                    continue;
                };
                if let Some(ix) = out.iter_mut().find(|i| i.name == name) {
                    let acc = s.get_document("accesses").cloned().unwrap_or_default();
                    ix.usage_ops = acc
                        .get_i64("ops")
                        .ok()
                        .or_else(|| acc.get_i32("ops").ok().map(i64::from));
                    ix.usage_since = acc.get_datetime("since").ok().copied();
                }
            }
        }
        Err(e) => tracing::debug!("$indexStats on {}: {e}", ns.to_string()),
    }
    match sizes {
        Ok(st) => {
            for (name, size) in st.index_sizes {
                if let Some(ix) = out.iter_mut().find(|i| i.name == name) {
                    ix.size = Some(size);
                }
            }
        }
        Err(e) => tracing::debug!("$collStats on {}: {e}", ns.to_string()),
    }
    out.sort_by(|a, b| {
        (a.name != "_id_")
            .cmp(&(b.name != "_id_"))
            .then(a.name.cmp(&b.name))
    });
    Ok(out)
}

/// The `createIndexes` command document (pure, tested). `options` is passed
/// through verbatim so every server option works; a missing `name` is derived
/// the way the server does (`field_1_other_-1`).
pub fn create_index_command(coll: &str, keys: &Document, options: &Document) -> Document {
    let mut ix = doc! { "key": keys.clone() };
    for (k, v) in options {
        ix.insert(k, v.clone());
    }
    if !ix.contains_key("name") {
        ix.insert("name", default_index_name(keys));
    }
    doc! { "createIndexes": coll, "indexes": [ix] }
}

pub fn default_index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(k, v)| {
            let v = match v {
                Bson::String(s) => s.clone(),
                Bson::Int32(i) => i.to_string(),
                Bson::Int64(i) => i.to_string(),
                Bson::Double(d) => format!("{}", *d as i64),
                other => ejson::summary(other, 16),
            };
            format!("{k}_{v}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

pub async fn create_index(
    client: &Client,
    ns: &Namespace,
    keys: &Document,
    options: &Document,
) -> Result<String> {
    let cmd = create_index_command(&ns.coll, keys, options);
    let name = cmd
        .get_array("indexes")
        .ok()
        .and_then(|a| a.first())
        .and_then(|b| b.as_document())
        .and_then(|d| d.get_str("name").ok())
        .unwrap_or("")
        .to_string();
    client
        .database(&ns.db)
        .run_command(cmd)
        .await
        .with_context(|| format!("createIndexes on {} failed", ns))?;
    Ok(name)
}

pub async fn drop_index(client: &Client, ns: &Namespace, name: &str) -> Result<()> {
    client
        .database(&ns.db)
        .run_command(doc! { "dropIndexes": ns.coll.clone(), "index": name })
        .await
        .with_context(|| format!("drop index {name} on {} failed", ns))?;
    Ok(())
}

pub async fn set_index_hidden(
    client: &Client,
    ns: &Namespace,
    name: &str,
    hidden: bool,
) -> Result<()> {
    client
        .database(&ns.db)
        .run_command(
            doc! { "collMod": ns.coll.clone(), "index": { "name": name, "hidden": hidden } },
        )
        .await
        .with_context(|| format!("collMod (hidden) on {} failed", ns))?;
    Ok(())
}

// ----- bulk ------------------------------------------------------------------

/// An update: operator document or aggregation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateSpec {
    Operators(Document),
    Pipeline(Vec<Document>),
}

impl UpdateSpec {
    /// `{ $set: {...} }` or `[ { $set: {...} }, ... ]`; operators are required
    /// in the document form (a replacement document is not a bulk update).
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        match ejson::parse_value(text).map_err(|e| e.to_string())? {
            Bson::Document(d) => {
                if d.is_empty() {
                    return Err("the update is empty".into());
                }
                if let Some(k) = d.keys().find(|k| !k.starts_with('$')) {
                    return Err(format!(
                        "`{k}` is not an update operator (use $set, $unset, $inc… or a pipeline)"
                    ));
                }
                Ok(UpdateSpec::Operators(d))
            }
            Bson::Array(a) => {
                let stages: Option<Vec<Document>> =
                    a.into_iter().map(|b| b.as_document().cloned()).collect();
                match stages {
                    Some(s) if !s.is_empty() => Ok(UpdateSpec::Pipeline(s)),
                    _ => Err("a pipeline update is a non-empty array of stage documents".into()),
                }
            }
            _ => Err("expected an update document or a pipeline array".into()),
        }
    }

    fn modifications(&self) -> mongodb::options::UpdateModifications {
        match self {
            UpdateSpec::Operators(d) => mongodb::options::UpdateModifications::Document(d.clone()),
            UpdateSpec::Pipeline(p) => mongodb::options::UpdateModifications::Pipeline(p.clone()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UpdateOutcome {
    pub matched: u64,
    pub modified: u64,
    pub upserted: bool,
}

pub async fn update_many(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    update: &UpdateSpec,
    collation: Option<&Document>,
    upsert: bool,
    ctx: &OpCtx,
) -> Result<UpdateOutcome> {
    let coll = ns.collection(client);
    let mut op = coll
        .update_many(filter, update.modifications())
        .upsert(upsert)
        .comment(Bson::String(ctx.comment.clone()));
    if let Some(c) = collation {
        op = op.collation(collation_of(c)?);
    }
    let r = op
        .await
        .with_context(|| format!("updateMany on {} failed", ns))?;
    Ok(UpdateOutcome {
        matched: r.matched_count,
        modified: r.modified_count,
        upserted: r.upserted_id.is_some(),
    })
}

pub async fn delete_many(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    collation: Option<&Document>,
    ctx: &OpCtx,
) -> Result<u64> {
    let coll = ns.collection(client);
    let mut op = coll
        .delete_many(filter)
        .comment(Bson::String(ctx.comment.clone()));
    if let Some(c) = collation {
        op = op.collation(collation_of(c)?);
    }
    let r = op
        .await
        .with_context(|| format!("deleteMany on {} failed", ns))?;
    Ok(r.deleted_count)
}

/// Exact count of a filter, under the op's time cap.
pub async fn count_filter(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    collation: Option<&Document>,
    ctx: &OpCtx,
) -> Result<u64> {
    let coll = ns.collection(client);
    let mut op = coll
        .count_documents(filter)
        .max_time(Duration::from_millis(ctx.max_time_ms.max(1)))
        .comment(Bson::String(ctx.comment.clone()));
    if let Some(c) = collation {
        op = op.collation(collation_of(c)?);
    }
    op.await.with_context(|| format!("count on {} failed", ns))
}

/// The first `n` documents matching `filter` (natural order).
pub async fn sample(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    n: u64,
    ctx: &OpCtx,
) -> Result<Vec<Document>> {
    ns.collection(client)
        .find(filter)
        .limit(n as i64)
        .max_time(Duration::from_millis(ctx.max_time_ms.max(1)))
        .comment(Bson::String(ctx.comment.clone()))
        .await
        .with_context(|| format!("find on {} failed", ns))?
        .try_collect()
        .await
        .with_context(|| format!("reading {} failed", ns))
}

/// Up to `n` random documents matching `filter` (`$sample`), for schema
/// analysis. Fails where `$sample` is unsupported; callers fall back to `sample`.
pub async fn sample_random(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    n: u64,
    ctx: &OpCtx,
) -> Result<Vec<Document>> {
    let mut pipeline = Vec::new();
    if !filter.is_empty() {
        pipeline.push(doc! { "$match": filter });
    }
    pipeline.push(doc! { "$sample": { "size": n as i64 } });
    aggregate(client, ns, pipeline, &AggOpts::default(), ctx).await
}

/// How a preview was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Applied by the server inside an aborted transaction.
    Transaction,
    /// Pipeline update run as an aggregation over the sample: exact.
    Aggregation,
    /// Emulated client-side: close, not authoritative.
    Approximate,
}

/// Before/after pairs for up to `n` matching documents. Tries a transaction
/// first (exact, needs a replica set), then an aggregation for pipeline
/// updates, then the client-side emulation.
pub async fn preview_update(
    client: &Client,
    ns: &Namespace,
    filter: Document,
    update: &UpdateSpec,
    n: u64,
    ctx: &OpCtx,
) -> Result<(PreviewKind, Vec<(Document, Document)>)> {
    let before = sample(client, ns, filter.clone(), n, ctx).await?;
    if before.is_empty() {
        return Ok((PreviewKind::Transaction, Vec::new()));
    }
    let ids: Vec<Bson> = before
        .iter()
        .filter_map(|d| d.get("_id").cloned())
        .collect();
    match preview_in_transaction(client, ns, &before, update).await {
        Ok(after) => {
            return Ok((
                PreviewKind::Transaction,
                before.into_iter().zip(after).collect(),
            ));
        }
        Err(e) => tracing::debug!("transactional preview unavailable: {e:#}"),
    }
    if let UpdateSpec::Pipeline(stages) = update {
        let mut pipeline = vec![doc! { "$match": { "_id": { "$in": ids.clone() } } }];
        pipeline.extend(stages.iter().cloned());
        let after: Vec<Document> = ns
            .collection(client)
            .aggregate(pipeline)
            .await
            .with_context(|| format!("preview aggregation on {} failed", ns))?
            .try_collect()
            .await?;
        let pairs = before
            .into_iter()
            .map(|b| {
                let id = b.get("_id");
                let a = after
                    .iter()
                    .find(|a| a.get("_id") == id)
                    .cloned()
                    .unwrap_or_default();
                (b, a)
            })
            .collect();
        return Ok((PreviewKind::Aggregation, pairs));
    }
    let UpdateSpec::Operators(ops) = update else {
        unreachable!()
    };
    let mut pairs = Vec::with_capacity(before.len());
    for b in before {
        let a = super::update_preview::apply(&b, ops)
            .map_err(|op| anyhow!("no preview: {op} is not emulated on a standalone server"))?;
        pairs.push((b, a));
    }
    Ok((PreviewKind::Approximate, pairs))
}

async fn preview_in_transaction(
    client: &Client,
    ns: &Namespace,
    before: &[Document],
    update: &UpdateSpec,
) -> Result<Vec<Document>> {
    let mut session = client.start_session().await?;
    session.start_transaction().await?;
    let coll = ns.collection(client);
    let mut after = Vec::with_capacity(before.len());
    let result: Result<()> = async {
        for b in before {
            let id = b.get("_id").cloned().unwrap_or(Bson::Null);
            let r = coll
                .find_one_and_update(doc! { "_id": id }, update.modifications())
                .return_document(mongodb::options::ReturnDocument::After)
                .session(&mut session)
                .await?;
            after.push(r.unwrap_or_default());
        }
        Ok(())
    }
    .await;
    let _ = session.abort_transaction().await;
    result?;
    Ok(after)
}

// ----- aggregation & explain --------------------------------------------------

/// Options shared by the aggregation page and its explain.
#[derive(Clone, Debug, Default)]
pub struct AggOpts {
    pub collation: Option<Document>,
    pub allow_disk_use: bool,
    pub max_time_ms: Option<u64>,
}

/// Run a pipeline and collect it. Callers append their own `$limit` for
/// previews and result pages; write stages (`$out`/`$merge`) run as given.
pub async fn aggregate(
    client: &Client,
    ns: &Namespace,
    pipeline: Vec<Document>,
    opts: &AggOpts,
    ctx: &OpCtx,
) -> Result<Vec<Document>> {
    let coll = ns.collection(client);
    let mut agg = coll
        .aggregate(pipeline)
        .max_time(Duration::from_millis(
            opts.max_time_ms
                .unwrap_or(ctx.max_time_ms)
                .min(ctx.max_time_ms)
                .max(1),
        ))
        .comment(Bson::String(ctx.comment.clone()));
    if opts.allow_disk_use {
        agg = agg.allow_disk_use(true);
    }
    if let Some(c) = &opts.collation {
        agg = agg.collation(collation_of(c)?);
    }
    agg.await
        .with_context(|| format!("aggregate on {} failed", ns))?
        .try_collect()
        .await
        .with_context(|| format!("reading aggregate on {} failed", ns))
}

/// A streaming find for exports: the whole query bar spec, no page window.
pub async fn find_cursor(
    client: &Client,
    ns: &Namespace,
    spec: &FindSpec,
    ctx: &OpCtx,
) -> Result<mongodb::Cursor<Document>> {
    let coll = ns.collection(client);
    let mut find = coll
        .find(spec.filter.clone())
        .skip(spec.skip)
        .max_time(Duration::from_millis(
            spec.max_time_ms
                .unwrap_or(ctx.max_time_ms)
                .min(ctx.max_time_ms)
                .max(1),
        ))
        .comment(Bson::String(ctx.comment.clone()));
    if spec.limit > 0 {
        find = find.limit(spec.limit as i64);
    }
    if let Some(p) = &spec.projection {
        find = find.projection(p.clone());
    }
    if let Some(s) = &spec.sort {
        find = find.sort(s.clone());
    }
    if let Some(c) = &spec.collation {
        find = find.collation(collation_of(c)?);
    }
    if let Some(h) = &spec.hint {
        find = find.hint(hint_of(h));
    }
    find.await.with_context(|| format!("find on {} failed", ns))
}

/// A streaming aggregation for exports.
pub async fn aggregate_cursor(
    client: &Client,
    ns: &Namespace,
    pipeline: Vec<Document>,
    opts: &AggOpts,
    ctx: &OpCtx,
) -> Result<mongodb::Cursor<Document>> {
    let coll = ns.collection(client);
    let mut agg = coll
        .aggregate(pipeline)
        .max_time(Duration::from_millis(
            opts.max_time_ms
                .unwrap_or(ctx.max_time_ms)
                .min(ctx.max_time_ms)
                .max(1),
        ))
        .comment(Bson::String(ctx.comment.clone()));
    if opts.allow_disk_use {
        agg = agg.allow_disk_use(true);
    }
    if let Some(c) = &opts.collation {
        agg = agg.collation(collation_of(c)?);
    }
    agg.await
        .with_context(|| format!("aggregate on {} failed", ns))
}

pub const VERBOSITIES: [&str; 3] = ["queryPlanner", "executionStats", "allPlansExecution"];

async fn run_explain(
    client: &Client,
    ns: &Namespace,
    inner: Document,
    verbosity: &str,
    ctx: &OpCtx,
) -> Result<Document> {
    let cmd = doc! {
        "explain": inner,
        "verbosity": verbosity,
        "comment": ctx.comment.clone(),
    };
    client
        .database(&ns.db)
        .run_command(cmd)
        .await
        .with_context(|| format!("explain on {} failed", ns))
}

/// Explain the query bar's find. The page window is not applied: the plan
/// is for the query as written.
pub async fn explain_find(
    client: &Client,
    ns: &Namespace,
    spec: &FindSpec,
    verbosity: &str,
    ctx: &OpCtx,
) -> Result<Document> {
    let mut find = doc! {
        "find": ns.coll.clone(),
        "filter": spec.filter.clone(),
        "maxTimeMS": spec.max_time_ms.unwrap_or(ctx.max_time_ms).min(ctx.max_time_ms).max(1) as i64,
    };
    if let Some(p) = &spec.projection {
        find.insert("projection", p.clone());
    }
    if let Some(s) = &spec.sort {
        find.insert("sort", s.clone());
    }
    if let Some(c) = &spec.collation {
        find.insert("collation", c.clone());
    }
    if let Some(h) = &spec.hint {
        find.insert("hint", h.clone());
    }
    if spec.skip > 0 {
        find.insert("skip", spec.skip as i64);
    }
    if spec.limit > 0 {
        find.insert("limit", spec.limit as i64);
    }
    run_explain(client, ns, find, verbosity, ctx).await
}

pub async fn explain_aggregate(
    client: &Client,
    ns: &Namespace,
    pipeline: Vec<Document>,
    opts: &AggOpts,
    verbosity: &str,
    ctx: &OpCtx,
) -> Result<Document> {
    let mut agg = doc! {
        "aggregate": ns.coll.clone(),
        "pipeline": pipeline,
        "cursor": {},
        "maxTimeMS": opts.max_time_ms.unwrap_or(ctx.max_time_ms).min(ctx.max_time_ms).max(1) as i64,
    };
    if opts.allow_disk_use {
        agg.insert("allowDiskUse", true);
    }
    if let Some(c) = &opts.collation {
        agg.insert("collation", c.clone());
    }
    run_explain(client, ns, agg, verbosity, ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_commands() {
        let cmd = create_collection_command(
            "c",
            &CreateCollSpec {
                capped: Some((1024, Some(10))),
                collation: Some(doc! { "locale": "en", "strength": 2 }),
                ..Default::default()
            },
        );
        assert_eq!(
            cmd,
            doc! { "create": "c", "capped": true, "size": 1024i64, "max": 10i64, "collation": { "locale": "en", "strength": 2 } }
        );
        let ts = create_collection_command(
            "t",
            &CreateCollSpec {
                time_series: Some(TimeSeriesSpec {
                    time_field: "ts".into(),
                    meta_field: Some("meta".into()),
                    granularity: Some("hours".into()),
                    expire_after_seconds: Some(3600),
                }),
                ..Default::default()
            },
        );
        assert_eq!(
            ts.get_document("timeseries")
                .unwrap()
                .get_str("metaField")
                .unwrap(),
            "meta"
        );
        assert_eq!(ts.get_i64("expireAfterSeconds").unwrap(), 3600);
        let v = create_collection_command(
            "v",
            &CreateCollSpec {
                view: Some(("src".into(), vec![doc! { "$match": { "a": 1 } }])),
                ..Default::default()
            },
        );
        assert_eq!(v.get_str("viewOn").unwrap(), "src");
        assert_eq!(v.get_array("pipeline").unwrap().len(), 1);
        let cl = create_collection_command(
            "k",
            &CreateCollSpec {
                clustered: true,
                ..Default::default()
            },
        );
        assert!(
            cl.get_document("clusteredIndex")
                .unwrap()
                .get_bool("unique")
                .unwrap()
        );
    }

    #[test]
    fn index_commands_and_kinds() {
        let keys = doc! { "a": 1, "b": -1 };
        let cmd = create_index_command("c", &keys, &doc! { "unique": true });
        let ix = cmd.get_array("indexes").unwrap()[0].as_document().unwrap();
        assert_eq!(ix.get_str("name").unwrap(), "a_1_b_-1");
        assert!(ix.get_bool("unique").unwrap());
        let named = create_index_command("c", &doc! { "t": "text" }, &doc! { "name": "txt" });
        assert_eq!(
            named.get_array("indexes").unwrap()[0]
                .as_document()
                .unwrap()
                .get_str("name")
                .unwrap(),
            "txt"
        );
        let info = IndexInfo {
            name: "x".into(),
            keys: doc! { "loc": "2dsphere" },
            ..Default::default()
        };
        assert_eq!(info.kind(), "geospatial");
        let ttl = IndexInfo {
            name: "t".into(),
            keys: doc! { "created": 1 },
            options: doc! { "expireAfterSeconds": 60, "hidden": true },
            ..Default::default()
        };
        assert_eq!(ttl.kind(), "ttl");
        assert_eq!(ttl.properties(), vec!["ttl", "hidden"]);
        let wild = IndexInfo {
            name: "w".into(),
            keys: doc! { "$**": 1 },
            ..Default::default()
        };
        assert_eq!(wild.kind(), "wildcard");
    }

    #[test]
    fn update_spec_parses() {
        assert!(matches!(
            UpdateSpec::parse("{ $set: { a: 1 } }"),
            Ok(UpdateSpec::Operators(_))
        ));
        assert!(matches!(
            UpdateSpec::parse("[{ $set: { a: 1 } }]"),
            Ok(UpdateSpec::Pipeline(_))
        ));
        assert!(UpdateSpec::parse("{ a: 1 }").is_err());
        assert!(UpdateSpec::parse("{}").is_err());
        assert!(UpdateSpec::parse("[]").is_err());
        assert!(UpdateSpec::parse("nope{").is_err());
    }
}

/// Integration round-trip against a live server; skipped unless
/// `VITI_TEST_URI` is set (e.g. `mongodb://localhost:27017`).
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    async fn find_pages_respect_query_window() {
        let Ok(uri) = std::env::var("VITI_TEST_URI") else {
            return;
        };
        let client = Client::with_uri_str(uri).await.unwrap();
        let ns = Namespace::new(
            &format!("viti_it_{}", uuid::Uuid::new_v4().simple()),
            "pages",
        );
        let ctx = OpCtx::new(10_000);
        insert_many(
            &client,
            &ns,
            (0..55)
                .map(|i| doc! { "_id": i, "n": i, "hidden": true })
                .collect(),
        )
        .await
        .unwrap();
        let spec = FindSpec {
            sort: Some(doc! { "_id": 1 }),
            ..Default::default()
        };
        assert_eq!(
            find_page(&client, &ns, &spec, 0, 26, &ctx)
                .await
                .unwrap()
                .len(),
            26
        );
        let last = find_page(&client, &ns, &spec, 50, 26, &ctx).await.unwrap();
        assert_eq!(last.len(), 5);
        assert_eq!(last[0].get_i32("_id").unwrap(), 50);
        let spec = FindSpec {
            filter: doc! { "n": { "$gte": 10 } },
            skip: 3,
            limit: 7,
            sort: Some(doc! { "n": -1 }),
            projection: Some(doc! { "hidden": 0 }),
            ..Default::default()
        };
        let first = find_page(&client, &ns, &spec, 0, 6, &ctx).await.unwrap();
        assert_eq!(first.len(), 6);
        assert_eq!(first[0].get_i32("n").unwrap(), 51);
        assert!(!first[0].contains_key("hidden"));
        let last = find_page(&client, &ns, &spec, 5, 6, &ctx).await.unwrap();
        assert_eq!(last.len(), 2);
        assert_eq!(last[0].get_i32("n").unwrap(), 46);
        assert!(
            find_page(&client, &ns, &spec, 7, 6, &ctx)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            find_page(&client, &ns, &spec, 0, 0, &ctx)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            find_page(&client, &ns, &spec, u64::MAX, 6, &ctx)
                .await
                .is_err()
        );
        client.database(&ns.db).drop().await.unwrap();
    }

    #[tokio::test]
    async fn manage_index_bulk_round_trip() {
        let Ok(uri) = std::env::var("VITI_TEST_URI") else {
            eprintln!("VITI_TEST_URI not set; skipping");
            return;
        };
        let client = Client::with_uri_str(&uri).await.unwrap();
        let db = format!("viti_it_{}", uuid::Uuid::new_v4().simple());
        let ns = Namespace::new(&db, "c");
        let ctx = OpCtx::new(10_000);

        create_collection(
            &client,
            &db,
            "c",
            &CreateCollSpec {
                capped: Some((1 << 20, Some(1000))),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        create_collection(
            &client,
            &db,
            "v",
            &CreateCollSpec {
                view: Some(("c".into(), vec![doc! { "$match": { "a": { "$gt": 1 } } }])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let colls = list_collections(&client, &db).await.unwrap();
        assert_eq!(colls.len(), 2);
        assert!(
            colls
                .iter()
                .any(|c| c.name == "v" && c.kind == CollKind::View)
        );

        insert_many(
            &client,
            &ns,
            (0..10).map(|i| doc! { "a": i, "tags": ["x"] }).collect(),
        )
        .await
        .unwrap();

        // Indexes: create, list with stats, hide, drop.
        let name = create_index(&client, &ns, &doc! { "a": 1 }, &doc! { "unique": true })
            .await
            .unwrap();
        assert_eq!(name, "a_1");
        let ixs = list_indexes(&client, &ns).await.unwrap();
        let a1 = ixs.iter().find(|i| i.name == "a_1").unwrap();
        assert!(a1.properties().contains(&"unique"));
        assert!(a1.size.is_some());
        set_index_hidden(&client, &ns, "a_1", true).await.unwrap();
        assert!(
            list_indexes(&client, &ns)
                .await
                .unwrap()
                .iter()
                .any(|i| i.name == "a_1" && i.is_hidden())
        );
        drop_index(&client, &ns, "a_1").await.unwrap();
        assert_eq!(list_indexes(&client, &ns).await.unwrap().len(), 1);

        // Bulk: preview (falls back on a standalone), update, count, delete.
        let update = UpdateSpec::parse("{ $set: { b: true }, $inc: { a: 100 } }").unwrap();
        let (_, pairs) =
            preview_update(&client, &ns, doc! { "a": { "$gte": 8 } }, &update, 5, &ctx)
                .await
                .unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs[0].1.get_bool("b").unwrap());
        let out = update_many(
            &client,
            &ns,
            doc! { "a": { "$gte": 8 } },
            &update,
            None,
            false,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!((out.matched, out.modified), (2, 2));
        assert_eq!(
            count_filter(&client, &ns, doc! { "b": true }, None, &ctx)
                .await
                .unwrap(),
            2
        );
        let pipe = UpdateSpec::parse("[{ $set: { c: { $add: ['$a', 1] } } }]").unwrap();
        let (kind, pairs) = preview_update(&client, &ns, doc! { "a": 0 }, &pipe, 5, &ctx)
            .await
            .unwrap();
        assert!(matches!(
            kind,
            PreviewKind::Transaction | PreviewKind::Aggregation
        ));
        assert_eq!(pairs[0].1.get_i32("c").unwrap(), 1);
        // Capped collections refuse deletes; use a plain one for that.
        create_collection(&client, &db, "p", &CreateCollSpec::default())
            .await
            .unwrap();
        let pns = Namespace::new(&db, "p");
        insert_many(&client, &pns, (0..5).map(|i| doc! { "a": i }).collect())
            .await
            .unwrap();
        assert_eq!(
            delete_many(&client, &pns, doc! { "a": { "$lt": 3 } }, None, &ctx)
                .await
                .unwrap(),
            3
        );

        // Aggregate and explain (find + pipeline, both verbosities).
        let pipeline = vec![
            doc! { "$match": { "a": { "$gte": 100 } } },
            doc! { "$group": { "_id": null, "n": { "$sum": 1 } } },
        ];
        let out = aggregate(&client, &ns, pipeline.clone(), &AggOpts::default(), &ctx)
            .await
            .unwrap();
        assert_eq!(out[0].get_i32("n").unwrap(), 2);
        let spec = FindSpec {
            filter: doc! { "a": { "$gte": 100 } },
            sort: Some(doc! { "a": -1 }),
            ..Default::default()
        };
        let plan = explain_find(&client, &ns, &spec, "executionStats", &ctx)
            .await
            .unwrap();
        let summary = super::super::explain::parse(&plan);
        assert_eq!(summary.n_returned, Some(2));
        assert!(summary.collscan);
        assert!(summary.has_execution_stats);
        assert!(summary.root.is_some());
        let plan = explain_aggregate(
            &client,
            &ns,
            pipeline,
            &AggOpts::default(),
            "queryPlanner",
            &ctx,
        )
        .await
        .unwrap();
        let summary = super::super::explain::parse(&plan);
        assert!(!summary.has_execution_stats);
        assert!(summary.root.is_some());

        // Rename, stats, drop.
        rename_collection(&client, &pns, "q", false).await.unwrap();
        let stats = coll_stats(&client, &Namespace::new(&db, "q"))
            .await
            .unwrap();
        assert_eq!(stats.count, Some(2));
        drop_collection(&client, &Namespace::new(&db, "v"))
            .await
            .unwrap();
        drop_database(&client, &db).await.unwrap();
        assert!(
            !list_databases(&client)
                .await
                .unwrap()
                .iter()
                .any(|d| d.name == db)
        );
    }
}
