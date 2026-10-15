//! The `NativeIdentity` adapter output of the Claude and Codex cores
//! (DL-194 T3): a root identity statement is an output beside the events,
//! so the durable driver can pass it to the output transaction in order.

#![allow(
    clippy::unwrap_used,
    reason = "a test harness: a panic is the failure report"
)]

use fleetdeck_proto::Event;
use fleetdeck_providers::claude::frames::parse_value as claude_frame;
use fleetdeck_providers::claude::{self, ClaudeCore};
use fleetdeck_providers::codex::rpc::parse_value as codex_frame;
use fleetdeck_providers::codex::{self, ClientInfo, CodexCore, Output as CodexOutput, SessionSpec};
use serde_json::{Value, json};

fn claude_core() -> ClaudeCore {
    ClaudeCore::new(claude::CoreConfig {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        capabilities: claude::capabilities(false),
        resumed: None,
    })
}

/// A compact view of the outputs: `identity:<id>` or the event kind.
fn claude_view(out: &[claude::Output]) -> Vec<String> {
    out.iter()
        .filter_map(|o| match o {
            claude::Output::NativeIdentity { native_id } => Some(format!("identity:{native_id}")),
            claude::Output::Emit(Event::SessionStarted { .. }) => Some("session.started".into()),
            claude::Output::Emit(_) | claude::Output::Write(_) => None,
        })
        .collect()
}

fn init(core: &mut ClaudeCore, body: Value) -> Vec<String> {
    let mut frame = json!({"type": "system", "subtype": "init", "model": "m",
        "permissionMode": "default", "claude_code_version": "2.1.289"});
    for (k, v) in body.as_object().unwrap() {
        frame[k] = v.clone();
    }
    claude_view(&core.on_frame(claude_frame(frame).unwrap()))
}

#[test]
fn the_first_claude_init_states_its_identity_before_session_started() {
    let mut core = claude_core();
    assert_eq!(
        init(&mut core, json!({"session_id": "s-1"})),
        ["identity:s-1", "session.started"]
    );
}

#[test]
fn a_second_root_claude_init_states_the_new_identity_only() {
    let mut core = claude_core();
    init(&mut core, json!({"session_id": "s-1"}));
    assert_eq!(
        init(&mut core, json!({"session_id": "s-2"})),
        ["identity:s-2"]
    );
}

#[test]
fn a_child_claude_init_states_nothing() {
    let mut core = claude_core();
    init(&mut core, json!({"session_id": "s-1"}));
    assert!(
        init(
            &mut core,
            json!({"session_id": "child-1", "parent_tool_use_id": "toolu_1"})
        )
        .is_empty()
    );
}

#[test]
fn a_claude_init_without_a_session_id_states_nothing_but_the_start() {
    let mut core = claude_core();
    assert_eq!(init(&mut core, json!({})), ["session.started"]);
}

fn codex_core() -> CodexCore {
    CodexCore::new(codex::CoreConfig {
        provider_instance: "codex".into(),
        cwd: "/w".to_owned(),
        capabilities: codex::capabilities(false),
        resumed: None,
        client: ClientInfo::fleetdeck(),
        request_models: false,
    })
}

fn codex_view(out: &[CodexOutput]) -> Vec<String> {
    out.iter()
        .filter_map(|o| match o {
            CodexOutput::NativeIdentity { native_id } => Some(format!("identity:{native_id}")),
            CodexOutput::Emit(Event::SessionStarted { .. }) => Some("session.started".into()),
            CodexOutput::Emit(_) | CodexOutput::Write(_) => None,
        })
        .collect()
}

fn answer_last(core: &mut CodexCore, out: &[CodexOutput], result: Value) -> Vec<CodexOutput> {
    let id = out
        .iter()
        .rev()
        .find_map(|o| match o {
            CodexOutput::Write(v) => Some(v["id"].clone()),
            _ => None,
        })
        .unwrap();
    core.on_frame(codex_frame(json!({"id": id, "result": result})))
}

/// A core after `initialize` and the thread-open answer; returns the
/// outputs of the thread-open answer.
fn opened_codex_core() -> (CodexCore, Vec<CodexOutput>) {
    let mut core = codex_core();
    let out = core.initialize().unwrap();
    answer_last(
        &mut core,
        &out,
        json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
    );
    let out = core.open_thread(&SessionSpec::default()).unwrap();
    let answer = answer_last(
        &mut core,
        &out,
        json!({"thread": {"id": "t-1"}, "model": "m"}),
    );
    (core, answer)
}

fn started(core: &mut CodexCore, thread: Value) -> Vec<String> {
    codex_view(&core.on_frame(codex_frame(
        json!({"method": "thread/started", "params": {"thread": thread}}),
    )))
}

#[test]
fn the_codex_thread_open_answer_states_the_identity_with_session_started() {
    let (_, answer) = opened_codex_core();
    assert_eq!(codex_view(&answer), ["session.started", "identity:t-1"]);
}

#[test]
fn a_root_thread_started_states_its_identity() {
    let (mut core, _) = opened_codex_core();
    assert_eq!(
        started(
            &mut core,
            json!({"id": "t-2", "source": "appServer", "parentThreadId": null})
        ),
        ["identity:t-2"]
    );
}

#[test]
fn a_thread_started_with_a_parent_states_nothing() {
    let (mut core, _) = opened_codex_core();
    assert!(started(&mut core, json!({"id": "t-3", "parentThreadId": "t-1"})).is_empty());
}

#[test]
fn a_thread_started_with_a_sub_agent_source_states_nothing() {
    let (mut core, _) = opened_codex_core();
    let source = json!({"subAgent": {"thread_spawn": {"depth": 1, "parent_thread_id": "t-1"}}});
    assert!(started(&mut core, json!({"id": "t-4", "source": source})).is_empty());
}

#[test]
fn a_turn_started_states_no_identity() {
    let (mut core, _) = opened_codex_core();
    let out = core.on_frame(codex_frame(json!({"method": "turn/started",
        "params": {"threadId": "t-1", "turn": {"id": "turn-1"}}})));
    assert!(codex_view(&out).iter().all(|v| !v.starts_with("identity:")));
}
