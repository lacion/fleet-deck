//! The provider traps of t3code-lessons §5, one test (or more) per trap.
//! Trap 8 (symlinked workspaces) is in `adapter_tests.rs`: the adapter
//! resolves the workspace before the core exists.

use fleetdeck_proto::event::{ExpiredReason, InputState, RunOrigin, ToolStatus, TurnStatus};
use fleetdeck_proto::{Event, InputId};
use serde_json::json;

use super::tests::{
    can_use_tool, config, emits, feed, init, input, kinds, lifecycle, result, running_core,
    tool_use, uuid, writes,
};
use super::*;
use crate::adapter::{AuthState, ProviderError};

/// Sends `initialize` and feeds its response with the advertised models.
fn initialized() -> ClaudeCore {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    let w = writes(&core.initialize().unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"current_permission_mode":"default","models":[
                   {"value":"default","displayName":"Default"},
                   {"value":"haiku","resolvedModel":"claude-haiku-4-5"}]}}}),
    );
    core
}

#[test]
fn trap_1_a_model_is_set_only_after_the_cli_advertised_it() {
    let mut fresh = ClaudeCore::new(config(false));
    assert!(matches!(
        fresh.set_model("haiku"),
        Err(ProviderError::NotReady("model"))
    ));
    let mut core = initialized();
    assert_eq!(
        core.models().unwrap(),
        ["claude-haiku-4-5", "default", "haiku"]
    );
    assert_eq!(core.mode(), Some("default"));
    assert!(matches!(
        core.set_model("gpt-5"),
        Err(ProviderError::InvalidModel(m)) if m == "gpt-5"
    ));
    let w = writes(&core.set_model("claude-haiku-4-5").unwrap());
    assert_eq!(w[0]["request"]["model"], "claude-haiku-4-5");
}

#[test]
fn trap_1_a_mode_is_set_only_when_the_session_offers_it() {
    let mut core = initialized();
    for bad in ["", "manual", "yolo", "bypassPermissions"] {
        assert!(
            matches!(core.set_mode(bad), Err(ProviderError::InvalidMode(_))),
            "{bad}"
        );
    }
}

#[test]
fn trap_2_default_is_a_sentinel_and_never_a_model_name() {
    let mut core = initialized();
    let w = writes(&core.set_model("default").unwrap());
    assert_eq!(w[0]["request"]["model"], serde_json::Value::Null);
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}}),
    ));
    assert_eq!(
        ev,
        [Event::ModelChanged {
            model: "default".to_owned()
        }]
    );
    for model in [None, Some("default")] {
        let launch = Launch {
            model: model.map(str::to_owned),
            ..Launch::default()
        };
        let args = cli_args(&launch, &capabilities(false));
        assert!(!args.iter().any(|a| a == "--model"), "{model:?}");
    }
}

#[test]
fn trap_3_an_interrupt_before_the_turn_started_cancels_the_input() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    core.prompt(&input(1, "x")).unwrap();
    lifecycle(&mut core, 1, "queued");
    let w = writes(&core.interrupt(None).unwrap());
    assert_eq!(w[0]["request"]["subtype"], "interrupt");
    let ev = lifecycle(&mut core, 1, "cancelled");
    assert!(matches!(
        &ev[0],
        Event::InputUpdated {
            turn_id: None,
            state: InputState::Cancelled,
            ..
        }
    ));
    assert!(core.current_turn().is_none());
    assert!(core.is_idle());
    // The next turn is not marked interrupted by the old request.
    core.prompt(&input(2, "y")).unwrap();
    lifecycle(&mut core, 2, "started");
    let ev = result(&mut core, "success", "completed");
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        }
    )));
}

#[test]
fn trap_3_an_interrupt_with_nothing_to_stop_is_an_error() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    assert!(matches!(core.interrupt(None), Err(ProviderError::NoTurn)));
}

#[test]
fn trap_4_provider_work_after_the_turn_is_a_provider_turn() {
    let mut core = running_core();
    result(&mut core, "success", "completed");
    lifecycle(&mut core, 1, "completed");
    core.prompt(&input(2, "waiting")).unwrap();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"assistant","message":{"role":"assistant",
               "content":[{"type":"text","text":"the background task finished"}]}}),
    ));
    assert_eq!(
        kinds(&ev),
        ["run.started", "turn.started", "message.completed"]
    );
    // A provider turn has no input behind it, so no `input_id` (DL-179).
    assert!(matches!(
        &ev[0],
        Event::RunStarted {
            origin: RunOrigin::Provider,
            input_id: None,
            ..
        }
    ));
    assert!(
        matches!(&ev[1], Event::TurnStarted { provider_refs, .. } if provider_refs["origin"] == "provider")
    );
    // The provider turn never confirms the waiting host input.
    assert_eq!(core.input_state(&InputId::new(uuid(2))), None);
    assert_eq!(core.pending_inputs(), 1);
}

