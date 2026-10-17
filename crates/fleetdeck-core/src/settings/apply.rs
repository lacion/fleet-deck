//! Applies a change to a `toml_edit` document, so that the comments and the
//! order of the file stay (design 1.3, step 2). Only the changed keys are
//! written; a key whose new value equals the shown value is not touched.

use std::collections::BTreeSet;

use fleetdeck_proto::settings::{FieldKind, FieldMeta};
use serde_json::{Map, Value};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike};

use super::error::SaveError;
use crate::config::{RAW_INSTANCE_KEYS, valid_env_key};

const INSTANCES: &str = "provider_instance";

/// Applies `changes` (a key and a JSON value; `null` removes the key).
/// `current` holds the values that the view shows now.
pub(super) fn apply_changes(
    doc: &mut DocumentMut,
    fields: &[FieldMeta],
    changes: &Map<String, Value>,
    current: &Map<String, Value>,
) -> Result<(), SaveError> {
    for (key, new) in changes {
        let meta = fields
            .iter()
            .find(|f| f.key == *key)
            .filter(|f| !f.read_only)
            .ok_or_else(|| SaveError::invalid(key))?;
        if current.get(key) == Some(new) {
            continue;
        }
        if new.is_null() {
            remove_key(doc, key);
            continue;
        }
        let bad = || SaveError::invalid(key);
        match meta.kind {
            FieldKind::Bool => set_value(doc, key, new.as_bool().ok_or_else(bad)?.into())?,
            FieldKind::Int => {
                let n = new.as_i64().ok_or_else(bad)?;
                if meta.min.is_some_and(|m| n < m) || meta.max.is_some_and(|m| n > m) {
                    return Err(bad());
                }
                set_value(doc, key, n.into())?;
            }
            FieldKind::Text | FieldKind::Path | FieldKind::Enum => {
                let s = new.as_str().ok_or_else(bad)?;
                if meta.kind == FieldKind::Enum && !meta.options.iter().any(|o| o == s) {
                    return Err(bad());
                }
                set_value(doc, key, s.into())?;
            }
            FieldKind::Paths | FieldKind::Texts | FieldKind::Addresses => {
                let list = new.as_array().ok_or_else(bad)?;
                let mut array = Array::new();
                for item in list {
                    array.push(item.as_str().ok_or_else(bad)?);
                }
                set_value(doc, key, array.into())?;
            }
            FieldKind::RepoGroups => set_repo_groups(doc, key, new)?,
            FieldKind::Instances => apply_instances(doc, new, current.get(key))?,
            FieldKind::Trackers => return Err(bad()),
        }
    }
    Ok(())
}

/// The table of a key and the key inside it: `model_help.enabled` is the
/// table `model_help` and the name `enabled`.
fn parent_mut<'a>(
    doc: &'a mut DocumentMut,
    key: &'a str,
    create: bool,
) -> Result<Option<(&'a mut dyn TableLike, &'a str)>, SaveError> {
    let root = doc.as_table_mut();
    let Some((table, name)) = key.split_once('.') else {
        return Ok(Some((root, key)));
    };
    if !root.contains_key(table) {
        if !create {
            return Ok(None);
        }
        root.insert(table, Item::Table(Table::new()));
    }
    root.get_mut(table)
        .and_then(Item::as_table_like_mut)
        .map(|t| Some((t, name)))
        .ok_or_else(|| SaveError::invalid(key))
}

fn remove_key(doc: &mut DocumentMut, key: &str) {
    if let Ok(Some((table, name))) = parent_mut(doc, key, false) {
        table.remove(name);
    }
}

/// Replaces a value and keeps the comments around it.
fn set_value(doc: &mut DocumentMut, key: &str, new: toml_edit::Value) -> Result<(), SaveError> {
    let (table, name) = parent_mut(doc, key, true)?.ok_or_else(|| SaveError::invalid(key))?;
    put(table, name, new);
    Ok(())
}

