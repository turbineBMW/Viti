//! Client-side approximation of an update document, for the bulk-update
//! preview on servers without transactions (standalone). Covers the everyday
//! operators; anything else is reported so the UI can say "no preview".
//! Pipeline updates are previewed server-side with `aggregate` instead.
use bson::{Bson, Document};

/// Apply `update` (an operator document like `{ $set: {...} }`) to `doc`.
/// Returns `Err(operator)` for an operator that is not emulated.
pub fn apply(doc: &Document, update: &Document) -> Result<Document, String> {
    let mut out = doc.clone();
    for (op, arg) in update {
        let Bson::Document(fields) = arg else {
            return Err(format!("{op} expects a document"));
        };
        match op.as_str() {
            "$set" => {
                for (path, v) in fields {
                    set_path(&mut out, path, v.clone());
                }
            }
            "$unset" => {
                for path in fields.keys() {
                    remove_path(&mut out, path);
                }
            }
            "$inc" | "$mul" => {
                for (path, v) in fields {
                    // A missing field counts as 0 for $inc and is set to 0 by $mul.
                    let cur = get_path(&out, path).cloned().unwrap_or(Bson::Int32(0));
                    let new = arith(&cur, v, op == "$inc")
                        .ok_or_else(|| format!("{op} on non-numeric field {path}"))?;
                    set_path(&mut out, path, new);
                }
            }
            "$min" | "$max" => {
                for (path, v) in fields {
                    let replace = match get_path(&out, path) {
                        None => true,
                        Some(cur) => {
                            let ord = compare(cur, v);
                            if op == "$min" {
                                ord == std::cmp::Ordering::Greater
                            } else {
                                ord == std::cmp::Ordering::Less
                            }
                        }
                    };
                    if replace {
                        set_path(&mut out, path, v.clone());
                    }
                }
            }
            "$rename" => {
                for (from, to) in fields {
                    let Bson::String(to) = to else {
                        return Err("$rename target must be a string".into());
                    };
                    if let Some(v) = remove_path(&mut out, from) {
                        set_path(&mut out, to, v);
                    }
                }
            }
            "$currentDate" => {
                for (path, spec) in fields {
                    let ts = matches!(spec, Bson::Document(d) if d.get_str("$type").ok() == Some("timestamp"));
                    let now = bson::DateTime::now();
                    let v = if ts {
                        Bson::Timestamp(bson::Timestamp {
                            time: (now.timestamp_millis() / 1000) as u32,
                            increment: 1,
                        })
                    } else {
                        Bson::DateTime(now)
                    };
                    set_path(&mut out, path, v);
                }
            }
            "$push" | "$addToSet" => {
                for (path, v) in fields {
                    let mut arr = match get_path(&out, path) {
                        Some(Bson::Array(a)) => a.clone(),
                        Some(_) => return Err(format!("{op} on non-array field {path}")),
                        None => Vec::new(),
                    };
                    let items: Vec<Bson> = match v {
                        Bson::Document(d) if d.contains_key("$each") => match d.get("$each") {
                            Some(Bson::Array(each)) => each.clone(),
                            _ => return Err("$each expects an array".into()),
                        },
                        other => vec![other.clone()],
                    };
                    for item in items {
                        if op == "$addToSet" && arr.contains(&item) {
                            continue;
                        }
                        arr.push(item);
                    }
                    if let Bson::Document(d) = v {
                        if let Ok(n) = d
                            .get_i64("$slice")
                            .or_else(|_| d.get_i32("$slice").map(i64::from))
                        {
                            if n >= 0 {
                                arr.truncate(n as usize);
                            } else {
                                let keep = (-n) as usize;
                                if arr.len() > keep {
                                    arr.drain(..arr.len() - keep);
                                }
                            }
                        }
                        if d.contains_key("$sort") || d.contains_key("$position") {
                            return Err(format!("{op} with $sort/$position"));
                        }
                    }
                    set_path(&mut out, path, Bson::Array(arr));
                }
            }
            "$pull" => {
                for (path, cond) in fields {
                    if let Some(Bson::Array(a)) = get_path(&out, path) {
                        let kept: Vec<Bson> = a
                            .iter()
                            .filter(|item| !matches_cond(item, cond))
                            .cloned()
                            .collect();
                        set_path(&mut out, path, Bson::Array(kept));
                    }
                }
            }
            "$pullAll" => {
                for (path, values) in fields {
                    let Bson::Array(values) = values else {
                        return Err("$pullAll expects an array".into());
                    };
                    if let Some(Bson::Array(a)) = get_path(&out, path) {
                        let kept: Vec<Bson> =
                            a.iter().filter(|i| !values.contains(i)).cloned().collect();
                        set_path(&mut out, path, Bson::Array(kept));
                    }
                }
            }
            "$pop" => {
                for (path, dir) in fields {
                    if let Some(Bson::Array(a)) = get_path(&out, path) {
                        let mut a = a.clone();
                        let first = matches!(dir, Bson::Int32(-1) | Bson::Int64(-1))
                            || matches!(dir, Bson::Double(d) if *d < 0.0);
                        if !a.is_empty() {
                            if first {
                                a.remove(0);
                            } else {
                                a.pop();
                            }
                        }
                        set_path(&mut out, path, Bson::Array(a));
                    }
                }
            }
            "$setOnInsert" => {}
            other => return Err(other.to_string()),
        }
    }
    Ok(out)
}

