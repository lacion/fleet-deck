//! A change of the reasoning effort of a Claude session (B18 Phase 2,
//! docs/v2/design/effort.md §9.2 item 6): the launch argument, the
//! `apply_flag_settings` write, the acceptance, the verifying read, the model
//! correction, and the checkpoint.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{EffortSetOutcome, ProviderKind};
use serde_json::{Value, json};

use super::tests::{config, emits, feed, input, lifecycle, writes};
use super::*;
use crate::adapter::ProviderError;
use crate::checkpoint::AdapterCheckpoint;

const OPUS: &str = "claude-opus-5-5";
const HAIKU: &str = "claude-haiku-5-5";

/// The `models` of an `initialize` answer.
fn models_json() -> Value {
    json!([
        {"value":"default","displayName":"Default","resolvedModel":OPUS,
         "supportsEffort":true,"supportedEffortLevels":["low","medium","high","xhigh","max"]},
        {"value":"sonnet","displayName":"Sonnet","resolvedModel":"claude-sonnet-5-5",
         "supportedEffortLevels":["low","medium","high"]},
        {"value":"haiku","displayName":"Haiku","resolvedModel":HAIKU,
         "supportedEffortLevels":["low","medium","high"]},
        {"value":"legacy","displayName":"Legacy","resolvedModel":"claude-legacy-1",
         "supportsEffort":false},
        {"value":"plain","displayName":"Plain","resolvedModel":"claude-plain-1"}
    ])
}

fn init_frame(model: &str) -> Value {
    json!({"type":"system","subtype":"init","session_id":"s-1","model":model,
           "permissionMode":"default","claude_code_version":"2.1.308"})
}

/// A `get_settings` answer whose `applied` has `model` and `effort`.
fn settings_answer(id: &str, model: &str, effort: Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,
        "response":{"effective":{"env":{"K":"fd-test-secret-0002"}},
                    "applied":{"model":model,"effort":effort}}}})
}

fn error_answer(id: &str, text: &str) -> Value {
    json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":text}})
}

/// The ack of an `apply_flag_settings` request: `{}`.
fn ack(core: &mut ClaudeCore, n: u64) -> Vec<Output> {
    feed(
        core,
        json!({"type":"control_response","response":{"subtype":"success",
               "request_id":format!("fd-effort-{n}"),"response":{}}}),
    )
}

/// A started core whose first read is answered (`medium`): no request is open.
fn started(model: &str) -> ClaudeCore {
    let mut core = ClaudeCore::new(config(false));
    let id = writes(&core.initialize().unwrap())[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"current_permission_mode":"default","models":models_json()}}}),
    );
    let out = feed(&mut core, init_frame(model));
    assert_eq!(settings_ids(&out), ["fd-settings-1"], "{out:?}");
    feed(
        &mut core,
        settings_answer("fd-settings-1", model, json!("medium")),
    );
    assert_eq!(core.settings_open, None);
    core
}

/// The ids of the `get_settings` requests in `out`.
fn settings_ids(out: &[Output]) -> Vec<String> {
    writes(out)
        .iter()
        .filter(|w| w["request"]["subtype"] == "get_settings")
        .map(|w| w["request_id"].as_str().unwrap().to_owned())
        .collect()
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

fn set_result(
    control_id: &str,
    requested: &str,
    effort: Option<&str>,
    outcome: EffortSetOutcome,
    message: Option<&str>,
) -> Event {
    Event::EffortSetResult {
        control_id: Some(control_id.to_owned()),
        requested: requested.to_owned(),
        effort: effort.map(str::to_owned),
        outcome,
        message: message.map(str::to_owned),
    }
}

fn changed(effort: Option<&str>) -> Event {
    Event::EffortChanged {
        effort: effort.map(str::to_owned),
    }
}

/// Only the `effort.set_result` events of `out`.
fn set_results(out: &[Output]) -> Vec<Event> {
    emits(out)
        .into_iter()
        .filter(|e| matches!(e, Event::EffortSetResult { .. }))
        .collect()
}

fn model_changes(out: &[Output]) -> Vec<Event> {
    emits(out)
        .into_iter()
        .filter(|e| matches!(e, Event::ModelChanged { .. }))
        .collect()
}

fn restored(core: &ClaudeCore) -> ClaudeCore {
    let text = core.checkpoint().unwrap().to_json().unwrap();
    let stored = AdapterCheckpoint::from_json(&text).unwrap();
    ClaudeCore::restore(config(false), &stored).unwrap()
}

#[test]
fn the_launch_passes_the_effort_after_the_model() {
    let caps = capabilities(false);
    let launch = Launch {
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        ..Launch::default()
    };
    let args = cli_args(&launch, &caps);
    let at = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[at + 2], "--effort");
    assert_eq!(args[at + 3], "high");
    assert!(
        !cli_args(&Launch::default(), &caps)
            .iter()
            .any(|a| a == "--effort")
    );
    assert!(caps.set_effort);
}

