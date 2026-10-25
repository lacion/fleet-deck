//! Regression tests from the round 6 and 7 reviews and from the live recordings
//! (split from `tests.rs` to keep both modules under 800 lines).

use serde_json::json;
use spike_canon::{CanonEvent, ToolStatus, TurnStatus};

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

// Round 8 review: an async agent message with several questions must not drop
// any of them, and RPC question answers are validated before they are written.

fn async_two_questions(core: &mut CodexCore) -> Vec<CanonEvent> {
    emits(&notify(
        core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "agentMessage", "id": "m1", "text": "Two things", "delivery": "async",
        "questions": [
            {"title": "Which color?", "options": ["red", "blue"]},
            {"title": "Which size?"}
        ]}}),
    ))
}

#[test]
fn every_async_question_becomes_its_own_request() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = async_two_questions(&mut core);
    let asked: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::ApprovalRequested {
                request_id,
                title,
                options,
                provider_refs,
                ..
            } => Some((
                request_id.clone(),
                title.clone(),
                options
                    .iter()
                    .map(|o| o.option_id.clone())
                    .collect::<Vec<_>>(),
                provider_refs["question_index"].clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 2, "{ev:?}");
    assert_eq!(asked[0].1, "Which color?");
    assert_eq!(asked[0].2, ["red", "blue", "answer"]);
    assert_eq!(asked[0].3, json!("0"));
    assert_eq!(asked[1].1, "Which size?");
    assert_eq!(
        asked[1].2,
        ["answer"],
        "a free-text question offers only `answer`"
    );
    assert_eq!(asked[1].3, json!("1"));
    complete(&mut core, "n1", "completed");

    // Each answer is its own user message and names its question.
    let (_, out) = core
        .answer(
            &asked[1].0,
            &Answer {
                option_id: "answer".into(),
                updated_input: None,
                message: Some("large".into()),
            },
        )
        .unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["params"]["input"][0]["text"], "Which size?: large");
    let (_, out) = core
        .answer(
            &asked[0].0,
            &Answer {
                option_id: "blue".into(),
                updated_input: None,
                message: None,
            },
        )
        .unwrap();
    assert!(
        writes(&out).is_empty(),
        "the second answer waits for the turn"
    );
    assert_eq!(core.pending_inputs(), 2);
}

#[test]
fn a_single_async_question_gets_the_bare_answer() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "agentMessage", "id": "m1", "text": "?", "delivery": "async",
            "questions": [{"title": "Which color?", "options": ["red"]}]}}),
    ));
    let CanonEvent::ApprovalRequested { request_id, .. } = &ev[1] else {
        panic!("{ev:?}")
    };
    complete(&mut core, "n1", "completed");
    let (_, out) = core
        .answer(
            request_id,
            &Answer {
                option_id: "red".into(),
                updated_input: None,
                message: None,
            },
        )
        .unwrap();
    assert_eq!(writes(&out)[0]["params"]["input"][0]["text"], "red");
}

#[test]
fn rpc_question_answers_are_validated_before_the_write() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&feed(
        &mut core,
        json!({"id": 3, "method": "item/tool/requestUserInput", "params": {
            "threadId": T, "turnId": "n1", "itemId": "call_q", "isBlocking": true,
            "questions": [
                {"id": "color", "header": "Color", "question": "Which color?", "options": [{"label": "Blue"}]},
                {"id": "size", "header": "Size", "question": "Which size?", "options": null}
            ]
        }}),
    ));
    let CanonEvent::ApprovalRequested { request_id, .. } = &ev[0] else {
        panic!("{ev:?}")
    };
    let with = |input: serde_json::Value| Answer {
        option_id: "answer".into(),
        updated_input: Some(input),
        message: None,
    };
    for bad in [
        json!({"answers": {}}),
        json!({"answers": {"shape": {"answers": ["round"]}}}),
        json!({"answers": {"color": "blue"}}),
        json!({"answers": {"color": {"answers": "blue"}}}),
        json!({"answers": {"color": {"answers": [1]}}}),
        json!({"answers": ["blue"]}),
    ] {
        assert!(
            matches!(
                core.answer(request_id, &with(bad.clone())),
                Err(AdapterError::InvalidAnswer(_))
            ),
            "accepted {bad}"
        );
    }
    let good = json!({"answers": {"color": {"answers": ["Blue"]}, "size": {"answers": ["large"]}}});
    let (receipt, out) = core.answer(request_id, &with(good.clone())).unwrap();
    assert!(
        !receipt.duplicate,
        "a refused answer leaves the request open"
    );
    assert_eq!(writes(&out), vec![json!({"id": 3, "result": good})]);
}

