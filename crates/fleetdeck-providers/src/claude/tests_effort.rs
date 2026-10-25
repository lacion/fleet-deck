//! The reasoning effort from the `get_settings` control request (B18,
//! docs/v2/design/effort.md §4.3): the asks, the answers, the checkpoint, and
//! the redacted raw log.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ProviderKind;
use serde_json::{Value, json};

use super::effort::raw_view;
use super::tests::{config, emits, feed, input, lifecycle, writes};
use super::*;
use crate::checkpoint::AdapterCheckpoint;

const SECRET: &str = "fd-test-secret-0001";

fn init_frame() -> Value {
    json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-sonnet-5-5",
           "permissionMode":"default","claude_code_version":"2.1.308"})
}

/// The `get_settings` answer of the CLI: the settings with an `env` secret, and `applied`.
fn settings_answer(id: &str, effort: Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,
        "response":{
            "effective":{"env":{"ANTHROPIC_AUTH_TOKEN":SECRET}},
            "sources":[{"source":"userSettings","settings":{"env":{"ANTHROPIC_AUTH_TOKEN":SECRET}}}],
            "applied":{"model":"claude-sonnet-5-5","effort":effort,"advisor":null,"ultracode":false}}}})
}

/// The ids of the `get_settings` requests in `out`.
fn settings_ids(out: &[Output]) -> Vec<String> {
    writes(out)
        .iter()
        .filter(|w| w["request"]["subtype"] == "get_settings")
        .map(|w| w["request_id"].as_str().unwrap().to_owned())
        .collect()
}

/// A started core (`fd-usage-1` and `fd-settings-1` are open).
fn started() -> ClaudeCore {
    let mut core = ClaudeCore::new(config(false));
    let out = feed(&mut core, init_frame());
    assert_eq!(settings_ids(&out), ["fd-settings-1"], "{out:?}");
    core
}

/// A finished turn: the input starts, then its `result` arrives.
fn finish_turn(core: &mut ClaudeCore, n: u8) -> Vec<Output> {
    core.prompt(&input(n, "go")).unwrap();
    lifecycle(core, n, "started");
    feed(
        core,
        json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
               "total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}),
    )
}

fn effort_events(out: &[Output]) -> Vec<Event> {
    emits(out)
        .into_iter()
        .filter(|e| matches!(e, Event::EffortChanged { .. }))
        .collect()
}

fn changed(effort: Option<&str>) -> Event {
    Event::EffortChanged {
        effort: effort.map(str::to_owned),
    }
}

fn restored(core: &ClaudeCore) -> ClaudeCore {
    let text = core.checkpoint().unwrap().to_json().unwrap();
    let stored = AdapterCheckpoint::from_json(&text).unwrap();
    ClaudeCore::restore(config(false), &stored).unwrap()
}

#[test]
fn init_writes_one_get_settings_after_get_usage() {
    let mut core = ClaudeCore::new(config(false));
    let out = feed(&mut core, init_frame());
    assert_eq!(
        writes(&out),
        vec![
            json!({"type":"control_request","request_id":"fd-usage-1",
                   "request":{"subtype":"get_usage","skip_behaviors":true}}),
            json!({"type":"control_request","request_id":"fd-settings-1",
                   "request":{"subtype":"get_settings"}}),
        ]
    );
    // Claude repeats `init` for each turn: no second ask while one is open.
    assert!(settings_ids(&feed(&mut core, init_frame())).is_empty());
}

#[test]
fn an_answer_emits_effort_changed_once() {
    let mut core = started();
    let out = feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    assert_eq!(emits(&out), [changed(Some("high"))]);
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let out = feed(&mut core, settings_answer("fd-settings-2", json!("high")));
    assert!(effort_events(&out).is_empty(), "{out:?}");
}

#[test]
fn a_result_asks_again_and_a_change_is_emitted() {
    let mut core = started();
    feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let out = feed(&mut core, settings_answer("fd-settings-2", json!("medium")));
    assert_eq!(emits(&out), [changed(Some("medium"))]);
}

