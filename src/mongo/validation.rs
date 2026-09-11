//! Collection validation rules: read and write the validator through
//! `listCollections` / `collMod`, and generate a `$jsonSchema` from a schema
//! analysis (pure, tested).
use super::ops::Namespace;
use super::schema::{Field, Schema};
use anyhow::{Context, Result};
use bson::{Bson, Document, doc};
use mongodb::Client;

pub const LEVELS: [&str; 3] = ["strict", "moderate", "off"];
pub const ACTIONS: [&str; 2] = ["error", "warn"];

#[derive(Clone, Debug, PartialEq)]
pub struct Validation {
    pub validator: Document,
    pub level: String,
    pub action: String,
}

impl Default for Validation {
    fn default() -> Self {
        Self {
            validator: Document::new(),
            level: "strict".into(),
            action: "error".into(),
        }
    }
}

/// The collection's current rules (defaults when it has none).
pub async fn fetch(client: &Client, ns: &Namespace) -> Result<Validation> {
    let r = client
        .database(&ns.db)
        .run_command(doc! { "listCollections": 1, "filter": { "name": ns.coll.clone() } })
        .await
        .with_context(|| format!("listCollections for {} failed", ns))?;
    let first = r
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_array("firstBatch").ok())
        .and_then(|b| b.first())
        .and_then(|b| b.as_document())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{} does not exist", ns))?;
    let options = first.get_document("options").cloned().unwrap_or_default();
    Ok(Validation {
        validator: options
            .get_document("validator")
            .cloned()
            .unwrap_or_default(),
        level: options
            .get_str("validationLevel")
            .unwrap_or("strict")
            .to_string(),
        action: options
            .get_str("validationAction")
            .unwrap_or("error")
            .to_string(),
    })
}

pub fn coll_mod_command(coll: &str, v: &Validation) -> Document {
    doc! {
        "collMod": coll,
        "validator": v.validator.clone(),
        "validationLevel": v.level.clone(),
        "validationAction": v.action.clone(),
    }
}

/// Apply the rules with `collMod`. An empty validator removes validation.
pub async fn set(client: &Client, ns: &Namespace, v: &Validation) -> Result<()> {
    client
        .database(&ns.db)
        .run_command(coll_mod_command(&ns.coll, v))
        .await
        .with_context(|| format!("collMod on {} failed", ns))?;
    Ok(())
}

/// The `$jsonSchema` alias for one of `ejson::type_name`'s names.
pub fn bson_type_alias(name: &str) -> &'static str {
    match name {
        "Object" => "object",
        "Array" => "array",
        "String" => "string",
        "Symbol" => "symbol",
        "Int32" => "int",
        "Int64" => "long",
        "Double" => "double",
        "Decimal128" => "decimal",
        "Boolean" => "bool",
        "Null" => "null",
        "Date" => "date",
        "ObjectId" => "objectId",
        "RegExp" => "regex",
        "Binary" => "binData",
        "Timestamp" => "timestamp",
        "Code" => "javascript",
        "CodeWScope" => "javascriptWithScope",
        "MinKey" => "minKey",
        "MaxKey" => "maxKey",
        "DBPointer" => "dbPointer",
        _ => "undefined",
    }
}

fn type_list(names: Vec<&'static str>) -> Bson {
    let mut names = names;
    names.dedup();
    if names.len() == 1 {
        Bson::String(names[0].into())
    } else {
        Bson::Array(names.into_iter().map(|n| Bson::String(n.into())).collect())
    }
}

/// `(properties, required)` for a set of sibling fields: a field is required
/// when every sampled parent had it.
fn object_parts(fields: &[Field]) -> (Document, Vec<String>) {
    let mut props = Document::new();
    let mut required = Vec::new();
    for f in fields {
        props.insert(f.name.clone(), property(f));
        if f.count == f.parent_count && f.parent_count > 0 {
            required.push(f.name.clone());
        }
    }
    (props, required)
}

