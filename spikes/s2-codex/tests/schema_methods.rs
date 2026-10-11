//! The vendored app-server schema (codex-cli 0.160.0, `generate-json-schema
//! --experimental`) is the protocol source of truth (DL-012). Every method name
//! that the core sends or matches must exist in it, so a typo or a renamed
//! method fails offline.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use s2_codex::adapter::{
    CLIENT_REQUESTS, DECLINED_REQUESTS, HANDLED_NOTIFICATIONS, HANDLED_REQUESTS,
    IGNORED_NOTIFICATIONS,
};
use serde_json::Value;

const SCHEMA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/schema/0.160.0/codex_app_server_protocol.schemas.json"
);

fn methods(schema: &Value, union: &str) -> BTreeSet<String> {
    schema["definitions"][union]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| {
            v["properties"]["method"]["enum"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect()
}

fn schema() -> Value {
    serde_json::from_str(&std::fs::read_to_string(SCHEMA).unwrap()).unwrap()
}

fn assert_all_in(names: &[&str], set: &BTreeSet<String>, union: &str) {
    let missing: Vec<_> = names.iter().filter(|n| !set.contains(**n)).collect();
    assert!(missing.is_empty(), "not in {union}: {missing:?}");
}

#[test]
fn every_client_request_exists() {
    let s = schema();
    assert_all_in(
        CLIENT_REQUESTS,
        &methods(&s, "ClientRequest"),
        "ClientRequest",
    );
    assert_all_in(
        &["initialized"],
        &methods(&s, "ClientNotification"),
        "ClientNotification",
    );
}

#[test]
fn every_server_request_the_core_names_exists() {
    let set = methods(&schema(), "ServerRequest");
    assert_all_in(HANDLED_REQUESTS, &set, "ServerRequest");
    assert_all_in(DECLINED_REQUESTS, &set, "ServerRequest");
}

#[test]
fn every_notification_the_core_names_exists() {
    let set = methods(&schema(), "ServerNotification");
    assert_all_in(HANDLED_NOTIFICATIONS, &set, "ServerNotification");
    assert_all_in(IGNORED_NOTIFICATIONS, &set, "ServerNotification");
}

#[test]
fn approval_decisions_match_the_schema() {
    let s = schema();
    let defs = &s["definitions"]["v2"]["definitions"];
    let decision = if defs["CommandExecutionApprovalDecision"].is_object() {
        &defs["CommandExecutionApprovalDecision"]
    } else {
        &s["definitions"]["CommandExecutionApprovalDecision"]
    };
    let text = decision.to_string();
    for id in [
        "accept",
        "acceptForSession",
        "acceptWithExecpolicyAmendment",
        "decline",
        "cancel",
    ] {
        assert!(
            text.contains(&format!("\"{id}\"")),
            "{id} not in CommandExecutionApprovalDecision"
        );
    }
}

/// L4 message channel: `agentMessage` items can carry `delivery: "async"` and
/// `questions`. The core reads these fields (`on_async_questions`); 0.160.0
/// gates the tool behind an under-development feature, so this channel has a
/// unit test and this schema check, not a live fixture (ADR-002 §2.5).
#[test]
fn async_question_fields_exist_in_the_schema() {
    let text = std::fs::read_to_string(SCHEMA).unwrap();
    for name in [
        "AsyncUserInputQuestion",
        "AgentMessageDelivery",
        "\"questions\"",
        "\"delivery\"",
    ] {
        assert!(text.contains(name), "{name} not in the schema");
    }
}
