use serde_json::json;
use spike_canon::{CanonEvent, InputState, ToolStatus, TurnStatus};

use super::*;
use crate::frames::parse_value;

fn emits(out: &[Output]) -> Vec<CanonEvent> {
    out.iter()
        .filter_map(|o| match o {
            Output::Emit(e) => Some(e.clone()),
            Output::Write(_) => None,
        })
        .collect()
}

fn writes(out: &[Output]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| match o {
            Output::Write(v) => Some(v.clone()),
            Output::Emit(_) => None,
        })
        .collect()
}

fn feed(core: &mut ClaudeCore, v: Value) -> Vec<Output> {
    core.on_frame(parse_value(v).unwrap())
}

const IN1: &str = "00000000-0000-4000-8000-000000000001";
const IN2: &str = "00000000-0000-4000-8000-000000000002";

fn lifecycle(core: &mut ClaudeCore, id: &str, state: &str) -> Vec<CanonEvent> {
    emits(&feed(
        core,
        json!({"type":"command_lifecycle","command_uuid":id,"state":state,"uuid":"x","session_id":"S"}),
    ))
}

/// Writes a prompt and lets the CLI start it.
fn run_prompt(core: &mut ClaudeCore, text: &str, id: &str) {
    core.prompt(text, id);
    lifecycle(core, id, "queued");
    lifecycle(core, id, "started");
}

fn started_core() -> ClaudeCore {
    let mut core = ClaudeCore::new();
    feed(
        &mut core,
        json!({"type":"system","subtype":"init","session_id":"S","model":"m","permissionMode":"manual","claude_code_version":"2.1.289"}),
    );
    core
}

#[test]
fn cli_args_always_pass_a_native_mode() {
    let args = cli_args(&SessionSpec::default());
    let i = args.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(args[i + 1], "default");
    let args = cli_args(&SessionSpec {
        mode: Some("yolo".into()),
        resume: Some("abc".into()),
        fork: true,
        ..SessionSpec::default()
    });
    let i = args.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(
        args[i + 1],
        SAFEST_MODE,
        "unknown stored mode runs as the safest mode"
    );
    assert!(args.windows(2).any(|w| w[0] == "--resume" && w[1] == "abc"));
    assert!(args.contains(&"--fork-session".to_owned()));
}

#[test]
fn simple_turn_maps_to_canonical_events() {
    let mut core = started_core();
    let out = core.prompt("hi", IN1);
    assert_eq!(writes(&out)[0]["type"], "user");
    assert_eq!(writes(&out)[0]["uuid"], IN1);
    assert!(
        core.current_turn().is_none(),
        "a turn starts only when the CLI confirms it"
    );
    let ev = lifecycle(&mut core, IN1, "queued");
    assert_eq!(
        ev,
        vec![CanonEvent::InputUpdated {
            input_id: IN1.into(),
            turn_id: None,
            state: InputState::Received
        }]
    );
    assert!(core.current_turn().is_none(), "queued is not started");
    let ev = lifecycle(&mut core, IN1, "started");
    assert!(matches!(ev[0], CanonEvent::TurnStarted { .. }));
    let ev = emits(&feed(
        &mut core,
        json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"he"}}}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::MessageDelta {
            turn_id: "turn-1".into(),
            text: "he".into()
        }]
    );
    let ev = emits(&feed(
        &mut core,
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::MessageCompleted {
            turn_id: "turn-1".into(),
            text: "hello".into()
        }]
    );
    let ev = emits(&feed(
        &mut core,
        json!({"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":4}}),
    ));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Done,
            ..
        })
    ));
    assert!(core.current_turn().is_none());
    let ev = lifecycle(&mut core, IN1, "completed");
    assert_eq!(
        ev,
        vec![CanonEvent::InputUpdated {
            input_id: IN1.into(),
            turn_id: Some("turn-1".into()),
            state: InputState::Completed
        }]
    );
}