fn property(f: &Field) -> Document {
    let mut p = Document::new();
    let names: Vec<&'static str> = f.types.iter().map(|t| bson_type_alias(t.name)).collect();
    if !names.is_empty() {
        p.insert("bsonType", type_list(names));
    }
    let is_object = f.types.iter().any(|t| t.name == "Object");
    let array = f.types.iter().find(|t| t.name == "Array");
    if is_object && !f.children.is_empty() {
        let (props, required) = object_parts(&f.children);
        if !required.is_empty() {
            p.insert("required", required);
        }
        p.insert("properties", props);
    }
    if let Some(arr) = array {
        let elem_names: Vec<&'static str> = arr
            .elements
            .iter()
            .map(|t| bson_type_alias(t.name))
            .collect();
        if !elem_names.is_empty() {
            let mut items = doc! { "bsonType": type_list(elem_names) };
            if arr.elements.iter().any(|t| t.name == "Object")
                && !is_object
                && !f.children.is_empty()
            {
                let (props, required) = object_parts(&f.children);
                if !required.is_empty() {
                    items.insert("required", required);
                }
                items.insert("properties", props);
            }
            p.insert("items", items);
        }
    }
    p
}

/// A `{ $jsonSchema: … }` validator describing the sampled documents: every
/// field with the types seen, required when always present, nested objects
/// and array items recursively.
pub fn json_schema(schema: &Schema) -> Document {
    let (props, required) = object_parts(&schema.fields);
    let mut s = doc! { "bsonType": "object" };
    if !required.is_empty() {
        s.insert("required", required);
    }
    s.insert("properties", props);
    doc! { "$jsonSchema": s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mongo::schema::analyze;

    #[test]
    fn generates_json_schema() {
        let docs = vec![
            doc! { "_id": 1, "name": "a", "age": 3, "tags": ["x"], "addr": { "city": "Oslo", "zip": "1" }, "items": [ { "sku": "s", "qty": 1 } ] },
            doc! { "_id": 2, "name": "b", "age": "old", "tags": [], "addr": { "city": "Lima" } },
        ];
        let s = json_schema(&analyze(&docs));
        let js = s.get_document("$jsonSchema").unwrap();
        assert_eq!(js.get_str("bsonType").unwrap(), "object");
        let required = js.get_array("required").unwrap();
        let req: Vec<&str> = required.iter().filter_map(|b| b.as_str()).collect();
        assert_eq!(req, vec!["_id", "addr", "age", "name", "tags"]);
        let props = js.get_document("properties").unwrap();
        assert_eq!(
            props
                .get_document("name")
                .unwrap()
                .get_str("bsonType")
                .unwrap(),
            "string"
        );
        let age = props.get_document("age").unwrap();
        let ts: Vec<&str> = age
            .get_array("bsonType")
            .unwrap()
            .iter()
            .filter_map(|b| b.as_str())
            .collect();
        assert_eq!(ts, vec!["int", "string"]);
        let addr = props.get_document("addr").unwrap();
        assert_eq!(addr.get_str("bsonType").unwrap(), "object");
        let addr_req: Vec<&str> = addr
            .get_array("required")
            .unwrap()
            .iter()
            .filter_map(|b| b.as_str())
            .collect();
        assert_eq!(addr_req, vec!["city"]);
        assert!(addr.get_document("properties").unwrap().contains_key("zip"));
        let tags = props.get_document("tags").unwrap();
        assert_eq!(tags.get_str("bsonType").unwrap(), "array");
        assert_eq!(
            tags.get_document("items")
                .unwrap()
                .get_str("bsonType")
                .unwrap(),
            "string"
        );
        let items = props.get_document("items").unwrap();
        let item = items.get_document("items").unwrap();
        assert_eq!(item.get_str("bsonType").unwrap(), "object");
        assert!(item.get_document("properties").unwrap().contains_key("qty"));
    }

    #[test]
    fn coll_mod() {
        let v = Validation {
            validator: doc! { "a": 1 },
            level: "moderate".into(),
            action: "warn".into(),
        };
        let c = coll_mod_command("c", &v);
        assert_eq!(c.get_str("collMod").unwrap(), "c");
        assert_eq!(c.get_str("validationLevel").unwrap(), "moderate");
        assert_eq!(c.get_str("validationAction").unwrap(), "warn");
        assert_eq!(bson_type_alias("Decimal128"), "decimal");
        assert_eq!(bson_type_alias("Nope"), "undefined");
    }
}
