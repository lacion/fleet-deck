//! Conformance of the recorded frames with the vendored app-server schema of
//! codex-cli 0.160.0 (DL-012). A small JSON-schema subset checker (no new
//! dependency): `$ref`, `type`, `enum`, `required`, `properties`,
//! `additionalProperties`, `items`, `anyOf`/`oneOf`/`allOf`.
//!
//! - Host writes (client requests, the `initialized` notification, replies to
//!   server requests) are checked strictly: every member that the core sends
//!   must be a declared property, so a typo in a parameter name fails.
//! - Provider frames (server notifications, server requests, and the responses
//!   to the core's client requests) are checked for required members and types,
//!   so a renamed field fails.
//! - Map values (`additionalProperties`, for example the answer map of
//!   `ToolRequestUserInputResponse`) are checked against their value schema.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};

const SCHEMA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/schema/0.160.0/codex_app_server_protocol.schemas.json"
);

struct Checker {
    root: Value,
    strict: bool,
}

impl Checker {
    fn resolve<'a>(&'a self, node: &'a Value) -> &'a Value {
        let mut node = node;
        while let Some(r) = node.get("$ref").and_then(Value::as_str) {
            let mut cur = &self.root;
            for part in r.trim_start_matches("#/").split('/') {
                cur = &cur[part];
            }
            node = cur;
        }
        node
    }

    fn type_ok(t: &str, v: &Value) -> bool {
        match t {
            "null" => v.is_null(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            _ => true,
        }
    }

    fn check(&self, v: &Value, node: &Value, path: &str, errs: &mut Vec<String>) {
        // The fixture scrubber replaced account values (DL-014); they match anything.
        if v == "<redacted>" {
            return;
        }
        let node = self.resolve(node);
        for key in ["anyOf", "oneOf"] {
            if let Some(alts) = node.get(key).and_then(Value::as_array) {
                let ok = alts.iter().any(|a| {
                    let mut e = Vec::new();
                    self.check(v, a, path, &mut e);
                    e.is_empty()
                });
                if !ok {
                    errs.push(format!("{path}: no {key} branch matches {}", short(v)));
                }
                return;
            }
        }
        if let Some(all) = node.get("allOf").and_then(Value::as_array) {
            for a in all {
                self.check(v, a, path, errs);
            }
        }
        match node.get("type") {
            Some(Value::String(t)) if !Self::type_ok(t, v) => {
                errs.push(format!("{path}: expected {t}, got {}", short(v)));
                return;
            }
            Some(Value::Array(ts))
                if !ts
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| Self::type_ok(t, v)) =>
            {
                errs.push(format!("{path}: expected {ts:?}, got {}", short(v)));
                return;
            }
            _ => {}
        }
        if let Some(e) = node.get("enum").and_then(Value::as_array)
            && !e.contains(v)
        {
            errs.push(format!("{path}: {} not in enum", short(v)));
        }
        let props = node.get("properties").and_then(Value::as_object);
        let extra = node.get("additionalProperties");
        if let Some(obj) = v.as_object()
            && (props.is_some() || extra.is_some())
        {
            for req in node
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(req) {
                    errs.push(format!("{path}: missing required `{req}`"));
                }
            }
            for (k, val) in obj {
                let at = format!("{path}.{k}");
                match (props.and_then(|p| p.get(k)), extra) {
                    (Some(p), _) => self.check(val, p, &at, errs),
                    (None, Some(Value::Bool(false))) => {
                        errs.push(format!("{path}: member `{k}` not allowed"));
                    }
                    (None, Some(schema)) if schema.is_object() => {
                        self.check(val, schema, &at, errs)
                    }
                    // An explicit open map (`config` of a thread request, B18) takes any member.
                    (None, Some(Value::Bool(true))) => {}
                    (None, _) if self.strict => {
                        errs.push(format!("{path}: undeclared member `{k}`"))
                    }
                    (None, _) => {}
                }
            }
        }
        if let (Some(arr), Some(items)) = (v.as_array(), node.get("items")) {
            for (i, item) in arr.iter().enumerate() {
                self.check(item, items, &format!("{path}[{i}]"), errs);
            }
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    s.chars().take(80).collect()
}

/// `method` → the schema node of the matching variant of `union`.
fn variants(root: &Value, union: &str) -> HashMap<String, Value> {
    root["definitions"][union]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| {
            let m = v["properties"]["method"]["enum"][0].as_str()?.to_owned();
            Some((m, v.clone()))
        })
        .collect()
}

/// The response type of a request variant: its `...Params` ref with `Response`.
fn response_ref(variant: &Value) -> Value {
    let params = variant["properties"]["params"]["$ref"].as_str().unwrap();
    let response = params.strip_suffix("Params").unwrap().to_owned() + "Response";
    serde_json::json!({ "$ref": response })
}

fn fixtures() -> Vec<Fixture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".jsonl"))
        .collect();
    files.sort();
    files.iter().map(|f| Fixture::load(f).unwrap()).collect()
}

fn root() -> Value {
    serde_json::from_str(&std::fs::read_to_string(SCHEMA).unwrap()).unwrap()
}