#[test]
fn the_initialize_answer_keeps_the_resolved_id_and_the_levels() {
    let core = started(OPUS);
    let by = |v: &str| {
        core.model_choices
            .iter()
            .find(|c| c.value == v)
            .unwrap()
            .clone()
    };
    let default = by("default");
    assert_eq!(default.resolved.as_deref(), Some(OPUS));
    assert_eq!(
        default.efforts,
        Some(
            ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    assert_eq!(default.default_effort, None);
    assert_eq!(by("legacy").efforts, Some(Vec::new()));
    assert_eq!(by("plain").efforts, None);
    assert_eq!(by("plain").resolved.as_deref(), Some("claude-plain-1"));
}

#[test]
fn an_invalid_advertised_level_is_dropped() {
    let mut core = ClaudeCore::new(config(false));
    let id = writes(&core.initialize().unwrap())[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"models":[{"value":"m","supportedEffortLevels":
                   ["low","High Effort","max",7]}]}}}),
    );
    assert_eq!(
        core.model_choices[0].efforts,
        Some(vec!["low".to_owned(), "max".to_owned()])
    );
}

#[test]
fn a_set_effort_writes_apply_flag_settings_and_emits_nothing() {
    let mut core = started(OPUS);
    let out = core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    assert_eq!(
        writes(&out),
        vec![json!({"type":"control_request","request_id":"fd-effort-1",
            "request":{"subtype":"apply_flag_settings","settings":{"effortLevel":"high"}}})]
    );
    assert!(emits(&out).is_empty(), "{out:?}");
    assert_eq!(core.effort_sets.len(), 1);
}

#[test]
fn the_ack_emits_accepted_and_writes_the_verifying_read() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    let out = ack(&mut core, 1);
    // Exactly the acceptance: no `effort.changed`, no other event.
    assert_eq!(
        emits(&out),
        [set_result(
            "eff-1",
            "high",
            None,
            EffortSetOutcome::Accepted,
            None
        )]
    );
    assert_eq!(settings_ids(&out), ["fd-settings-2"]);
    // The read answers `high`: the effort changes, then the set is verified.
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("high")),
    );
    assert_eq!(
        emits(&out),
        [
            changed(Some("high")),
            set_result(
                "eff-1",
                "high",
                Some("high"),
                EffortSetOutcome::Applied,
                None
            )
        ]
    );
    assert!(core.effort_sets.is_empty());
}

#[test]
fn an_ack_with_a_read_open_waits_and_the_new_read_verifies() {
    let mut core = started(OPUS);
    // A read is open (written after a result).
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    core.set_effort("low", Some("eff-1".to_owned())).unwrap();
    let out = ack(&mut core, 1);
    assert_eq!(
        emits(&out),
        [set_result(
            "eff-1",
            "low",
            None,
            EffortSetOutcome::Accepted,
            None
        )]
    );
    // No second read while the open one waits.
    assert!(settings_ids(&out).is_empty(), "{out:?}");
    // The open read was written before the change: it does not verify, and a
    // new read is written.
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("medium")),
    );
    assert!(set_results(&out).is_empty(), "{out:?}");
    assert_eq!(settings_ids(&out), ["fd-settings-3"]);
    // The answer of the new read verifies the change (after the Accepted).
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", OPUS, json!("low")),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "low",
            Some("low"),
            EffortSetOutcome::Applied,
            None
        )]
    );
}

#[test]
fn another_level_than_requested_is_adjusted() {
    let mut core = started(OPUS);
    core.set_effort("max", Some("eff-1".to_owned())).unwrap();
    ack(&mut core, 1);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("high")),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "max",
            Some("high"),
            EffortSetOutcome::Adjusted,
            None
        )]
    );
}

#[test]
fn a_null_level_is_adjusted_without_an_effort() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    ack(&mut core, 1);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, Value::Null),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "high",
            None,
            EffortSetOutcome::Adjusted,
            None
        )]
    );
}

#[test]
fn an_error_ack_emits_refused_with_the_message_and_no_accepted() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    let out = feed(&mut core, error_answer("fd-effort-1", "not allowed"));
    assert_eq!(
        emits(&out),
        [set_result(
            "eff-1",
            "high",
            None,
            EffortSetOutcome::Refused,
            Some("not allowed")
        )]
    );
    assert!(writes(&out).is_empty(), "{out:?}");
    assert!(core.effort_sets.is_empty());
    // A late second answer of the same request emits nothing.
    assert!(emits(&feed(&mut core, error_answer("fd-effort-1", "again"))).is_empty());
}