#[test]
fn trap_5_a_subagent_that_outlives_its_run_starts_no_turn() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_task", "Task", json!({"prompt":"look"}));
    result(&mut core, "success", "completed");
    let ev = emits(&feed(
        &mut core,
        json!({"type":"assistant","parent_tool_use_id":"toolu_task","message":{"role":"assistant",
               "content":[{"type":"text","text":"late child text"},
                          {"type":"tool_use","id":"toolu_child","name":"Read","input":{"file_path":"/w/a"}}]}}),
    ));
    assert_eq!(kinds(&ev), ["tool.started"], "child text needs a turn");
    assert!(core.current_turn().is_none());
    assert!(core.current_run().is_none());
}

#[test]
fn trap_6_a_turn_end_closes_requests_and_tools() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_1", "Bash", json!({"command":"ls"}));
    can_use_tool(&mut core, "native-1", "toolu_1");
    let ev = result(&mut core, "error_during_execution", "aborted_tools");
    assert_eq!(
        kinds(&ev),
        [
            "approval.expired",
            "tool.completed",
            "usage.updated",
            "turn.completed",
            "run.completed"
        ]
    );
    assert!(matches!(
        &ev[0],
        Event::ApprovalExpired {
            reason: ExpiredReason::TurnEnded,
            ..
        }
    ));
    assert!(matches!(
        &ev[1],
        Event::ToolCompleted {
            status: ToolStatus::Failed,
            ..
        }
    ));
    assert!(matches!(
        &ev[3],
        Event::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        }
    ));
    assert!(core.open_requests().is_empty());
}

#[test]
fn trap_6_a_process_exit_closes_everything_once() {
    let mut core = running_core();
    core.prompt(&input(2, "queued behind")).unwrap();
    tool_use(&mut core, "toolu_1", "Bash", json!({"command":"ls"}));
    can_use_tool(&mut core, "native-1", "toolu_1");
    feed(
        &mut core,
        json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash"}]}),
    );
    let ev = emits(&core.on_exit(Some(137)));
    assert_eq!(
        kinds(&ev),
        [
            "approval.expired",
            "tool.completed",
            "input.updated",
            "input.updated",
            "turn.completed",
            "run.completed",
            "session.ended"
        ]
    );
    assert!(matches!(
        &ev[0],
        Event::ApprovalExpired {
            reason: ExpiredReason::ProcessLost,
            ..
        }
    ));
    assert!(matches!(
        &ev[5],
        Event::RunCompleted {
            status: TurnStatus::Failed,
            background_pending: true,
            ..
        }
    ));
    assert!(matches!(
        &ev[6],
        Event::SessionEnded {
            exit_code: Some(137),
            ..
        }
    ));
    assert!(core.has_ended());
    assert!(core.background().is_empty());
    assert!(core.on_exit(Some(0)).is_empty(), "idempotent");
    assert!(feed(&mut core, json!({"type":"system","subtype":"init"})).is_empty());
    assert!(matches!(
        core.prompt(&input(3, "late")),
        Err(ProviderError::SessionEnded)
    ));
}

#[test]
fn trap_6_an_exit_before_init_still_reports_the_held_events() {
    let mut core = ClaudeCore::new(config(false));
    core.prompt(&input(1, "x")).unwrap();
    lifecycle(&mut core, 1, "queued");
    let ev = emits(&core.on_exit(Some(1)));
    assert_eq!(
        kinds(&ev),
        ["input.updated", "input.updated", "session.ended"]
    );
    assert!(matches!(
        &ev[1],
        Event::InputUpdated {
            state: InputState::Discarded,
            ..
        }
    ));
}

#[test]
fn trap_7_a_rejected_rate_limit_is_a_usage_limit_error_with_the_reset_time() {
    let mut core = running_core();
    let ev = emits(&feed(
        &mut core,
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected",
               "rateLimitType":"five_hour","resetsAt":1_767_240_000_u64,"utilization":1.0}}),
    ));
    assert_eq!(kinds(&ev), ["usage.updated", "error"]);
    let Event::UsageUpdated { rate_limits, .. } = &ev[0] else {
        panic!()
    };
    assert_eq!(rate_limits[0].scope, "five_hour");
    assert_eq!(rate_limits[0].used_percent, Some(100.0));
    assert_eq!(rate_limits[0].resets_at, Some(1_767_240_000_000));
    assert!(matches!(
        &ev[1],
        Event::Error { code, retry_at: Some(1_767_240_000_000), .. } if code == "usage_limit"
    ));
    let warn = emits(&feed(
        &mut core,
        json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","utilization":0.8}}),
    ));
    assert_eq!(kinds(&warn), ["usage.updated"]);
}