/// `{ $pull: { tags: "x" } }` or `{ $pull: { tags: { $in: [...] } } }` /
/// `{ score: { $gt: 5 } }`; documents in arrays match by (sub)document equality
/// of the given fields.
fn matches_cond(item: &Bson, cond: &Bson) -> bool {
    match cond {
        Bson::Document(c) if c.keys().any(|k| k.starts_with('$')) => {
            c.iter().all(|(k, v)| match k.as_str() {
                "$in" => matches!(v, Bson::Array(a) if a.contains(item)),
                "$nin" => matches!(v, Bson::Array(a) if !a.contains(item)),
                "$eq" => item == v,
                "$ne" => item != v,
                "$gt" => compare(item, v) == std::cmp::Ordering::Greater,
                "$gte" => compare(item, v) != std::cmp::Ordering::Less,
                "$lt" => compare(item, v) == std::cmp::Ordering::Less,
                "$lte" => compare(item, v) != std::cmp::Ordering::Greater,
                _ => false,
            })
        }
        // A plain document matches array elements field by field; each field
        // may itself carry operators (`{ k: { $gt: 1 } }`).
        Bson::Document(c) => match item {
            Bson::Document(d) => c
                .iter()
                .all(|(k, v)| d.get(k).is_some_and(|dv| matches_cond(dv, v))),
            _ => false,
        },
        other => item == other,
    }
}

fn as_f64(b: &Bson) -> Option<f64> {
    match b {
        Bson::Int32(i) => Some(*i as f64),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    }
}

fn arith(cur: &Bson, by: &Bson, add: bool) -> Option<Bson> {
    match (cur, by) {
        (Bson::Int32(a), Bson::Int32(b)) => Some(if add {
            a.checked_add(*b)
                .map(Bson::Int32)
                .unwrap_or(Bson::Int64(*a as i64 + *b as i64))
        } else {
            a.checked_mul(*b)
                .map(Bson::Int32)
                .unwrap_or(Bson::Int64(*a as i64 * *b as i64))
        }),
        (Bson::Int64(a), Bson::Int32(b)) | (Bson::Int32(b), Bson::Int64(a)) => {
            Some(Bson::Int64(if add { a + *b as i64 } else { a * *b as i64 }))
        }
        (Bson::Int64(a), Bson::Int64(b)) => Some(Bson::Int64(if add { a + b } else { a * b })),
        _ => {
            let (a, b) = (as_f64(cur)?, as_f64(by)?);
            Some(Bson::Double(if add { a + b } else { a * b }))
        }
    }
}