#[test]
fn host_writes_conform_strictly() {
    let root = root();
    let client = variants(&root, "ClientRequest");
    let client_notes = variants(&root, "ClientNotification");
    let server = variants(&root, "ServerRequest");
    let c = Checker {
        root: root.clone(),
        strict: true,
    };
    let mut errs = Vec::new();
    let mut checked = 0;
    for fx in fixtures() {
        // native id of each server request → its method, per process
        let mut asked: HashMap<String, String> = HashMap::new();
        for rec in &fx.records {
            match rec {
                Record::Spawn { .. } => asked.clear(),
                Record::Out { frame } => {
                    if let (Some(id), Some(m)) = (frame.get("id"), frame["method"].as_str()) {
                        asked.insert(id.to_string(), m.to_owned());
                    }
                }
                Record::In { frame } => {
                    let at = format!("{}: {}", fx.header.scenario, short(frame));
                    if let Some(m) = frame["method"].as_str() {
                        let node = client.get(m).or_else(|| client_notes.get(m));
                        match node {
                            Some(n) => c.check(frame, n, &at, &mut errs),
                            None => errs.push(format!("{at}: unknown client method {m}")),
                        }
                    } else {
                        let id = frame["id"].to_string();
                        let m = asked.get(&id).unwrap();
                        let node = response_ref(&server[m]);
                        c.check(&frame["result"], &node, &at, &mut errs);
                    }
                    checked += 1;
                }
                _ => {}
            }
        }
    }
    assert!(checked > 50, "only {checked} host writes");
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn provider_frames_conform() {
    let root = root();
    let notes = variants(&root, "ServerNotification");
    let server = variants(&root, "ServerRequest");
    let c = Checker {
        root: root.clone(),
        strict: false,
    };
    let mut errs = Vec::new();
    let mut checked = 0;
    for fx in fixtures() {
        for rec in &fx.records {
            let Record::Out { frame } = rec else { continue };
            let Some(m) = frame["method"].as_str() else {
                continue;
            };
            let at = format!("{}: {m}", fx.header.scenario);
            match notes.get(m).or_else(|| server.get(m)) {
                Some(n) => c.check(frame, n, &at, &mut errs),
                None => errs.push(format!("{at}: unknown server method")),
            }
            checked += 1;
        }
    }
    assert!(checked > 500, "only {checked} provider frames");
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn provider_responses_conform() {
    let root = root();
    let client = variants(&root, "ClientRequest");
    let c = Checker {
        root: root.clone(),
        strict: false,
    };
    let mut errs = Vec::new();
    let mut checked = HashMap::<String, usize>::new();
    for fx in fixtures() {
        // id of each client request → its method, per process
        let mut sent: HashMap<String, String> = HashMap::new();
        for rec in &fx.records {
            match rec {
                Record::Spawn { .. } => sent.clear(),
                Record::In { frame } => {
                    if let (Some(id), Some(m)) = (frame.get("id"), frame["method"].as_str()) {
                        sent.insert(id.to_string(), m.to_owned());
                    }
                }
                Record::Out { frame } if frame.get("method").is_none() => {
                    let id = frame["id"].to_string();
                    let Some(m) = sent.get(&id) else {
                        errs.push(format!(
                            "{}: response to unknown id {id}",
                            fx.header.scenario
                        ));
                        continue;
                    };
                    let at = format!("{}: response to {m}", fx.header.scenario);
                    if let Some(result) = frame.get("result") {
                        c.check(result, &response_ref(&client[m]), &at, &mut errs);
                    } else {
                        let e = &frame["error"];
                        if !(e["code"].is_i64() && e["message"].is_string()) {
                            errs.push(format!("{at}: malformed error {}", short(e)));
                        }
                    }
                    *checked.entry(m.clone()).or_default() += 1;
                }
                _ => {}
            }
        }
    }
    for m in [
        "initialize",
        "thread/start",
        "thread/resume",
        "turn/start",
        "turn/steer",
        "turn/interrupt",
    ] {
        assert!(
            checked.contains_key(m),
            "no recorded response to {m}: {checked:?}"
        );
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn the_checker_validates_answer_map_values() {
    let root = root();
    let c = Checker {
        root: root.clone(),
        strict: true,
    };
    let node = serde_json::json!({ "$ref": "#/definitions/ToolRequestUserInputResponse" });
    let check = |v: Value| {
        let mut errs = Vec::new();
        c.check(&v, &node, "reply", &mut errs);
        errs
    };
    assert!(check(serde_json::json!({"answers": {"q": {"answers": ["blue"]}}})).is_empty());
    for bad in [
        serde_json::json!({"answers": {"q": {"answers": "blue"}}}),
        serde_json::json!({"answers": {"q": {"answers": [1]}}}),
        serde_json::json!({"answers": {"q": "blue"}}),
        serde_json::json!({"answers": {"q": {}}}),
        serde_json::json!({"answers": ["blue"]}),
    ] {
        assert!(!check(bad.clone()).is_empty(), "accepted {bad}");
    }
}

#[test]
fn the_checker_rejects_a_misspelled_parameter() {
    let root = root();
    let client = variants(&root, "ClientRequest");
    let c = Checker {
        root: root.clone(),
        strict: true,
    };
    let bad = serde_json::json!({
        "id": "fd-1", "method": "turn/start",
        "params": {"threadId": "t", "input": [], "clientUserMesageId": "x"}
    });
    let mut errs = Vec::new();
    c.check(&bad, &client["turn/start"], "bad", &mut errs);
    assert!(
        errs.iter().any(|e| e.contains("clientUserMesageId")),
        "{errs:?}"
    );
}

#[test]
fn the_checker_rejects_a_response_without_a_required_member() {
    let root = root();
    let client = variants(&root, "ClientRequest");
    let c = Checker {
        root: root.clone(),
        strict: false,
    };
    let node = response_ref(&client["turn/start"]);
    let mut errs = Vec::new();
    c.check(
        &serde_json::json!({"turnId": "t"}),
        &node,
        "turn/start",
        &mut errs,
    );
    assert!(errs.iter().any(|e| e.contains("`turn`")), "{errs:?}");
}