#[test]
fn unrelated_frames_never_confirm_a_pending_prompt() {
    let mut core = started_core();
    core.prompt("hi", IN1);
    // Status, ui frames, hooks, stream events, assistant text, and results are not
    // confirmations: none of them names the input.
    for frame in [
        json!({"type":"system","subtype":"status","status":"requesting"}),
        json!({"type":"system","subtype":"status","status":"compacting"}),
        json!({"type":"system","subtype":"ui_invalidate","event":"ui.render"}),
        json!({"type":"system","subtype":"hook_started"}),
        json!({"type":"system","subtype":"task_notification","status":"completed"}),
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}),
        json!({"type":"result","subtype":"success","is_error":false}),
    ] {
        let ev = emits(&feed(&mut core, frame.clone()));
        assert!(
            !ev.iter().any(|e| matches!(
                e,
                CanonEvent::TurnStarted { .. } | CanonEvent::TurnCompleted { .. }
            )),
            "{frame} must not start or end a turn: {ev:?}"
        );
        assert!(core.current_turn().is_none());
    }
    assert_eq!(core.pending_inputs(), 1);
    assert_eq!(core.input_state(IN1), None);
}

#[test]
fn provider_turn_does_not_confirm_a_pending_prompt() {
    let mut core = started_core();
    core.prompt("hi", IN1);
    // A background task finished: the CLI starts a turn on its own (trap 4).
    let ev = emits(&feed(
        &mut core,
        json!({"type":"stream_event","event":{"type":"message_start","message":{}}}),
    ));
    let CanonEvent::TurnStarted { provider_refs, .. } = &ev[0] else {
        panic!("{ev:?}")
    };
    assert_eq!(
        provider_refs.get("origin").and_then(Value::as_str),
        Some("provider")
    );
    assert_eq!(
        core.input_state(IN1),
        None,
        "the host input is still pending"
    );
    assert_eq!(core.pending_inputs(), 1);
    let ev = emits(&feed(
        &mut core,
        json!({"type":"result","subtype":"success","is_error":false}),
    ));
    assert!(matches!(ev.last(), Some(CanonEvent::TurnCompleted { .. })));
    assert!(!core.is_idle());
}

#[test]
fn only_the_matching_uuid_confirms() {
    let mut core = started_core();
    core.prompt("hi", IN1);
    // A replay echo with another uuid does not confirm (fallback path).
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","isReplay":true,"uuid":IN2,"message":{"role":"user","content":"hi"}}),
    ));
    assert!(ev.is_empty());
    // A user message with our uuid that is not a replay echo does not confirm either.
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","uuid":IN1,"message":{"role":"user","content":"hi"}}),
    ));
    assert!(ev.is_empty());
    assert!(core.current_turn().is_none());
    // The replay echo with our uuid confirms when the CLI sends no lifecycle frames.
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","isReplay":true,"uuid":IN1,"message":{"role":"user","content":"hi"}}),
    ));
    assert!(matches!(ev[0], CanonEvent::TurnStarted { .. }));
    // A lifecycle frame for an input of another client is ignored.
    assert!(lifecycle(&mut core, IN2, "started").is_empty());
    assert_eq!(core.current_turn(), Some("turn-1"));
}

#[test]
fn replay_echo_is_ignored_once_lifecycle_frames_exist() {
    let mut core = started_core();
    core.prompt("a", IN1);
    lifecycle(&mut core, IN1, "queued");
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","isReplay":true,"uuid":IN1,"message":{"role":"user","content":"a"}}),
    ));
    assert!(ev.is_empty(), "lifecycle is authoritative: {ev:?}");
    assert!(core.current_turn().is_none());
}

#[test]
fn unknown_lifecycle_state_is_counted_not_applied() {
    let mut core = started_core();
    core.prompt("a", IN1);
    assert!(lifecycle(&mut core, IN1, "teleported").is_empty());
    assert_eq!(core.unknown_frames(), 1);
    assert_eq!(core.input_state(IN1), None);
}

#[test]
fn input_started_during_a_turn_folds_into_it() {
    let mut core = started_core();
    run_prompt(&mut core, "a", IN1);
    core.prompt("b", IN2);
    let ev = lifecycle(&mut core, IN2, "started");
    assert_eq!(
        ev,
        vec![CanonEvent::InputUpdated {
            input_id: IN2.into(),
            turn_id: Some("turn-1".into()),
            state: InputState::Started
        }]
    );
}