#[test]
fn trap_7_a_max_turns_or_budget_result_is_a_usage_limit() {
    for subtype in ["error_max_turns", "error_max_budget_usd"] {
        let mut core = running_core();
        let ev = result(&mut core, subtype, "max_turns");
        assert!(matches!(&ev[0], Event::Error { code, .. } if code == "usage_limit"));
        assert!(ev.iter().any(|e| matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Failed,
                ..
            }
        )));
    }
}

#[test]
fn trap_9_an_unknown_sign_in_state_is_unknown() {
    assert_eq!(
        parse_auth_status(true, r#"{"loggedIn":true,"authMethod":"claude.ai"}"#),
        AuthState::SignedIn {
            method: Some("claude.ai".to_owned())
        }
    );
    assert_eq!(
        parse_auth_status(true, r#"{"loggedIn":false}"#),
        AuthState::SignedOut
    );
    for (ok, out) in [
        (false, r#"{"loggedIn":true}"#),
        (true, "Logged in as someone"),
        (true, r#"{"user":"x"}"#),
        (true, r#"{"loggedIn":"yes"}"#),
        (true, ""),
    ] {
        assert_eq!(parse_auth_status(ok, out), AuthState::Unknown, "{out}");
    }
}

#[test]
fn trap_10_an_interrupt_is_a_request_and_the_turn_ends_with_its_result() {
    let mut core = running_core();
    let out = core.interrupt(Some("effect-7".to_owned())).unwrap();
    assert!(
        emits(&out).is_empty(),
        "no terminal event before the CLI confirms"
    );
    assert!(core.current_turn().is_some());
    let w = writes(&out);
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    let reply = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{"still_queued":[]}}}),
    ));
    // The matching success answer is the interrupt's evidence; the turn
    // still runs until its own result.
    assert_eq!(
        reply,
        [Event::InterruptResult {
            control_id: Some("effect-7".to_owned()),
            accepted: true,
            message: None,
        }]
    );
    assert!(core.current_turn().is_some());
    let ev = result(&mut core, "error_during_execution", "aborted_streaming");
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        }
    )));
    let ev = lifecycle(&mut core, 1, "cancelled");
    assert!(matches!(
        &ev[0],
        Event::InputUpdated {
            state: InputState::Cancelled,
            ..
        }
    ));
}

#[test]
fn pending_background_work_after_the_turn_is_not_idle() {
    let mut core = running_core();
    feed(
        &mut core,
        json!({"type":"system","subtype":"background_tasks_changed",
               "tasks":[{"task_id":"b1","task_type":"local_bash"}]}),
    );
    let ev = result(&mut core, "success", "completed");
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::RunCompleted {
            background_pending: true,
            ..
        }
    )));
    lifecycle(&mut core, 1, "completed");
    assert!(core.current_turn().is_none());
    assert_eq!(core.pending_inputs(), 0);
    assert!(core.is_quiet());
    assert!(!core.is_idle(), "background work pending (PLAN §6.3, L7)");
    // Nothing runs that an interrupt can stop.
    assert!(matches!(core.interrupt(None), Err(ProviderError::NoTurn)));
    // The task ends: the session is idle again.
    let ev = emits(&feed(
        &mut core,
        json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
    ));
    assert_eq!(kinds(&ev), ["background.updated"]);
    assert!(core.is_idle());
    // The provider turn that reports the result makes it busy, then idle.
    feed(
        &mut core,
        json!({"type":"assistant","message":{"role":"assistant",
               "content":[{"type":"text","text":"the task finished"}]}}),
    );
    assert!(!core.is_idle());
    result(&mut core, "success", "completed");
    assert!(core.is_idle());
}

#[test]
fn a_refused_interrupt_reports_its_control_id_and_the_turn_ends_normally() {
    let mut core = running_core();
    let w = writes(&core.interrupt(Some("effect-8".to_owned())).unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    // A response to another control id is no evidence for this interrupt.
    let other = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"success",
               "request_id":"fd-999","response":{}}}),
    ));
    assert!(other.is_empty());
    let reply = emits(&feed(
        &mut core,
        json!({"type":"control_response","response":{"subtype":"error","request_id":id,
               "error":"no turn to interrupt"}}),
    ));
    assert_eq!(
        reply,
        [Event::InterruptResult {
            control_id: Some("effect-8".to_owned()),
            accepted: false,
            message: Some("no turn to interrupt".to_owned()),
        }]
    );
    let ev = result(&mut core, "success", "completed");
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        }
    )));
}