fn put(table: &mut dyn TableLike, name: &str, mut new: toml_edit::Value) {
    if let Some(Item::Value(old)) = table.get_mut(name) {
        *new.decor_mut() = old.decor().clone();
        *old = new;
    } else {
        table.insert(name, Item::Value(new));
    }
}

fn set_repo_groups(doc: &mut DocumentMut, key: &str, new: &Value) -> Result<(), SaveError> {
    let bad = || SaveError::invalid(key);
    let map = new.as_object().ok_or_else(bad)?;
    let mut fresh = Table::new();
    for (path, group) in map {
        fresh.insert(path, toml_edit::value(group.as_str().ok_or_else(bad)?));
    }
    let root = doc.as_table_mut();
    match root.get_mut(key) {
        Some(Item::Table(t)) => {
            t.clear();
            for (k, v) in fresh {
                t.insert(&k, v);
            }
        }
        _ => {
            root.insert(key, Item::Table(fresh));
        }
    }
    Ok(())
}

/// Takes the `[[provider_instance]]` tables out of the document and leaves
/// an empty array of tables at the same place, so that the order of the file
/// stays. [`put_instances`] fills it again.
fn take_instances(doc: &mut DocumentMut) -> Vec<Table> {
    let root = doc.as_table_mut();
    match root.get_mut(INSTANCES) {
        Some(Item::ArrayOfTables(list)) => std::mem::take(list).into_iter().collect(),
        Some(Item::Value(toml_edit::Value::Array(list))) => {
            let tables = std::mem::take(list)
                .into_iter()
                .filter_map(|v| match v {
                    toml_edit::Value::InlineTable(t) => Some(t.into_table()),
                    _ => None,
                })
                .collect();
            root.insert(INSTANCES, Item::ArrayOfTables(ArrayOfTables::new()));
            tables
        }
        _ => {
            root.insert(INSTANCES, Item::ArrayOfTables(ArrayOfTables::new()));
            Vec::new()
        }
    }
}

fn put_instances(doc: &mut DocumentMut, list: Vec<Table>) {
    if list.is_empty() {
        // `provider_instance = []` means no instance; no key means the defaults.
        doc.as_table_mut()
            .insert(INSTANCES, Item::Value(Array::new().into()));
    } else if let Some(Item::ArrayOfTables(out)) = doc.as_table_mut().get_mut(INSTANCES) {
        for t in list {
            out.push(t);
        }
    }
}

fn id_of(t: &Table) -> Option<&str> {
    t.get("id").and_then(Item::as_str)
}

fn is_set_marker(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == 1 && o.get("set") == Some(&Value::Bool(true)))
}

/// Applies the instance list. An instance is matched by `id`. In `env`, an
/// absent entry keeps its value, `null` removes it and `{ "set": true }`
/// keeps it; a string is refused (a secret comes only through the relay).
fn apply_instances(
    doc: &mut DocumentMut,
    new: &Value,
    shown: Option<&Value>,
) -> Result<(), SaveError> {
    let bad = || SaveError::invalid(INSTANCES);
    let list = new.as_array().ok_or_else(bad)?;
    let mut existing = take_instances(doc);
    let mut out: Vec<Table> = Vec::with_capacity(list.len());
    for entry in list {
        let obj = entry.as_object().ok_or_else(bad)?;
        if obj.keys().any(|k| !RAW_INSTANCE_KEYS.contains(&k.as_str())) {
            return Err(bad());
        }
        let id = obj.get("id").and_then(Value::as_str).ok_or_else(bad)?;
        let old_shown = shown.and_then(Value::as_array).and_then(|l| {
            l.iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some(id))
        });
        let mut table = match existing.iter().position(|t| id_of(t) == Some(id)) {
            Some(i) => existing.remove(i),
            None => Table::new(),
        };
        put(&mut table, "id", id.into());
        let driver = obj.get("driver").and_then(Value::as_str).ok_or_else(bad)?;
        if !matches!(driver, "claude" | "codex") {
            return Err(bad());
        }
        put(&mut table, "driver", driver.into());
        for field in ["binary", "config_home"] {
            let Some(v) = obj.get(field) else { continue };
            if old_shown.and_then(|o| o.get(field)) == Some(v) {
                continue;
            }
            match v {
                Value::Null => {
                    table.remove(field);
                }
                Value::String(s) if s.is_empty() => {
                    table.remove(field);
                }
                Value::String(s)
                    if field == "binary" && s == driver && !table.contains_key(field) => {}
                Value::String(s) => put(&mut table, field, s.as_str().into()),
                _ => return Err(bad()),
            }
        }
        match obj.get("env") {
            None | Some(Value::Null) => {}
            Some(Value::Object(env)) => apply_env(&mut table, env)?,
            Some(_) => return Err(bad()),
        }
        out.push(table);
    }
    put_instances(doc, out);
    Ok(())
}