#[test]
fn approval_answer_is_idempotent_and_uses_native_ids() {
    let mut core = started_core();
    run_prompt(&mut core, "run ls", IN1);
    feed(
        &mut core,
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}]}}),
    );
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_request","request_id":"N1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"tu1"}}),
    ));
    let CanonEvent::ApprovalRequested {
        request_id,
        node_id,
        options,
        ..
    } = &ev[0]
    else {
        panic!("{ev:?}")
    };
    assert_eq!(node_id.as_deref(), Some("node-2"));
    let ids: Vec<_> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["allow", "deny"]);
    let bad = core.answer(
        request_id,
        &Answer {
            option_id: "Yes".into(),
            updated_input: None,
            message: None,
        },
    );
    assert!(matches!(bad, Err(AdapterError::UnknownOption { .. })));
    let edited = json!({"command":"ls -a"});
    let (r1, out) = core
        .answer(
            request_id,
            &Answer {
                option_id: "allow".into(),
                updated_input: Some(edited.clone()),
                message: None,
            },
        )
        .unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["response"]["request_id"], "N1");
    assert_eq!(w[0]["response"]["response"]["updatedInput"], edited);
    let (r2, out2) = core
        .answer(
            request_id,
            &Answer {
                option_id: "deny".into(),
                updated_input: None,
                message: None,
            },
        )
        .unwrap();
    assert!(out2.is_empty(), "a second answer writes nothing");
    assert!(r2.duplicate);
    assert_eq!(r2.option_id, r1.option_id);
}

#[test]
fn denied_tool_completes_as_denied() {
    let mut core = started_core();
    run_prompt(&mut core, "x", IN1);
    feed(
        &mut core,
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Write","input":{"file_path":"/t/a"}}]}}),
    );
    feed(
        &mut core,
        json!({"type":"control_request","request_id":"N1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"/t/a"},"tool_use_id":"tu1"}}),
    );
    let req = core.open_requests()[0].clone();
    core.answer(
        &req,
        &Answer {
            option_id: "deny".into(),
            updated_input: None,
            message: None,
        },
    )
    .unwrap();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","is_error":true,"content":"denied"}]}}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::ToolCompleted {
            node_id: "node-2".into(),
            status: ToolStatus::Denied
        }]
    );
}

#[test]
fn unknown_control_request_fails_closed() {
    let mut core = started_core();
    let out = feed(
        &mut core,
        json!({"type":"control_request","request_id":"Z","request":{"subtype":"brand_new_prompt"}}),
    );
    let w = writes(&out);
    assert_eq!(w[0]["response"]["subtype"], "error");
    assert_eq!(w[0]["response"]["request_id"], "Z");
    assert!(core.open_requests().is_empty());
    feed(&mut core, json!({"type":"surprise"}));
    assert_eq!(core.unknown_frames(), 1);
}

#[test]
fn interrupt_needs_a_turn_and_marks_it_interrupted() {
    let mut core = started_core();
    assert_eq!(core.interrupt().unwrap_err(), AdapterError::NoTurn);
    run_prompt(&mut core, "long task", IN1);
    let (_, out) = core.interrupt().unwrap();
    assert_eq!(writes(&out)[0]["request"]["subtype"], "interrupt");
    let ev = emits(&feed(
        &mut core,
        json!({"type":"result","subtype":"error_during_execution","is_error":true}),
    ));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        })
    ));
}

#[test]
fn exit_expires_open_requests() {
    let mut core = started_core();
    run_prompt(&mut core, "x", IN1);
    feed(
        &mut core,
        json!({"type":"control_request","request_id":"N9","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}),
    );
    let ev = emits(&core.on_exit(Some(1)));
    assert!(
        matches!(&ev[0], CanonEvent::ApprovalExpired { reason, .. } if reason == "process_lost")
    );
    assert!(matches!(ev.last(), Some(CanonEvent::SessionEnded { .. })));
}

#[test]
fn set_mode_accepts_only_native_modes() {
    let mut core = started_core();
    assert!(matches!(
        core.set_mode("manual"),
        Err(AdapterError::UnsupportedMode(_))
    ));
    let (id, _) = core.set_mode("plan").unwrap();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::ModeChanged {
            mode: "plan".into()
        }]
    );
}