/// Codex 0.160.0 names the child thread of `spawnAgent` only in the
/// completed item (`receiverThreadIds` is empty in `item/started`), as in the
/// `subagent` fixture. The child's tool items must still get the parent link.
#[test]
fn spawn_agent_links_its_child_thread_when_the_item_completes() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let spawn = json!({"type": "collabAgentToolCall", "id": "spawn1", "tool": "spawnAgent",
        "receiverThreadIds": [], "status": "inProgress"});
    notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": spawn}),
    );
    assert_eq!(core.child_thread_parent("child-1"), None);
    let mut done = json!({"type": "collabAgentToolCall", "id": "spawn1", "tool": "spawnAgent",
        "receiverThreadIds": ["child-1"], "status": "completed"});
    notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": done.clone()}),
    );
    assert_eq!(core.child_thread_parent("child-1"), Some("node-2"));
    // A later `wait` call names the same thread; the first parent stays.
    done["id"] = json!("wait1");
    done["tool"] = json!("wait");
    notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": done}),
    );
    assert_eq!(core.child_thread_parent("child-1"), Some("node-2"));
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": "child-1", "item": {"type": "commandExecution", "id": "cc1", "command": "ls"}}),
    ));
    assert!(
        matches!(&ev[0], CanonEvent::ToolStarted { parent_node_id: Some(p), .. } if p == "node-2"),
        "{ev:?}"
    );
    // The child's own message is not a message of the parent turn.
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": "child-1", "item": {"type": "agentMessage", "id": "m1", "text": "sub-ok"}}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
}

fn command_item(core: &mut CodexCore, method: &str, id: &str, status: &str) -> Vec<CanonEvent> {
    emits(&notify(
        core,
        method,
        json!({"threadId": T, "item": {"type": "commandExecution", "id": id,
            "command": "sleep 25", "status": status}}),
    ))
}

fn completions(ev: &[CanonEvent]) -> Vec<(String, ToolStatus)> {
    ev.iter()
        .filter_map(|e| match e {
            CanonEvent::ToolCompleted { node_id, status } => Some((node_id.clone(), *status)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_command_that_outlives_a_completed_turn_stays_open_until_its_item_completes() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    command_item(&mut core, "item/started", "bg1", "inProgress");
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": {"type": "mcpToolCall", "id": "mcp1", "server": "s", "tool": "t"}}),
    ));
    let mcp = match &ev[0] {
        CanonEvent::ToolStarted { node_id, .. } => node_id.clone(),
        e => panic!("{e:?}"),
    };
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert_eq!(
        completions(&ev),
        [(mcp, ToolStatus::Failed)],
        "only the non-command tool closes with the turn (trap 6)"
    );
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Done,
            ..
        })
    ));
    let bg = core.background_tools();
    assert_eq!(bg.len(), 1);
    let ev = command_item(&mut core, "item/completed", "bg1", "completed");
    assert_eq!(completions(&ev), [(bg[0].clone(), ToolStatus::Ok)]);
    assert!(core.background_tools().is_empty());
    // A repeated completion stays silent.
    assert!(command_item(&mut core, "item/completed", "bg1", "completed").is_empty());
}

#[test]
fn a_background_command_keeps_its_failure_status() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    command_item(&mut core, "item/started", "bg1", "inProgress");
    complete(&mut core, "n1", "completed");
    // The next turn ends while the background command still runs.
    start_turn(&mut core, "in-2", "n2");
    let ev = emits(&complete(&mut core, "n2", "interrupted"));
    assert!(completions(&ev).is_empty(), "{ev:?}");
    let ev = command_item(&mut core, "item/completed", "bg1", "failed");
    assert_eq!(completions(&ev)[0].1, ToolStatus::Failed);
}

#[test]
fn background_commands_close_when_the_process_ends() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    command_item(&mut core, "item/started", "bg1", "inProgress");
    complete(&mut core, "n1", "completed");
    let node = core.background_tools()[0].clone();
    let ev = emits(&core.on_exit(Some(0)));
    assert_eq!(completions(&ev), [(node, ToolStatus::Failed)]);
}

#[test]
fn a_command_of_an_interrupted_turn_closes_with_it() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    command_item(&mut core, "item/started", "c1", "inProgress");
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    assert_eq!(completions(&ev).len(), 1);
    assert_eq!(completions(&ev)[0].1, ToolStatus::Failed);
    assert!(core.background_tools().is_empty());
}
