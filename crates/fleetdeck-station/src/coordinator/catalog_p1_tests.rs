//! MCP P1 catalog tests (`docs/v2/design/mcp-p1.md` §4.1 to §4.4): one
//! annotation row per tool, the two output schemas, the byte budget and the
//! per-tool `$defs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::annotations::{annotations, output_schema};
use super::bindings::ToolClass;
use super::desktop_tools::DESKTOP_TOOLS;
use super::fleet_tools::FLEET_TOOLS;
use super::tools::{self, TOOLS};

/// Every class that a binding can hold.
const EVERY_CLASS: [ToolClass; 15] = [
    ToolClass::Read,
    ToolClass::Mail,
    ToolClass::Drive,
    ToolClass::Spawn,
    ToolClass::Inspect,
    ToolClass::Manage,
    ToolClass::Answer,
    ToolClass::Desktop,
    ToolClass::Capture,
    ToolClass::Input,
    ToolClass::Recording,
    ToolClass::InputEnabled,
    ToolClass::ClickEnabled,
    ToolClass::InputAny,
    ToolClass::Own,
];

fn every_class() -> BTreeSet<ToolClass> {
    EVERY_CLASS.iter().copied().collect()
}

#[test]
fn every_tool_has_one_annotation_row_and_only_the_two_station_results_have_an_output_schema() {
    let all: Vec<&str> = TOOLS
        .iter()
        .chain(FLEET_TOOLS)
        .chain(DESKTOP_TOOLS)
        .map(|t| t.name)
        .collect();
    assert!(!all.is_empty());
    let unique: BTreeSet<&str> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len(), "a tool name is listed twice");
    for name in &all {
        let row = annotations(name).unwrap_or_else(|| panic!("{name}: no annotation row"));
        let keys: BTreeSet<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        if row["readOnlyHint"] == true {
            assert_eq!(
                keys,
                BTreeSet::from(["readOnlyHint", "openWorldHint"]),
                "{name}"
            );
        } else {
            assert_eq!(
                keys,
                BTreeSet::from([
                    "readOnlyHint",
                    "destructiveHint",
                    "idempotentHint",
                    "openWorldHint"
                ]),
                "{name}"
            );
        }
        assert!(row.as_object().unwrap().values().all(Value::is_boolean));
        let expect_output = matches!(*name, "publish_artifact" | "set_focus");
        assert_eq!(output_schema(name).is_some(), expect_output, "{name}");
    }
    assert!(annotations("no_such_tool").is_none());
    assert!(output_schema("no_such_tool").is_none());
    // Six rows of §4.1.
    assert_eq!(
        annotations("list_sessions").unwrap(),
        json!({"readOnlyHint": true, "openWorldHint": false})
    );
    assert_eq!(
        annotations("project_items").unwrap(),
        json!({"readOnlyHint": true, "openWorldHint": true})
    );
    assert_eq!(
        annotations("publish_artifact").unwrap(),
        json!({"readOnlyHint": false, "destructiveHint": false,
               "idempotentHint": false, "openWorldHint": false})
    );
    assert_eq!(
        annotations("project_fetch").unwrap(),
        json!({"readOnlyHint": false, "destructiveHint": false,
               "idempotentHint": true, "openWorldHint": true})
    );
    assert_eq!(
        annotations("land_session").unwrap(),
        json!({"readOnlyHint": false, "destructiveHint": true,
               "idempotentHint": false, "openWorldHint": true})
    );
    assert_eq!(
        annotations("type_text").unwrap(),
        json!({"readOnlyHint": false, "destructiveHint": true,
               "idempotentHint": false, "openWorldHint": true})
    );
    // The catalog carries the rows and the two output schemas.
    let catalog = tools::catalog(&every_class());
    for tool in catalog["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["annotations"], annotations(name).unwrap(), "{name}");
        match output_schema(name) {
            Some(schema) => assert_eq!(tool["outputSchema"], schema, "{name}"),
            None => assert!(tool.get("outputSchema").is_none(), "{name}"),
        }
    }
}

#[test]
fn the_full_catalog_stays_within_the_budget_and_is_deterministic() {
    let classes = every_class();
    let first = serde_json::to_vec(&tools::catalog(&classes)).unwrap();
    let second = serde_json::to_vec(&tools::catalog(&classes)).unwrap();
    eprintln!("catalog size: {} bytes", first.len());
    // Fleet Sync: station id or name descriptions.
    assert!(first.len() <= 26_240, "{} bytes", first.len());
    assert_eq!(first, second);
}

/// Every `$ref` string under `node`.
fn collect_refs(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get("$ref") {
                out.push(r.clone());
            }
            for value in map.values() {
                collect_refs(value, out);
            }
        }
        Value::Array(items) => {
            for value in items {
                collect_refs(value, out);
            }
        }
        _ => {}
    }
}

fn def_keys(refs: &[String]) -> BTreeSet<String> {
    refs.iter()
        .map(|r| r.strip_prefix("#/$defs/").expect(r).to_owned())
        .collect()
}

#[test]
fn every_desktop_schema_carries_only_the_defs_that_it_references() {
    for tool in DESKTOP_TOOLS {
        let name = tool.name;
        let schema = (tool.schema)();
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // The references of the schema outside `$defs`.
        let mut body = schema.clone();
        body.as_object_mut().unwrap().remove("$defs");
        let mut refs = Vec::new();
        collect_refs(&body, &mut refs);
        let mut reached = def_keys(&refs);
        // Follow the references of the reached defs until nothing is new.
        loop {
            let mut more = Vec::new();
            for key in &reached {
                let def = defs
                    .get(key)
                    .unwrap_or_else(|| panic!("{name}: #/$defs/{key} does not resolve"));
                collect_refs(def, &mut more);
            }
            let next: BTreeSet<String> = reached.union(&def_keys(&more)).cloned().collect();
            if next == reached {
                break;
            }
            reached = next;
        }
        let have: BTreeSet<String> = defs.keys().cloned().collect();
        assert_eq!(have, reached, "{name}: $defs and references differ");
    }
}
