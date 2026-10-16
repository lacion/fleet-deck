//! Regression tests from the spike reviews and the live recordings, plus the
//! production additions (D15 child messages, background polling).

use fleetdeck_proto::event::{BackgroundTask, ExpiredReason, ToolStatus, TurnStatus};
use fleetdeck_proto::{Event, NodeId, RequestId, TurnId};
use serde_json::json;

use super::tests::*;
use super::*;
use crate::adapter::ProviderError;

#[test]
fn answers_stay_idempotent_after_resolution_and_turn_end() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let Event::ApprovalRequested { request_id: r1, .. } =
        command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    assert!(!core.answer(&ans(&r1, "accept")).unwrap().is_empty());
    notify(
        &mut core,
        "serverRequest/resolved",
        json!({"threadId": T, "requestId": 0}),
    );
    assert!(
        core.answer(&ans(&r1, "cancel")).unwrap().is_empty(),
        "no second reply after serverRequest/resolved"
    );
    let Event::ApprovalRequested { request_id: r2, .. } =
        command_approval(&mut core, json!(1), "call_2")
    else {
        panic!()
    };
    core.answer(&ans(&r2, "accept")).unwrap();
    let Event::ApprovalRequested { request_id: r3, .. } =
        command_approval(&mut core, json!(2), "call_3")
    else {
        panic!()
    };
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(
        !ev.iter()
            .any(|e| matches!(e, Event::ApprovalExpired { request_id, .. } if *request_id == r2)),
        "an answered request does not expire"
    );
    assert!(core.answer(&ans(&r2, "cancel")).unwrap().is_empty());
    assert!(matches!(
        core.answer(&ans(&r3, "accept")),
        Err(ProviderError::RequestClosed { request, reason: ExpiredReason::TurnEnded }) if request == r3
    ));
    let unknown = RequestId::new("req-99");
    assert!(matches!(
        core.answer(&ans(&unknown, "accept")),
        Err(ProviderError::UnknownRequest(r)) if r == unknown
    ));
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
    // The child turn reports the agent (B6) and starts no turn.
    assert_eq!(
        ev.iter().map(Event::kind_name).collect::<Vec<_>>(),
        ["task.updated"]
    );
    let turn1 = TurnId::new("turn-1");
    assert_eq!(core.current_turn(), Some(&turn1));
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": "child-1", "item": {"type": "commandExecution", "id": "cc1", "command": "ls"}}),
    ));
    assert!(
        matches!(&ev[0], Event::ToolStarted { parent_node_id: Some(p), .. } if p.as_str() == "node-2")
    );
    let ev = emits(&notify(
        &mut core,
        "turn/completed",
        json!({"threadId": "child-1", "turn": {"id": "c1", "status": "completed"}}),
    ));
    assert_eq!(
        ev.iter().map(Event::kind_name).collect::<Vec<_>>(),
        ["task.updated"]
    );
    assert_eq!(core.current_turn(), Some(&turn1));
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted { turn_id, status: TurnStatus::Done, .. } if *turn_id == turn1
    )));
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
    assert!(matches!(ev.last(), Some(Event::RunCompleted { .. })));
}

#[test]
fn relative_diff_paths_join_the_thread_cwd_before_the_duplicate_check() {
    // Fixture file_change: the fileChange item has an absolute path, the turn
    // diff has the same file relative to the cwd.
    let mut core = ready_core_with(&SessionSpec {
        cwd: Some("/w/".into()),
        ..SessionSpec::default()
    });
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "fileChange", "id": "fc1", "status": "completed",
            "changes": [{"path": "/w/a.txt", "kind": {"type": "add"}, "diff": ""}]}}),
    ));
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, Event::FileChanged { .. }))
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