#[test]
fn an_ack_after_the_asks_stopped_emits_accepted_then_unverified() {
    let mut core = started(OPUS);
    core.settings_stopped = true;
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    let out = ack(&mut core, 1);
    assert_eq!(
        emits(&out),
        [
            set_result("eff-1", "high", None, EffortSetOutcome::Accepted, None),
            set_result("eff-1", "high", None, EffortSetOutcome::Unverified, None),
        ]
    );
    assert!(writes(&out).is_empty(), "{out:?}");
    assert!(core.effort_sets.is_empty());
}

#[test]
fn an_error_answer_of_the_verifying_read_emits_unverified() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    ack(&mut core, 1);
    let out = feed(&mut core, error_answer("fd-settings-2", "unsupported"));
    assert_eq!(
        emits(&out),
        [set_result(
            "eff-1",
            "high",
            None,
            EffortSetOutcome::Unverified,
            None
        )]
    );
    assert!(core.settings_stopped);
    assert!(core.effort_sets.is_empty());
}

#[test]
fn two_changes_are_verified_in_id_order() {
    let mut core = started(OPUS);
    core.set_effort("low", Some("eff-1".to_owned())).unwrap();
    core.set_effort("max", Some("eff-2".to_owned())).unwrap();
    let first = ack(&mut core, 1);
    assert_eq!(settings_ids(&first), ["fd-settings-2"]);
    // The second ack comes while the read of the first is open.
    let second = ack(&mut core, 2);
    assert!(settings_ids(&second).is_empty(), "{second:?}");
    // The read of the first ack does not verify the second change.
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("low")),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "low",
            Some("low"),
            EffortSetOutcome::Applied,
            None
        )]
    );
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", OPUS, json!("max")),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-2",
            "max",
            Some("max"),
            EffortSetOutcome::Applied,
            None
        )]
    );
}

#[test]
fn a_level_that_the_model_does_not_advertise_is_not_offered() {
    let mut core = started(OPUS);
    assert!(matches!(
        core.set_effort("ultra", None),
        Err(ProviderError::EffortNotOffered(e)) if e == "ultra"
    ));
    assert_eq!(core.effort_seq, 0);
    assert!(core.effort_sets.is_empty());
    // A model that advertises no effort setting offers no level.
    let mut legacy = started("claude-legacy-1");
    assert!(matches!(
        legacy.set_effort("low", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
    // A model without level information accepts the write.
    let mut plain = started("claude-plain-1");
    assert_eq!(writes(&plain.set_effort("low", None).unwrap()).len(), 1);
    // A model that is not advertised accepts the write too.
    let mut unknown = started("claude-other-9");
    assert_eq!(writes(&unknown.set_effort("low", None).unwrap()).len(), 1);
}

#[test]
fn the_current_model_is_matched_by_its_resolved_id() {
    let mut core = started(HAIKU);
    assert_eq!(writes(&core.set_effort("high", None).unwrap()).len(), 1);
    assert!(matches!(
        core.set_effort("max", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
    // A context suffix keeps the base model.
    let mut one_m = started("claude-opus-5-5[1m]");
    assert_eq!(writes(&one_m.set_effort("max", None).unwrap()).len(), 1);
}

#[test]
fn the_applied_model_decides_the_levels_after_a_model_change() {
    let mut core = started(OPUS);
    finish_turn(&mut core, 1);
    feed(
        &mut core,
        settings_answer("fd-settings-2", HAIKU, json!("medium")),
    );
    assert!(matches!(
        core.set_effort("max", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
}

#[test]
fn a_session_that_ended_refuses_a_change() {
    let mut core = started(OPUS);
    core.on_exit(Some(0));
    assert!(matches!(
        core.set_effort("low", None),
        Err(ProviderError::SessionEnded)
    ));
}

#[test]
fn a_model_command_turn_emits_model_changed_once() {
    let mut core = started(OPUS);
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", "claude-sonnet-5-5", json!("medium")),
    );
    assert_eq!(
        model_changes(&out),
        [Event::ModelChanged {
            model: "claude-sonnet-5-5".to_owned()
        }]
    );
    assert_eq!(core.model.as_deref(), Some("claude-sonnet-5-5"));
    finish_turn(&mut core, 2);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", "claude-sonnet-5-5", json!("medium")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
}

#[test]
fn a_set_model_answer_matches_its_resolved_settings_model() {
    let mut core = started(OPUS);
    let w = writes(&core.set_model("sonnet").unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let out = feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{}}}),
    );
    assert_eq!(
        model_changes(&out),
        [Event::ModelChanged {
            model: "sonnet".to_owned()
        }]
    );
    assert_eq!(settings_ids(&out), ["fd-settings-2"]);
    // The settings answer names the resolved id of `sonnet`: no second change.
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", "claude-sonnet-5-5", json!("low")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
    // `default` resolves to the opus id of the advertised choice.
    let w = writes(&core.set_model("default").unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{}}}),
    );
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", OPUS, json!("high")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
}

#[test]
fn a_settings_model_that_is_not_text_is_ignored() {
    let mut core = started(OPUS);
    finish_turn(&mut core, 1);
    let bad_models = [
        json!(""),
        json!("a\u{7}b"),
        json!("x".repeat(201)),
        json!(5),
    ];
    for (i, bad) in bad_models.into_iter().enumerate() {
        let id = format!("fd-settings-{}", core.settings_seq);
        let out = feed(
            &mut core,
            json!({"type":"control_response","response":{"subtype":"success","request_id":id,
                   "response":{"applied":{"model":bad,"effort":"medium"}}}}),
        );
        assert!(model_changes(&out).is_empty(), "{out:?}");
        assert_eq!(core.applied_model.as_deref(), Some(OPUS));
        finish_turn(&mut core, 10 + u8::try_from(i).unwrap());
    }
}

#[test]
fn an_open_effort_request_survives_a_checkpoint_in_both_phases() {
    // Phase 1: the `apply_flag_settings` answer is open.
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    let mut core = restored(&core);
    let out = ack(&mut core, 1);
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "high",
            None,
            EffortSetOutcome::Accepted,
            None
        )]
    );
    // Phase 2: the change is applied; the verifying read is open.
    assert_eq!(core.effort_sets[&1].verify_after, Some(1));
    let mut core = restored(&core);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("high")),
    );
    assert_eq!(
        set_results(&out),
        [set_result(
            "eff-1",
            "high",
            Some("high"),
            EffortSetOutcome::Applied,
            None
        )]
    );
}