fn compare(a: &Bson, b: &Bson) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if let (Some(x), Some(y)) = (as_f64(a), as_f64(b)) {
        return x.partial_cmp(&y).unwrap_or(Equal);
    }
    match (a, b) {
        (Bson::String(x), Bson::String(y)) => x.cmp(y),
        (Bson::DateTime(x), Bson::DateTime(y)) => x.cmp(y),
        (Bson::Boolean(x), Bson::Boolean(y)) => x.cmp(y),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

/// BSON comparison order across types.
fn type_rank(b: &Bson) -> u8 {
    match b {
        Bson::MinKey => 0,
        Bson::Null | Bson::Undefined => 1,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => 2,
        Bson::String(_) | Bson::Symbol(_) => 3,
        Bson::Document(_) => 4,
        Bson::Array(_) => 5,
        Bson::Binary(_) => 6,
        Bson::ObjectId(_) => 7,
        Bson::Boolean(_) => 8,
        Bson::DateTime(_) => 9,
        Bson::Timestamp(_) => 10,
        Bson::RegularExpression(_) => 11,
        _ => 12,
    }
}

pub fn get_path<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut cur: &Bson = doc.get(path.split('.').next()?)?;
    for seg in path.split('.').skip(1) {
        cur = match cur {
            Bson::Document(d) => d.get(seg)?,
            Bson::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Set a dotted path, creating intermediate documents (array indices are
/// honoured when the array already exists).
pub fn set_path(doc: &mut Document, path: &str, value: Bson) {
    let segs: Vec<&str> = path.split('.').collect();
    fn go(target: &mut Bson, segs: &[&str], value: Bson) {
        let (head, rest) = (segs[0], &segs[1..]);
        match target {
            Bson::Document(d) => {
                if rest.is_empty() {
                    d.insert(head, value);
                } else {
                    let child = d
                        .entry(head.to_string())
                        .or_insert_with(|| Bson::Document(Document::new()));
                    if !matches!(child, Bson::Document(_) | Bson::Array(_)) {
                        *child = Bson::Document(Document::new());
                    }
                    go(child, rest, value);
                }
            }
            Bson::Array(a) => {
                let Ok(i) = head.parse::<usize>() else { return };
                while a.len() <= i {
                    a.push(Bson::Null);
                }
                if rest.is_empty() {
                    a[i] = value;
                } else {
                    if !matches!(a[i], Bson::Document(_) | Bson::Array(_)) {
                        a[i] = Bson::Document(Document::new());
                    }
                    go(&mut a[i], rest, value);
                }
            }
            _ => {}
        }
    }
    let mut root = Bson::Document(std::mem::take(doc));
    go(&mut root, &segs, value);
    if let Bson::Document(d) = root {
        *doc = d;
    }
}

pub fn remove_path(doc: &mut Document, path: &str) -> Option<Bson> {
    let (head, rest) = match path.split_once('.') {
        Some((h, r)) => (h, Some(r)),
        None => (path, None),
    };
    match rest {
        None => doc.remove(head),
        Some(rest) => match doc.get_mut(head)? {
            Bson::Document(d) => remove_path(d, rest),
            Bson::Array(a) => {
                let i: usize = rest.split('.').next()?.parse().ok()?;
                match rest.split_once('.') {
                    None => a.get_mut(i).map(|v| std::mem::replace(v, Bson::Null)),
                    Some((_, deeper)) => match a.get_mut(i)? {
                        Bson::Document(d) => remove_path(d, deeper),
                        _ => None,
                    },
                }
            }
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn set_unset_inc_rename() {
        let d = doc! { "a": 1, "b": { "c": 2 }, "tags": ["x", "y"] };
        let u = doc! {
            "$set": { "b.d": "new", "e.f.g": true },
            "$unset": { "a": "" },
            "$inc": { "b.c": 3, "n": 1.5 },
            "$rename": { "tags": "labels" },
        };
        let out = apply(&d, &u).unwrap();
        assert_eq!(
            out,
            doc! { "b": { "c": 5, "d": "new" }, "e": { "f": { "g": true } }, "n": 1.5, "labels": ["x", "y"] }
        );
    }

    #[test]
    fn arrays_and_minmax() {
        let d = doc! { "tags": ["a", "b", "b"], "score": 10, "items": [ { "k": 1 }, { "k": 2 } ] };
        let u = doc! {
            "$push": { "tags": { "$each": ["c", "d"], "$slice": -4 } },
            "$addToSet": { "new": "z" },
            "$pull": { "items": { "k": { "$gt": 1 } } },
            "$min": { "score": 3 },
            "$max": { "hi": 7 },
            "$pop": { "tags": 1 },
        };
        let out = apply(&d, &u).unwrap();
        assert_eq!(
            out.get_array("tags").unwrap(),
            &vec![Bson::from("b"), Bson::from("b"), Bson::from("c")]
        );
        assert_eq!(out.get_array("new").unwrap(), &vec![Bson::from("z")]);
        assert_eq!(
            out.get_array("items").unwrap(),
            &vec![Bson::Document(doc! { "k": 1 })]
        );
        assert_eq!(out.get_i32("score").unwrap(), 3);
        assert_eq!(out.get_i32("hi").unwrap(), 7);
    }

    #[test]
    fn unsupported_is_reported() {
        let d = doc! { "a": 1 };
        assert_eq!(
            apply(&d, &doc! { "$bit": { "a": { "and": 1 } } }),
            Err("$bit".into())
        );
        assert!(apply(&d, &doc! { "$set": 1 }).is_err());
    }
}