fn async_two_questions(core: &mut CodexCore) -> Vec<Event> {
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
            Event::ApprovalRequested {
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
    let mut size = ans(&asked[1].0, "answer");
    size.decision.text = Some("large".into());
    let out = core.answer(&size).unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["params"]["input"][0]["text"], "Which size?: large");
    let out = core.answer(&ans(&asked[0].0, "blue")).unwrap();
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
    let Event::ApprovalRequested { request_id, .. } = &ev[1] else {
        panic!("{ev:?}")
    };
    complete(&mut core, "n1", "completed");
    let out = core.answer(&ans(request_id, "red")).unwrap();
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
    // ev[0] is the question node (DL-068).
    let Event::ApprovalRequested { request_id, .. } = &ev[1] else {
        panic!("{ev:?}")
    };
    let with = |input: serde_json::Value| {
        let mut a = ans(request_id, "answer");
        a.decision.updated_input = Some(input);
        a
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
                core.answer(&with(bad.clone())),
                Err(ProviderError::InvalidAnswer(_))
            ),
            "accepted {bad}"
        );
    }
    let good = json!({"answers": {"color": {"answers": ["Blue"]}, "size": {"answers": ["large"]}}});
    let out = core.answer(&with(good.clone())).unwrap();
    assert_eq!(writes(&out), vec![json!({"id": 3, "result": good})]);
}

/// The station admits a first answer with the same pure validator, against
/// the request input of `approval.requested` (M2 gate correction, DL-070).
#[test]
fn the_station_validator_is_the_core_validator_over_the_event_input() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&feed(
        &mut core,
        json!({"id": 4, "method": "item/tool/requestUserInput", "params": {
            "threadId": T, "turnId": "n1", "itemId": "call_q", "isBlocking": true,
            "questions": [{"id": "choice", "header": "Color", "question": "Which color?",
                "options": [{"label": "Red"}, {"label": "Blue"}]}]
        }}),
    ));
    let Event::ApprovalRequested {
        request_id, input, ..
    } = &ev[1]
    else {
        panic!("{ev:?}")
    };
    let params = input.as_ref().expect("an RPC question keeps its params");
    for bad in [
        None,
        Some(json!({"answers": {}})),
        Some(json!({"answers": {"colour": {"answers": ["Blue"]}}})),
        Some(json!({"answers": {"choice": {"answers": [7]}}})),
    ] {
        assert!(
            matches!(
                validate_question_answer(params, bad.as_ref()),
                Err(ProviderError::InvalidAnswer(_))
            ),
            "accepted {bad:?}"
        );
        let mut a = ans(request_id, "answer");
        a.decision.updated_input = bad;
        assert!(core.answer(&a).is_err(), "the core agrees");
    }
    let good = json!({"answers": {"choice": {"answers": ["Blue"]}}});
    let reply = validate_question_answer(params, Some(&good)).unwrap();
    let mut a = ans(request_id, "answer");
    a.decision.updated_input = Some(good);
    let out = core.answer(&a).unwrap();
    assert_eq!(writes(&out), vec![json!({"id": 4, "result": reply})]);
}

/// Codex 0.160.0 names the child thread of `spawnAgent` only in the
/// completed item (`receiverThreadIds` is empty in `item/started`), as in the
/// `subagent` fixture. The child's items still get the parent link, and the
/// child's messages become messages of that node (D15 child timelines).
#[test]
fn spawn_agent_links_its_child_thread_when_the_item_completes() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let spawn = json!({"type": "collabAgentToolCall", "id": "spawn1", "tool": "spawnAgent",
        "prompt": "check the tests", "receiverThreadIds": [], "status": "inProgress"});
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": spawn}),
    ));
    assert!(matches!(
        &ev[0],
        Event::ToolStarted { title, input_summary, tool_kind: fleetdeck_proto::event::ToolKind::Subagent, .. }
            if title == "spawnAgent" && input_summary == "check the tests"
    ));
    assert_eq!(core.child_thread_parent("child-1"), None);
    let mut done = json!({"type": "collabAgentToolCall", "id": "spawn1", "tool": "spawnAgent",
        "receiverThreadIds": ["child-1"], "status": "completed"});
    notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": done.clone()}),
    );
    let node2 = NodeId::new("node-2");
    assert_eq!(core.child_thread_parent("child-1"), Some(&node2));
    // A later `wait` call names the same thread; the first parent stays.
    done["id"] = json!("wait1");
    done["tool"] = json!("wait");
    notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "item": done}),
    );
    assert_eq!(core.child_thread_parent("child-1"), Some(&node2));
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": "child-1", "item": {"type": "commandExecution", "id": "cc1", "command": "ls"}}),
    ));
    assert!(
        matches!(&ev[0], Event::ToolStarted { parent_node_id: Some(p), .. } if *p == node2),
        "{ev:?}"
    );
    // The child's deltas stay out; its completed message nests under node-2.
    let ev = emits(&notify(
        &mut core,
        "item/agentMessage/delta",
        json!({"threadId": "child-1", "itemId": "m1", "delta": "sub"}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": "child-1", "item": {"type": "agentMessage", "id": "m1", "text": "sub-ok"}}),
    ));
    assert!(
        matches!(&ev[..], [Event::MessageCompleted { parent_node_id: Some(p), text, .. }] if *p == node2 && text == "sub-ok"),
        "{ev:?}"
    );
    // A thread that no tool call named is not this session's.
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": "stranger", "item": {"type": "agentMessage", "id": "m2", "text": "hi"}}),
    ));
    assert!(ev.is_empty(), "{ev:?}");
}