#[test]
fn a_pre_effort_set_checkpoint_restores_without_a_reported_model() {
    let core = started(OPUS);
    let mut state = core.checkpoint().unwrap().state;
    let map = state.as_object_mut().unwrap();
    for key in [
        "model_choices",
        "applied_model",
        "reported_model",
        "effort_seq",
        "effort_sets",
    ] {
        assert!(map.remove(key).is_some(), "{key}");
    }
    let stored = AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: 1,
        state,
    };
    let mut core = ClaudeCore::restore(config(false), &stored).unwrap();
    assert_eq!(core.reported_model, None);
    assert!(core.model_choices.is_empty());
    // The first answer sets the reported model and emits no `model.changed`.
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", "claude-sonnet-5-5", json!("medium")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
    assert_eq!(core.reported_model.as_deref(), Some("claude-sonnet-5-5"));
}

/// The host requests of the previous build.
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
fn the_checkpoint_with_an_open_change_stays_readable_by_the_previous_build() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    ack(&mut core, 1);
    core.set_effort("low", Some("eff-2".to_owned())).unwrap();
    let checkpoint = core.checkpoint().unwrap();
    assert_eq!(checkpoint.version, 1);
    let mut state = checkpoint.state.clone();
    // The effort requests are not host requests: the previous enum decodes all.
    let requests = state["host_requests"].as_object().unwrap();
    assert!(requests.keys().all(|k| !k.starts_with("fd-effort-")));
    for (id, kind) in requests {
        serde_json::from_value::<PreEffortHostRequest>(kind.clone())
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    // The new keys are plain fields with defaults.
    let map = state.as_object_mut().unwrap();
    for key in [
        "model_choices",
        "applied_model",
        "reported_model",
        "effort_seq",
        "effort_sets",
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
fn the_checkpoint_has_no_settings_text_after_a_change() {
    let mut core = started(OPUS);
    core.set_effort("high", Some("eff-1".to_owned())).unwrap();
    ack(&mut core, 1);
    feed(
        &mut core,
        settings_answer("fd-settings-2", OPUS, json!("high")),
    );
    let text = core.checkpoint().unwrap().to_json().unwrap();
    assert!(!text.contains("fd-test-secret"), "{text}");
    assert!(!text.contains("effective"), "{text}");
}

#[test]
fn a_recorded_set_effort_command_applies_to_the_core() {
    let cmd: HostCmd =
        serde_json::from_value(json!({"op":"set_effort","effort":"high","control_id":"eff-1"}))
            .unwrap();
    let mut core = started(OPUS);
    let out = core.apply(&cmd).unwrap();
    assert_eq!(writes(&out)[0]["request_id"], "fd-effort-1");
    let cmd: HostCmd = serde_json::from_value(json!({"op":"set_effort","effort":"low"})).unwrap();
    assert!(matches!(
        cmd,
        HostCmd::SetEffort {
            control_id: None,
            ..
        }
    ));
}
