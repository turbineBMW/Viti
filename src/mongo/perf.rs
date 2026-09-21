//! The Performance page's model: one `Snapshot` per `serverStatus`, `Rates`
//! between two snapshots, the active operations from `$currentOp`, and the
//! hottest collections from two `top` results. Pure parsing; `ui/performance.rs`
//! polls and draws.
use bson::{Bson, Document};

/// A dotted-path numeric getter tolerant of Int32 / Int64 / Double.
pub fn num(doc: &Document, path: &str) -> Option<f64> {
    let mut cur: &Document = doc;
    let mut parts = path.split('.').peekable();
    while let Some(p) = parts.next() {
        let v = cur.get(p)?;
        if parts.peek().is_none() {
            return as_f64(v);
        }
        cur = v.as_document()?;
    }
    None
}

pub fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) => Some(*n),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counters {
    pub insert: f64,
    pub query: f64,
    pub update: f64,
    pub delete: f64,
    pub getmore: f64,
    pub command: f64,
}

/// One `serverStatus`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub uptime_s: f64,
    pub host: String,
    pub version: String,
    pub process: String,
    pub ops: Counters,
    pub active_readers: f64,
    pub active_writers: f64,
    pub queued_readers: f64,
    pub queued_writers: f64,
    pub connections: f64,
    pub connections_available: f64,
    pub mem_resident_mb: f64,
    pub mem_virtual_mb: f64,
    pub net_in: f64,
    pub net_out: f64,
    pub net_requests: f64,
}

impl Snapshot {
    pub fn parse(doc: &Document) -> Snapshot {
        let n = |p: &str| num(doc, p).unwrap_or(0.0);
        let s = |k: &str| doc.get_str(k).unwrap_or("").to_string();
        Snapshot {
            uptime_s: n("uptime"),
            host: s("host"),
            version: s("version"),
            process: s("process"),
            ops: Counters {
                insert: n("opcounters.insert"),
                query: n("opcounters.query"),
                update: n("opcounters.update"),
                delete: n("opcounters.delete"),
                getmore: n("opcounters.getmore"),
                command: n("opcounters.command"),
            },
            active_readers: n("globalLock.activeClients.readers"),
            active_writers: n("globalLock.activeClients.writers"),
            queued_readers: n("globalLock.currentQueue.readers"),
            queued_writers: n("globalLock.currentQueue.writers"),
            connections: n("connections.current"),
            connections_available: n("connections.available"),
            mem_resident_mb: n("mem.resident"),
            mem_virtual_mb: n("mem.virtual"),
            net_in: n("network.bytesIn"),
            net_out: n("network.bytesOut"),
            net_requests: n("network.numRequests"),
        }
    }
}

/// Per-second rates between two snapshots plus the gauges of the newer one:
/// one point on every chart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rates {
    pub ops: Counters,
    pub active_readers: f64,
    pub active_writers: f64,
    pub queued_readers: f64,
    pub queued_writers: f64,
    pub connections: f64,
    pub mem_resident_mb: f64,
    pub mem_virtual_mb: f64,
    /// bytes / s
    pub net_in: f64,
    pub net_out: f64,
    pub requests: f64,
}

impl Rates {
    /// `dt` in seconds. Counters that went backwards (restart) count as zero.
    pub fn between(prev: &Snapshot, next: &Snapshot, dt: f64) -> Rates {
        let dt = if dt > 0.0 { dt } else { 1.0 };
        let rate = |a: f64, b: f64| ((b - a) / dt).max(0.0);
        Rates {
            ops: Counters {
                insert: rate(prev.ops.insert, next.ops.insert),
                query: rate(prev.ops.query, next.ops.query),
                update: rate(prev.ops.update, next.ops.update),
                delete: rate(prev.ops.delete, next.ops.delete),
                getmore: rate(prev.ops.getmore, next.ops.getmore),
                command: rate(prev.ops.command, next.ops.command),
            },
            active_readers: next.active_readers,
            active_writers: next.active_writers,
            queued_readers: next.queued_readers,
            queued_writers: next.queued_writers,
            connections: next.connections,
            mem_resident_mb: next.mem_resident_mb,
            mem_virtual_mb: next.mem_virtual_mb,
            net_in: rate(prev.net_in, next.net_in),
            net_out: rate(prev.net_out, next.net_out),
            requests: rate(prev.net_requests, next.net_requests),
        }
    }
}