#[test]
fn a_null_effort_clears_a_shown_value() {
    let mut core = started();
    feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    finish_turn(&mut core, 1);
    let out = feed(&mut core, settings_answer("fd-settings-2", Value::Null));
    assert_eq!(emits(&out), [changed(None)]);
}

#[test]
fn an_error_answer_stops_the_asks_without_an_error_event() {
    let mut core = started();
    let err = json!({"type":"control_response","response":{"subtype":"error",
        "request_id":"fd-settings-1",
        "error":"Unsupported control request subtype: get_settings"}});
    assert!(emits(&feed(&mut core, err)).is_empty());
    assert!(settings_ids(&finish_turn(&mut core, 1)).is_empty());
    assert!(settings_ids(&finish_turn(&mut core, 2)).is_empty());
}

#[test]
fn an_answer_without_applied_stops_the_asks() {
    let mut core = started();
    let odd = json!({"type":"control_response","response":{"subtype":"success",
        "request_id":"fd-settings-1","response":{"effective":{"env":{"K":SECRET}}}}});
    assert!(emits(&feed(&mut core, odd)).is_empty());
    assert!(settings_ids(&finish_turn(&mut core, 1)).is_empty());
}

#[test]
fn an_invalid_effort_is_not_passed_on() {
    let mut core = started();
    let out = feed(
        &mut core,
        settings_answer("fd-settings-1", json!("High Effort")),
    );
    assert!(effort_events(&out).is_empty(), "{out:?}");
    finish_turn(&mut core, 1);
    feed(&mut core, settings_answer("fd-settings-2", json!("high")));
    finish_turn(&mut core, 2);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", json!("High Effort")),
    );
    assert_eq!(emits(&out), [changed(None)]);
}

#[test]
fn a_request_while_one_is_open_is_sent_after_its_answer() {
    let mut core = started();
    assert!(settings_ids(&finish_turn(&mut core, 1)).is_empty());
    let out = feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    assert_eq!(effort_events(&out), [changed(Some("high"))]);
    assert_eq!(settings_ids(&out), ["fd-settings-2"]);
}

#[test]
fn a_stale_request_is_replaced_after_60_seconds() {
    let mut core = started();
    core.settings_asked = Instant::now().checked_sub(Duration::from_secs(61));
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    // The late answer of the replaced request is ignored.
    let late = feed(&mut core, settings_answer("fd-settings-1", json!("low")));
    assert!(
        emits(&late).is_empty() && writes(&late).is_empty(),
        "{late:?}"
    );
    let out = feed(&mut core, settings_answer("fd-settings-2", json!("high")));
    assert_eq!(emits(&out), [changed(Some("high"))]);
}

#[test]
fn no_get_settings_before_the_session_started() {
    let mut core = ClaudeCore::new(config(false));
    let w = writes(&core.initialize().unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"current_permission_mode":"default","models":[
                   {"value":"default","displayName":"Default"}]}}}),
    );
    let w = writes(&core.set_model("default").unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let out = feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{}}}),
    );
    assert!(settings_ids(&out).is_empty(), "{out:?}");
}

#[test]
fn a_model_change_asks_for_the_effort() {
    let mut core = ClaudeCore::new(config(false));
    let w = writes(&core.initialize().unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"current_permission_mode":"default","models":[
                   {"value":"default","displayName":"Default"}]}}}),
    );
    let out = feed(&mut core, init_frame());
    assert_eq!(settings_ids(&out), ["fd-settings-1"]);
    feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    let w = writes(&core.set_model("default").unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let out = feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{}}}),
    );
    assert_eq!(
        emits(&out),
        [Event::ModelChanged {
            model: "default".into()
        }]
    );
    assert_eq!(settings_ids(&out), ["fd-settings-2"]);
}