/// The CLI sends `command_lifecycle` frames before `system/init`; the canonical
/// stream still opens with `session.started`.
#[test]
fn events_before_init_follow_session_started() {
    let mut core = ClaudeCore::new();
    core.prompt("hi", IN1);
    assert!(lifecycle(&mut core, IN1, "queued").is_empty());
    assert!(lifecycle(&mut core, IN1, "started").is_empty());
    let ev = emits(&feed(
        &mut core,
        json!({"type":"system","subtype":"init","session_id":"S","model":"m","permissionMode":"default","claude_code_version":"2.1.289"}),
    ));
    let kinds: Vec<_> = ev.iter().map(CanonEvent::kind_name).collect();
    assert_eq!(
        kinds,
        [
            "session.started",
            "input.updated",
            "turn.started",
            "input.updated"
        ],
        "{ev:?}"
    );
}

/// A process that dies before `system/init` still reports its held events.
#[test]
fn held_events_are_flushed_on_exit() {
    let mut core = ClaudeCore::new();
    core.prompt("hi", IN1);
    lifecycle(&mut core, IN1, "started");
    let kinds: Vec<_> = emits(&core.on_exit(Some(1)))
        .iter()
        .map(CanonEvent::kind_name)
        .collect();
    assert_eq!(
        kinds,
        [
            "turn.started",
            "input.updated",
            "turn.completed",
            "session.ended"
        ]
    );
}

/// `allow_always` sends only allow rules, scoped to the session (DL-009): the
/// recorded suggestions also hold a `localSettings` destination, `addDirectories`,
/// and `setMode`.
#[test]
fn allow_always_sends_session_scoped_rules_only() {
    let mut core = started_core();
    run_prompt(&mut core, "x", IN1);
    feed(
        &mut core,
        json!({"type":"control_request","request_id":"N1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch a"},"tool_use_id":"tu1","permission_suggestions":[
            {"type":"addRules","behavior":"allow","destination":"localSettings","rules":[{"toolName":"Bash","ruleContent":"touch a"}]},
            {"type":"addDirectories","destination":"session","directories":["/w"]},
            {"type":"setMode","destination":"session","mode":"acceptEdits"}
        ]}}),
    );
    let req = core.open_requests()[0].clone();
    let (_, out) = core
        .answer(
            &req,
            &Answer {
                option_id: "allow_always".into(),
                updated_input: None,
                message: None,
            },
        )
        .unwrap();
    let body = &writes(&out)[0]["response"]["response"];
    assert_eq!(
        body["updatedPermissions"],
        json!([{"type":"addRules","behavior":"allow","destination":"session","rules":[{"toolName":"Bash","ruleContent":"touch a"}]}])
    );
}

/// Without an allow rule among the suggestions there is no `allow_always`.
#[test]
fn no_allow_rule_means_no_allow_always() {
    let mut core = started_core();
    run_prompt(&mut core, "x", IN1);
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_request","request_id":"N1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{},"permission_suggestions":[
            {"type":"setMode","destination":"session","mode":"acceptEdits"}
        ]}}),
    ));
    let CanonEvent::ApprovalRequested { options, .. } = &ev[0] else {
        panic!("{ev:?}")
    };
    let ids: Vec<_> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["allow", "deny"]);
}

/// Trap 6: a turn that ends (or a process that exits) leaves no tool call open.
#[test]
fn terminal_paths_close_open_tools() {
    let mut core = started_core();
    run_prompt(&mut core, "x", IN1);
    feed(
        &mut core,
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{}}]}}),
    );
    let ev = emits(&feed(
        &mut core,
        json!({"type":"result","subtype":"error_during_execution","is_error":true}),
    ));
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::ToolCompleted {
            status: ToolStatus::Failed,
            ..
        }
    )));
    // A late tool_result for the closed node emits nothing.
    let late = emits(&feed(
        &mut core,
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"x"}]}}),
    ));
    assert!(late.is_empty(), "{late:?}");

    run_prompt(&mut core, "y", IN2);
    feed(
        &mut core,
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{}}]}}),
    );
    let ev = emits(&core.on_exit(None));
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::ToolCompleted {
            status: ToolStatus::Failed,
            ..
        }
    )));
}