// ----- current operations ----------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CurrentOp {
    pub opid: Bson,
    /// `query`, `update`, `command`, `getmore`…
    pub op: String,
    pub ns: String,
    pub secs_running: f64,
    pub active: bool,
    pub waiting_for_lock: bool,
    pub client: String,
    pub app_name: String,
    /// The command, compacted, for the details view.
    pub command: Document,
    pub comment: String,
    pub plan_summary: String,
}

/// Active operations sorted slowest first. Viti's own polling (`$currentOp`,
/// `serverStatus`, `top`) is left out.
pub fn parse_current_ops(docs: &[Document]) -> Vec<CurrentOp> {
    let mut out: Vec<CurrentOp> = docs
        .iter()
        .filter_map(|d| {
            let command = d.get_document("command").cloned().unwrap_or_default();
            if is_own_poll(&command) || is_noise(d, &command) {
                return None;
            }
            let opid = d.get("opid")?.clone();
            Some(CurrentOp {
                opid,
                op: d.get_str("op").unwrap_or("").to_string(),
                ns: d.get_str("ns").unwrap_or("").to_string(),
                secs_running: num(d, "secs_running")
                    .or_else(|| num(d, "microsecs_running").map(|u| u / 1e6))
                    .unwrap_or(0.0),
                active: d.get_bool("active").unwrap_or(false),
                waiting_for_lock: d.get_bool("waitingForLock").unwrap_or(false),
                client: d.get_str("client").unwrap_or("").to_string(),
                app_name: d.get_str("appName").unwrap_or("").to_string(),
                comment: command.get_str("comment").unwrap_or("").to_string(),
                plan_summary: d.get_str("planSummary").unwrap_or("").to_string(),
                command,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.secs_running
            .partial_cmp(&a.secs_running)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Idle placeholders and the drivers' awaitable `hello` heartbeats.
fn is_noise(op: &Document, command: &Document) -> bool {
    if op.get_str("op").unwrap_or("") == "none" {
        return true;
    }
    matches!(
        command.keys().next().map(String::as_str),
        Some("hello" | "isMaster" | "ismaster")
    )
}

fn is_own_poll(command: &Document) -> bool {
    if command.contains_key("serverStatus") || command.contains_key("top") {
        return true;
    }
    if let Ok(pipeline) = command.get_array("pipeline") {
        return pipeline
            .iter()
            .filter_map(|s| s.as_document())
            .any(|s| s.contains_key("$currentOp"));
    }
    command.contains_key("currentOp")
}

impl CurrentOp {
    /// `query on db.coll` style one-liner.
    pub fn label(&self) -> String {
        let what = if self.op == "command" {
            self.command
                .keys()
                .next()
                .map(|k| k.to_string())
                .unwrap_or_else(|| "command".into())
        } else {
            self.op.clone()
        };
        if self.ns.is_empty() {
            what
        } else {
            format!("{what} on {}", self.ns)
        }
    }
}

// ----- hottest collections (`top`) -----------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopEntry {
    pub ns: String,
    pub total_count: f64,
    pub total_time_us: f64,
    pub read_count: f64,
    pub write_count: f64,
}

/// `top`'s `totals`, minus the note and the system databases.
pub fn parse_top(doc: &Document) -> Vec<TopEntry> {
    let Ok(totals) = doc.get_document("totals") else {
        return Vec::new();
    };
    totals
        .iter()
        .filter_map(|(ns, v)| {
            let d = v.as_document()?;
            if ns == "note" || is_system_ns(ns) {
                return None;
            }
            let n = |p: &str| num(d, p).unwrap_or(0.0);
            Some(TopEntry {
                ns: ns.clone(),
                total_count: n("total.count"),
                total_time_us: n("total.time"),
                read_count: n("readLock.count"),
                write_count: n("writeLock.count"),
            })
        })
        .collect()
}

fn is_system_ns(ns: &str) -> bool {
    ns.starts_with("admin.")
        || ns.starts_with("local.")
        || ns.starts_with("config.")
        || ns.contains(".system.")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HotCollection {
    pub ns: String,
    pub ops_per_s: f64,
    /// 0..=1 of this namespace's operations that were reads.
    pub read_share: f64,
    /// Server time spent per second of wall clock (µs/s → a load fraction).
    pub load: f64,
}

/// The namespaces with the most operations between two `top` results,
/// busiest first, at most `limit`, idle ones left out.
pub fn hottest(prev: &[TopEntry], next: &[TopEntry], dt: f64, limit: usize) -> Vec<HotCollection> {
    let dt = if dt > 0.0 { dt } else { 1.0 };
    let mut out: Vec<HotCollection> = next
        .iter()
        .filter_map(|n| {
            let p = prev.iter().find(|p| p.ns == n.ns)?;
            let count = (n.total_count - p.total_count).max(0.0);
            if count <= 0.0 {
                return None;
            }
            let reads = (n.read_count - p.read_count).max(0.0);
            let writes = (n.write_count - p.write_count).max(0.0);
            let time = (n.total_time_us - p.total_time_us).max(0.0);
            Some(HotCollection {
                ns: n.ns.clone(),
                ops_per_s: count / dt,
                read_share: if reads + writes > 0.0 {
                    reads / (reads + writes)
                } else {
                    1.0
                },
                load: time / 1e6 / dt,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.ops_per_s
            .partial_cmp(&a.ops_per_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

/// `1.2k`, `35`, `0.4` for chart legends.
pub fn short_number(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.1}G", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 10.0 || v == v.trunc() {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

pub fn short_bytes(v: f64) -> String {
    let a = v.abs();
    if a >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", v / (1024.0 * 1024.0 * 1024.0))
    } else if a >= 1024.0 * 1024.0 {
        format!("{:.1} MB", v / (1024.0 * 1024.0))
    } else if a >= 1024.0 {
        format!("{:.1} kB", v / 1024.0)
    } else {
        format!("{v:.0} B")
    }
}

pub fn uptime_text(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3_600, (s % 3_600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn status(insert: i64, bytes_in: i64) -> Document {
        doc! {
            "host": "box:27017", "version": "7.0.1", "process": "mongod", "uptime": 125,
            "opcounters": { "insert": insert, "query": 10i32, "update": 0, "delete": 0, "getmore": 0, "command": 100.0 },
            "globalLock": { "activeClients": { "readers": 2, "writers": 1 }, "currentQueue": { "readers": 0, "writers": 3 } },
            "connections": { "current": 7, "available": 993 },
            "mem": { "resident": 120, "virtual": 1500 },
            "network": { "bytesIn": bytes_in, "bytesOut": 5000, "numRequests": 50 },
        }
    }

    #[test]
    fn snapshot_and_rates() {
        let a = Snapshot::parse(&status(100, 1000));
        assert_eq!(a.host, "box:27017");
        assert_eq!(a.uptime_s, 125.0);
        assert_eq!(a.ops.insert, 100.0);
        assert_eq!(a.ops.query, 10.0);
        assert_eq!(a.ops.command, 100.0);
        assert_eq!(a.active_readers, 2.0);
        assert_eq!(a.queued_writers, 3.0);
        assert_eq!(a.connections, 7.0);
        assert_eq!(a.mem_resident_mb, 120.0);
        let b = Snapshot::parse(&status(160, 3000));
        let r = Rates::between(&a, &b, 2.0);
        assert_eq!(r.ops.insert, 30.0);
        assert_eq!(r.ops.query, 0.0);
        assert_eq!(r.net_in, 1000.0);
        assert_eq!(r.connections, 7.0);
        // Counter reset: never negative.
        let r2 = Rates::between(&b, &a, 1.0);
        assert_eq!(r2.ops.insert, 0.0);
        assert_eq!(r2.net_in, 0.0);
        // Missing sections parse as zero.
        assert_eq!(Snapshot::parse(&doc! {}).ops.insert, 0.0);
    }

    #[test]
    fn current_ops_skip_own_polls_and_sort() {
        let docs = vec![
            doc! { "opid": 1, "op": "query", "ns": "db.a", "secs_running": 2, "active": true,
            "command": { "find": "a", "filter": {}, "comment": "viti:x" }, "planSummary": "COLLSCAN" },
            doc! { "opid": 2, "op": "command", "ns": "admin.$cmd", "secs_running": 0, "active": true,
            "command": { "aggregate": 1, "pipeline": [ { "$currentOp": {} } ] } },
            doc! { "opid": 3, "op": "command", "ns": "admin.$cmd", "microsecs_running": 7_500_000i64, "active": true,
            "command": { "serverStatus": 1 } },
            doc! { "opid": 4, "op": "update", "ns": "db.b", "microsecs_running": 7_500_000i64, "active": true,
            "command": { "update": "b" }, "waitingForLock": true },
            doc! { "desc": "no opid" },
            doc! { "opid": 5, "op": "none", "active": true, "command": {} },
            doc! { "opid": 6, "op": "command", "active": true, "secs_running": 9,
            "command": { "hello": 1, "maxAwaitTimeMS": 10000 } },
        ];
        let ops = parse_current_ops(&docs);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].opid, Bson::Int32(4));
        assert_eq!(ops[0].secs_running, 7.5);
        assert!(ops[0].waiting_for_lock);
        assert_eq!(ops[0].label(), "update on db.b");
        assert_eq!(ops[1].comment, "viti:x");
        assert_eq!(ops[1].plan_summary, "COLLSCAN");
        let cmd = CurrentOp {
            op: "command".into(),
            command: doc! { "count": "a" },
            ..Default::default()
        };
        assert_eq!(cmd.label(), "count");
    }

    #[test]
    fn top_and_hottest() {
        let t = |a: i64, b: i64| {
            doc! { "totals": {
                "note": "all times in microseconds",
                "shop.orders": { "total": { "time": a * 1000, "count": a }, "readLock": { "count": a }, "writeLock": { "count": 0 } },
                "shop.users": { "total": { "time": b * 10, "count": b }, "readLock": { "count": 0 }, "writeLock": { "count": b } },
                "admin.system.version": { "total": { "time": 1, "count": 1 } },
                "config.x": { "total": { "time": 1, "count": 1 } },
            } }
        };
        let p = parse_top(&t(10, 5));
        assert_eq!(p.len(), 2);
        let n = parse_top(&t(30, 5));
        let hot = hottest(&p, &n, 2.0, 5);
        assert_eq!(hot.len(), 1, "idle namespaces are left out");
        assert_eq!(hot[0].ns, "shop.orders");
        assert_eq!(hot[0].ops_per_s, 10.0);
        assert_eq!(hot[0].read_share, 1.0);
        assert!((hot[0].load - 0.01).abs() < 1e-9);
        assert!(parse_top(&doc! {}).is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(short_number(0.0), "0");
        assert_eq!(short_number(0.44), "0.4");
        assert_eq!(short_number(12.7), "13");
        assert_eq!(short_number(1500.0), "1.5k");
        assert_eq!(short_number(2_500_000.0), "2.5M");
        assert_eq!(short_bytes(512.0), "512 B");
        assert_eq!(short_bytes(2048.0), "2.0 kB");
        assert_eq!(uptime_text(59.0), "0m 59s");
        assert_eq!(uptime_text(3_661.0), "1h 1m");
        assert_eq!(uptime_text(90_000.0), "1d 1h 0m");
    }
}