fn apply_env(table: &mut Table, env: &Map<String, Value>) -> Result<(), SaveError> {
    for (var, v) in env {
        let key = format!("{INSTANCES}.env.{var}");
        if !valid_env_key(var) {
            return Err(SaveError::invalid(INSTANCES));
        }
        let has = table
            .get("env")
            .and_then(Item::as_table_like)
            .is_some_and(|e| e.contains_key(var));
        match v {
            Value::Null => {
                if let Some(e) = table.get_mut("env").and_then(Item::as_table_like_mut) {
                    e.remove(var);
                }
            }
            v if is_set_marker(v) && has => {}
            Value::String(_) => return Err(SaveError::SecretViaRelay { key }),
            _ => return Err(SaveError::invalid(INSTANCES)),
        }
    }
    Ok(())
}

/// Sets `env.<var>` of the instance `id` to `{ secret = "<name>" }`. A file
/// without `provider_instance` has the default instances `default_ids`; they
/// are written out first.
pub(super) fn set_env_reference(
    doc: &mut DocumentMut,
    id: &str,
    var: &str,
    name: &str,
    default_ids: &[String],
) -> Result<(), SaveError> {
    let key = format!("{INSTANCES}.{id}.env.{var}");
    let mut tables = take_instances(doc);
    if tables.is_empty() {
        for d in default_ids {
            let mut t = Table::new();
            t.insert("id", toml_edit::value(d.as_str()));
            t.insert("driver", toml_edit::value(d.as_str()));
            tables.push(t);
        }
    }
    let result = match tables.iter_mut().find(|t| id_of(t) == Some(id)) {
        None => Err(SaveError::invalid(&key)),
        Some(t) => {
            if !t.contains_key("env") {
                t.insert("env", Item::Value(InlineTable::new().into()));
            }
            let mut reference = InlineTable::new();
            reference.insert("secret", name.into());
            match t.get_mut("env").and_then(Item::as_table_like_mut) {
                Some(env) => {
                    env.insert(var, Item::Value(reference.into()));
                    Ok(())
                }
                None => Err(SaveError::invalid(&key)),
            }
        }
    };
    put_instances(doc, tables);
    result
}

/// The names of all secrets that the document references. It walks the
/// whole document and takes every table that has a text `secret`: it may
/// find more names than the parser uses, never fewer, and a name that it
/// finds is never deleted.
pub(super) fn referenced_secrets(doc: &DocumentMut) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(doc.as_item(), &mut out);
    out
}

fn collect(item: &Item, out: &mut BTreeSet<String>) {
    match item {
        Item::Table(t) => collect_table(t, out),
        Item::ArrayOfTables(list) => list.iter().for_each(|t| collect_table(t, out)),
        Item::Value(v) => collect_value(v, out),
        Item::None => {}
    }
}

fn collect_table(t: &dyn TableLike, out: &mut BTreeSet<String>) {
    if let Some(name) = t.get("secret").and_then(Item::as_str) {
        out.insert(name.to_owned());
    }
    for (_, item) in t.iter() {
        collect(item, out);
    }
}

fn collect_value(v: &toml_edit::Value, out: &mut BTreeSet<String>) {
    match v {
        toml_edit::Value::InlineTable(t) => collect_table(t, out),
        toml_edit::Value::Array(list) => list.iter().for_each(|v| collect_value(v, out)),
        _ => {}
    }
}