fn command_item(core: &mut CodexCore, method: &str, id: &str, status: &str) -> Vec<Event> {
    emits(&notify(
        core,
        method,
        json!({"threadId": T, "item": {"type": "commandExecution", "id": id,
            "command": "sleep 25", "status": status, "exitCode": if status == "completed" { json!(0) } else { json!(null) }}}),
    ))
}

fn completions(ev: &[Event]) -> Vec<(NodeId, ToolStatus)> {
    ev.iter()
        .filter_map(|e| match e {
            Event::ToolCompleted {
                node_id, status, ..
            } => Some((node_id.clone(), *status)),
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
        Event::ToolStarted { node_id, title, .. } => {
            assert_eq!(title, "s.t");
            node_id.clone()
        }
        e => panic!("{e:?}"),
    };
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert_eq!(
        completions(&ev),
        [(mcp, ToolStatus::Failed)],
        "only the non-command tool closes with the turn (trap 6)"
    );
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::RunCompleted {
            background_pending: true,
            ..
        }
    )));
    let bg = core.background_tools();
    assert_eq!(bg.len(), 1);
    let ev = command_item(&mut core, "item/completed", "bg1", "completed");
    assert!(matches!(
        &ev[..],
        [Event::ToolCompleted { node_id, status: ToolStatus::Ok, exit_code: Some(0), .. }] if *node_id == bg[0]
    ));
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

/// Codex reports background terminals only when asked (trap 4, DL-024): the
/// poll reply becomes `background.updated`, and only a change is reported.
#[test]
fn polled_background_terminals_report_only_changes() {
    let mut core = ready_core();
    let out = core.list_background().unwrap();
    assert_eq!(writes(&out)[0]["method"], "thread/backgroundTerminals/list");
    let ev = emits(&reply(
        &mut core,
        &out,
        json!({"data": [{"processId": "p-1", "command": "sleep 25"}]}),
    ));
    let task = BackgroundTask {
        task_id: "p-1".into(),
        task_kind: "background_terminal".into(),
    };
    assert_eq!(ev, vec![Event::BackgroundUpdated { tasks: vec![task] }]);
    let out = core.list_background().unwrap();
    let same = reply(
        &mut core,
        &out,
        json!({"data": [{"processId": "p-1", "command": "sleep 25"}]}),
    );
    assert!(emits(&same).is_empty());
    let out = core.list_background().unwrap();
    let ev = emits(&reply(&mut core, &out, json!({"data": []})));
    assert_eq!(ev, vec![Event::BackgroundUpdated { tasks: Vec::new() }]);
    assert!(core.background().is_empty());
}

#[test]
fn every_host_command_round_trips_through_its_fixture_form() {
    let cmds = [
        json!({"op": "initialize"}),
        json!({"op": "open_thread", "cwd": "<workdir>", "effort": "low", "fork": true, "resume": "t"}),
        json!({"op": "prompt", "text": "hi", "input_id": "fd-in-1"}),
        json!({"op": "steer", "text": "also", "input_id": "fd-in-2"}),
        json!({"op": "answer", "request_id": "req-1", "option_id": "accept"}),
        json!({"op": "interrupt"}),
        json!({"op": "set_mode", "mode": "untrusted/read-only"}),
        json!({"op": "set_model", "model": "gpt-next"}),
        json!({"op": "set_collaboration", "mode": "plan"}),
        json!({"op": "list_background"}),
    ];
    for c in cmds {
        let cmd: HostCmd = serde_json::from_value(c.clone()).unwrap();
        assert_eq!(serde_json::to_value(&cmd).unwrap(), c);
    }
}
