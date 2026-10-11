//! Regression tests from the round 6 and 7 reviews and from the live recordings
//! (split from `tests.rs` to keep both modules under 800 lines).

use serde_json::json;
use spike_canon::{CanonEvent, TurnStatus};

use super::tests::*;
use super::*;

#[test]
fn answers_stay_idempotent_after_resolution_and_turn_end() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ans = |o: &str| Answer {
        option_id: o.into(),
        updated_input: None,
        message: None,
    };
    let CanonEvent::ApprovalRequested { request_id: r1, .. } =
        command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    let (first, _) = core.answer(&r1, &ans("accept")).unwrap();
    notify(
        &mut core,
        "serverRequest/resolved",
        json!({"threadId": T, "requestId": 0}),
    );
    let (again, out) = core.answer(&r1, &ans("cancel")).unwrap();
    assert!(
        out.is_empty(),
        "no second reply after serverRequest/resolved"
    );
    assert!(again.duplicate);
    assert_eq!(again.option_id, first.option_id);

    let CanonEvent::ApprovalRequested { request_id: r2, .. } =
        command_approval(&mut core, json!(1), "call_2")
    else {
        panic!()
    };
    core.answer(&r2, &ans("accept")).unwrap();
    let CanonEvent::ApprovalRequested { request_id: r3, .. } =
        command_approval(&mut core, json!(2), "call_3")
    else {
        panic!()
    };
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(
        !ev.iter().any(
            |e| matches!(e, CanonEvent::ApprovalExpired { request_id, .. } if *request_id == r2)
        ),
        "an answered request does not expire"
    );
    let (again, out) = core.answer(&r2, &ans("cancel")).unwrap();
    assert!(again.duplicate && out.is_empty() && again.option_id == "accept");
    assert_eq!(
        core.answer(&r3, &ans("accept")).unwrap_err(),
        AdapterError::Expired(r3.clone())
    );
    assert_eq!(
        core.answer("req-99", &ans("accept")).unwrap_err(),
        AdapterError::UnknownRequest("req-99".into())
    );
}

#[test]
fn child_thread_turns_never_replace_the_parent_turn() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": {"type": "collabAgentToolCall", "id": "spawn1",
            "tool": "spawn_agent", "receiverThreadIds": ["child-1"]}}),
    );
    let ev = emits(&notify(
        &mut core,
        "turn/started",
        json!({"threadId": "child-1", "turn": {"id": "c1"}}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(core.current_turn(), Some("turn-1"));
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": "child-1", "item": {"type": "commandExecution", "id": "cc1", "command": "ls"}}),
    ));
    assert!(
        matches!(&ev[0], CanonEvent::ToolStarted { parent_node_id: Some(p), .. } if p == "node-2")
    );
    let ev = emits(&notify(
        &mut core,
        "turn/completed",
        json!({"threadId": "child-1", "turn": {"id": "c1", "status": "completed"}}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(core.current_turn(), Some("turn-1"));
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted { turn_id, status: TurnStatus::Done, .. }) if turn_id == "turn-1"
    ));
}

#[test]
fn a_second_native_turn_never_replaces_the_running_one() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": "other"}}),
    ));
    assert!(ev.is_empty());
    assert_eq!(core.unknown_frames(), 1);
    assert!(emits(&complete(&mut core, "other", "completed")).is_empty());
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(matches!(ev.last(), Some(CanonEvent::TurnCompleted { .. })));
}

#[test]
fn relative_diff_paths_join_the_thread_cwd_before_the_duplicate_check() {
    // Fixture file_change: the fileChange item has an absolute path, the turn
    // diff has the same file relative to the cwd.
    let mut core = CodexCore::new();
    let out = core.initialize();
    reply(&mut core, &out, json!({"userAgent": "x/0.160.0"}));
    let spec = SessionSpec {
        cwd: Some("/w/".into()),
        ..SessionSpec::default()
    };
    let out = core.open_thread(&spec);
    reply(&mut core, &out, json!({"thread": {"id": T}}));
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "fileChange", "id": "fc1", "status": "completed",
            "changes": [{"path": "/w/a.txt", "kind": {"type": "add"}, "diff": ""}]}}),
    ));
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, CanonEvent::FileChanged { .. }))
            .count(),
        1
    );
    let ev = emits(&notify(
        &mut core,
        "turn/diff/updated",
        json!({"threadId": T, "turnId": "n1", "diff": "diff --git a/a.txt b/a.txt\n+x\n"}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
}

#[test]
fn empty_agent_messages_are_not_reported() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "agentMessage", "id": "m1", "text": ""}}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
}
