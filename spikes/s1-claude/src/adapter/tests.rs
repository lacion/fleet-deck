use serde_json::json;
use spike_canon::{CanonEvent, ToolStatus, TurnStatus};

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
    assert_eq!(args[i + 1], "manual");
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
    let out = core.prompt("hi");
    assert_eq!(writes(&out)[0]["type"], "user");
    assert!(
        core.current_turn().is_none(),
        "a turn starts only when the CLI confirms it"
    );
    let ev = emits(&feed(
        &mut core,
        json!({"type":"user","message":{"role":"user","content":"hi"}}),
    ));
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
}

#[test]
fn approval_answer_is_idempotent_and_uses_native_ids() {
    let mut core = started_core();
    core.prompt("run ls");
    feed(
        &mut core,
        json!({"type":"system","subtype":"status","status":"requesting"}),
    );
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
    core.prompt("x");
    feed(
        &mut core,
        json!({"type":"system","subtype":"status","status":"requesting"}),
    );
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
    core.prompt("long task");
    feed(
        &mut core,
        json!({"type":"system","subtype":"status","status":"requesting"}),
    );
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
    core.prompt("x");
    feed(
        &mut core,
        json!({"type":"system","subtype":"status","status":"requesting"}),
    );
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
        core.set_mode("default"),
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