#[test]
fn a_pre_effort_checkpoint_restores_and_asks_after_the_next_result() {
    let state: Value = serde_json::from_str(include_str!("checkpoint_pre_effort.json")).unwrap();
    let stored = AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: 1,
        state,
    };
    let mut core = ClaudeCore::restore(config(false), &stored).unwrap();
    assert_eq!(core.effort, None);
    assert_eq!(core.settings_open, None);
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-1"]);
}

#[test]
fn an_open_settings_request_survives_a_checkpoint() {
    let core = started();
    let mut core = restored(&core);
    let out = feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    assert_eq!(emits(&out), [changed(Some("high"))]);
}

#[test]
fn a_restored_open_request_counts_as_stale() {
    let core = started();
    let mut core = restored(&core);
    // No 60 s wait: the restored request has no ask time.
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let late = feed(&mut core, settings_answer("fd-settings-1", json!("low")));
    assert!(emits(&late).is_empty(), "{late:?}");
    let out = feed(&mut core, settings_answer("fd-settings-2", json!("high")));
    assert_eq!(emits(&out), [changed(Some("high"))]);
}

/// The host requests of the previous build (without the settings request).
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code, reason = "only the decoding is checked")]
enum PreEffortHostRequest {
    Initialize,
    Interrupt(Option<String>),
    SetMode(String),
    SetModel(Option<String>),
    Usage,
}

#[test]
fn the_checkpoint_stays_readable_by_the_previous_build() {
    let mut core = started();
    feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    assert_eq!(core.settings_open, Some(2));
    let checkpoint = core.checkpoint().unwrap();
    // (d) the version does not change.
    assert_eq!(checkpoint.version, 1);
    let mut state = checkpoint.state.clone();
    let requests = state["host_requests"].as_object().unwrap();
    // (a) the open settings request is not a host request.
    assert!(requests.keys().all(|k| !k.starts_with("fd-settings-")));
    // (b) the previous enum decodes every value.
    for (id, kind) in requests {
        serde_json::from_value::<PreEffortHostRequest>(kind.clone())
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    // (c) the state without the new keys restores.
    let map = state.as_object_mut().unwrap();
    for key in [
        "settings_seq",
        "settings_open",
        "settings_again",
        "settings_stopped",
        "effort",
    ] {
        assert!(map.remove(key).is_some(), "{key}");
    }
    let stripped = AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: 1,
        state,
    };
    assert!(ClaudeCore::restore(config(false), &stripped).is_ok());
}

#[test]
fn the_checkpoint_has_no_settings_text() {
    let mut core = started();
    feed(&mut core, settings_answer("fd-settings-1", json!("high")));
    let text = core.checkpoint().unwrap().to_json().unwrap();
    assert!(!text.contains("fd-test-secret"), "{text}");
    assert!(!text.contains("effective"), "{text}");
}

#[test]
fn raw_view_keeps_only_applied() {
    let frame = settings_answer("fd-settings-1", json!("high"));
    let view = raw_view(&frame);
    assert!(matches!(view, Cow::Owned(_)));
    assert_eq!(
        *view,
        json!({"type":"control_response","response":{"subtype":"success",
            "request_id":"fd-settings-1","response":{"applied":
                {"model":"claude-sonnet-5-5","effort":"high","advisor":null,"ultracode":false}}}})
    );
    assert!(!view.to_string().contains("fd-test-secret"));
    // An error answer keeps its error text.
    let err = json!({"type":"control_response","response":{"subtype":"error",
        "request_id":"fd-settings-2","error":"not available"}});
    let view = raw_view(&err);
    assert_eq!(view["response"]["error"], "not available");
    assert_eq!(view["response"]["response"], json!({"applied": null}));
    // Another answer and another frame stay as they are, borrowed.
    let usage = json!({"type":"control_response","response":{"subtype":"success",
        "request_id":"fd-usage-1","response":{"rate_limits_available":false}}});
    let assistant = json!({"type":"assistant","message":{"content":[]}});
    for frame in [&usage, &assistant] {
        let view = raw_view(frame);
        assert!(matches!(view, Cow::Borrowed(_)));
        assert_eq!(*view, *frame);
    }
}
