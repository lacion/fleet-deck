use serde_json::{Value, json};
use spike_canon::{CanonEvent, InputState, RequestKind, ResponseChannel, ToolStatus, TurnStatus};

use super::*;
use crate::rpc::parse_value;

const T: &str = "thr-1";

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

fn feed(core: &mut CodexCore, v: Value) -> Vec<Output> {
    core.on_frame(parse_value(v))
}

fn notify(core: &mut CodexCore, method: &str, params: Value) -> Vec<Output> {
    feed(core, json!({"method": method, "params": params}))
}

/// Replies to the last host request in `out` with `result`.
fn reply(core: &mut CodexCore, out: &[Output], result: Value) -> Vec<Output> {
    let w = writes(out);
    let id = w.last().unwrap()["id"].clone();
    feed(core, json!({"id": id, "result": result}))
}

fn ready_core() -> CodexCore {
    let mut core = CodexCore::new();
    let out = core.initialize();
    let after = reply(
        &mut core,
        &out,
        json!({"userAgent": "fleet-deck-spike/0.160.0 (Linux)"}),
    );
    assert_eq!(writes(&after)[0], json!({"method": "initialized"}));
    let out = core.open_thread(&SessionSpec::default());
    let ev = emits(&reply(
        &mut core,
        &out,
        json!({"thread": {"id": T}, "model": "m"}),
    ));
    assert!(
        matches!(&ev[0], CanonEvent::SessionStarted { provider_version, mode, .. }
        if provider_version.as_deref() == Some("0.160.0") && mode.as_deref() == Some(DEFAULT_MODE))
    );
    core
}

/// Sends a prompt and accepts it as native turn `native`.
fn start_turn(core: &mut CodexCore, input: &str, native: &str) -> Vec<CanonEvent> {
    let out = core.prompt("hi", input).unwrap();
    let mut ev = emits(&reply(
        core,
        &out,
        json!({"turn": {"id": native, "status": "inProgress"}}),
    ));
    ev.extend(emits(&notify(
        core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": native}}),
    )));
    ev.extend(user_item(core, input));
    ev
}

fn user_item(core: &mut CodexCore, client: &str) -> Vec<CanonEvent> {
    emits(&notify(
        core,
        "item/started",
        json!({"threadId": T, "item": {"type": "userMessage", "id": format!("u-{client}"), "clientId": client}}),
    ))
}

fn complete(core: &mut CodexCore, native: &str, status: &str) -> Vec<Output> {
    notify(
        core,
        "turn/completed",
        json!({"threadId": T, "turn": {"id": native, "status": status}}),
    )
}

fn command_approval(core: &mut CodexCore, id: Value, item: &str) -> CanonEvent {
    notify(
        core,
        "item/started",
        json!({"threadId": T, "item": {"type": "commandExecution", "id": item, "command": "touch x"}}),
    );
    let ev = emits(&feed(
        core,
        json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {
            "threadId": T, "turnId": "n1", "itemId": item, "startedAtMs": 1, "command": "touch x",
            "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "x"]}}, "cancel"]
        }}),
    ));
    ev.into_iter().next().unwrap()
}

#[test]
fn modes_parse_and_fall_back_to_the_safest() {
    assert_eq!(
        Mode::parse("on-request/workspace-write").unwrap().name(),
        "on-request/workspace-write"
    );
    assert!(
        Mode::parse("default").is_none(),
        "no sentinel names (trap 2)"
    );
    assert!(Mode::parse("untrusted/full").is_none());
    assert_eq!(Mode::or_safest(Some("bogus")).name(), SAFEST_MODE);
    assert_eq!(Mode::or_safest(None).name(), DEFAULT_MODE);
    assert_eq!(
        Mode::parse("never/danger-full-access")
            .unwrap()
            .sandbox_policy(),
        json!({"type": "dangerFullAccess"})
    );
}

#[test]
fn a_prompt_is_confirmed_only_by_its_own_user_message() {
    let mut core = ready_core();
    let out = core.prompt("hi", "in-1").unwrap();
    assert_eq!(writes(&out)[0]["params"]["clientUserMessageId"], "in-1");
    let ev = emits(&reply(&mut core, &out, json!({"turn": {"id": "n1"}})));
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::InputUpdated {
            state: InputState::Received,
            ..
        }
    )));
    assert!(
        ev.iter()
            .any(|e| matches!(e, CanonEvent::TurnStarted { .. }))
    );
    assert_eq!(
        core.input_state("in-1"),
        Some(InputState::Received),
        "accepted is not confirmed"
    );
    // Unrelated frames confirm nothing.
    notify(
        &mut core,
        "thread/status/changed",
        json!({"threadId": T, "status": {"type": "active"}}),
    );
    user_item(&mut core, "someone-else");
    notify(
        &mut core,
        "item/started",
        json!({"threadId": "child", "item": {"type": "userMessage", "id": "c", "clientId": "in-1"}}),
    );
    assert_eq!(core.input_state("in-1"), Some(InputState::Received));
    let ev = user_item(&mut core, "in-1");
    assert!(
        matches!(&ev[0], CanonEvent::InputUpdated { state: InputState::Started, turn_id: Some(t), .. } if t == "turn-1")
    );
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(matches!(
        &ev[0],
        CanonEvent::InputUpdated {
            state: InputState::Completed,
            ..
        }
    ));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Done,
            ..
        })
    ));
    assert!(core.is_idle());
}

#[test]
fn a_busy_prompt_waits_host_side_and_runs_as_its_own_turn() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let out = core.prompt("second", "in-2").unwrap();
    assert!(
        out.is_empty(),
        "no turn/start while a turn runs: codex would merge it"
    );
    assert_eq!(core.pending_inputs(), 1);
    let out = complete(&mut core, "n1", "completed");
    let w = writes(&out);
    assert_eq!(w[0]["method"], "turn/start");
    assert_eq!(w[0]["params"]["clientUserMessageId"], "in-2");
    let ev = emits(&reply(&mut core, &out, json!({"turn": {"id": "n2"}})));
    assert!(
        ev.iter()
            .any(|e| matches!(e, CanonEvent::TurnStarted { turn_id, .. } if turn_id == "turn-2"))
    );
}

#[test]
fn steer_targets_the_running_turn() {
    let mut core = ready_core();
    assert_eq!(core.steer("x", "s-0").unwrap_err(), AdapterError::NoTurn);
    start_turn(&mut core, "in-1", "n1");
    let out = core.steer("also say banana", "s-1").unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["method"], "turn/steer");
    assert_eq!(w[0]["params"]["expectedTurnId"], "n1");
    reply(&mut core, &out, json!({"turnId": "n1"}));
    let ev = user_item(&mut core, "s-1");
    assert!(
        matches!(&ev[0], CanonEvent::InputUpdated { state: InputState::Started, turn_id: Some(t), .. } if t == "turn-1")
    );
    let ev = emits(&complete(&mut core, "n1", "completed"));
    let completed = ev
        .iter()
        .filter(|e| {
            matches!(
                e,
                CanonEvent::InputUpdated {
                    state: InputState::Completed,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        completed, 2,
        "the prompt and the steer complete with the turn"
    );
}

#[test]
fn approvals_use_the_native_decisions_and_answers_are_idempotent() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let CanonEvent::ApprovalRequested {
        request_id,
        node_id,
        options,
        response,
        request_kind,
        ..
    } = command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    assert_eq!(request_kind, RequestKind::Command);
    assert_eq!(response, ResponseChannel::RpcReply);
    assert_eq!(node_id.as_deref(), Some("node-2"));
    let ids: Vec<_> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["accept", "acceptWithExecpolicyAmendment", "cancel"]);
    let ans = |o: &str| Answer {
        option_id: o.into(),
        updated_input: None,
        message: None,
    };
    assert!(matches!(
        core.answer(&request_id, &ans("decline")),
        Err(AdapterError::UnknownOption { .. })
    ));
    let edited = Answer {
        updated_input: Some(json!({"command": "ls"})),
        ..ans("accept")
    };
    assert_eq!(
        core.answer(&request_id, &edited).unwrap_err(),
        AdapterError::Unsupported("edited input before allow")
    );
    let (_, out) = core
        .answer(&request_id, &ans("acceptWithExecpolicyAmendment"))
        .unwrap();
    assert_eq!(
        writes(&out)[0],
        json!({"id": 0, "result": {"decision": {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "x"]}}}})
    );
    let (r2, out2) = core.answer(&request_id, &ans("cancel")).unwrap();
    assert!(r2.duplicate && out2.is_empty());
    let ev = emits(&notify(
        &mut core,
        "serverRequest/resolved",
        json!({"threadId": T, "requestId": 0}),
    ));
    assert!(ev.is_empty(), "resolved confirms the answer silently");
}

#[test]
fn cancel_marks_the_tool_denied_and_unanswered_requests_expire() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let CanonEvent::ApprovalRequested { request_id, .. } =
        command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    core.answer(
        &request_id,
        &Answer {
            option_id: "cancel".into(),
            updated_input: None,
            message: None,
        },
    )
    .unwrap();
    let CanonEvent::ApprovalRequested { request_id: r2, .. } =
        command_approval(&mut core, json!(1), "call_2")
    else {
        panic!()
    };
    let ev = emits(&notify(
        &mut core,
        "serverRequest/resolved",
        json!({"requestId": 1}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::ApprovalExpired {
            request_id: r2,
            reason: "cancelled_by_provider".into()
        }]
    );
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    assert!(ev.contains(&CanonEvent::ToolCompleted {
        node_id: "node-2".into(),
        status: ToolStatus::Denied
    }));
    assert!(ev.contains(&CanonEvent::ToolCompleted {
        node_id: "node-3".into(),
        status: ToolStatus::Failed
    }));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        })
    ));
}

#[test]
fn request_user_input_is_answered_by_rpc_reply() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&feed(
        &mut core,
        json!({"id": 3, "method": "item/tool/requestUserInput", "params": {
            "threadId": T, "turnId": "n1", "itemId": "call_q", "isBlocking": true,
            "questions": [{"id": "color", "header": "Color", "question": "Which color?", "options": [{"label": "Blue"}]}]
        }}),
    ));
    let CanonEvent::ApprovalRequested {
        request_id,
        request_kind,
        title,
        ..
    } = &ev[0]
    else {
        panic!()
    };
    assert_eq!(*request_kind, RequestKind::Question);
    assert_eq!(title, "Which color?");
    let no_answers = Answer {
        option_id: "answer".into(),
        updated_input: None,
        message: None,
    };
    assert!(matches!(
        core.answer(request_id, &no_answers),
        Err(AdapterError::InvalidAnswer(_))
    ));
    let answers = json!({"answers": {"color": {"answers": ["blue"]}}});
    let (_, out) = core
        .answer(
            request_id,
            &Answer {
                updated_input: Some(answers.clone()),
                ..no_answers
            },
        )
        .unwrap();
    assert_eq!(writes(&out)[0], json!({"id": 3, "result": answers}));
}

#[test]
fn async_questions_are_answered_by_a_new_message() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "agentMessage", "id": "m1", "text": "Pick one", "delivery": "async",
            "questions": [{"title": "Which color?", "options": ["red", "blue"]}]}}),
    ));
    assert!(matches!(&ev[0], CanonEvent::MessageCompleted { .. }));
    let CanonEvent::ApprovalRequested {
        request_id,
        response,
        options,
        ..
    } = &ev[1]
    else {
        panic!()
    };
    assert_eq!(*response, ResponseChannel::Message);
    let ids: Vec<_> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["red", "blue", "answer"]);
    let request_id = request_id.clone();
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(
        !ev.iter()
            .any(|e| matches!(e, CanonEvent::ApprovalExpired { .. })),
        "the question outlives the turn"
    );
    let (_, out) = core
        .answer(
            &request_id,
            &Answer {
                option_id: "blue".into(),
                updated_input: None,
                message: None,
            },
        )
        .unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["method"], "turn/start");
    assert_eq!(w[0]["params"]["input"][0]["text"], "blue");
    assert_eq!(
        w[0]["params"]["clientUserMessageId"],
        format!("answer-{request_id}")
    );
}

#[test]
fn unknown_server_requests_fail_closed() {
    let mut core = ready_core();
    let out = feed(
        &mut core,
        json!({"id": 7, "method": "item/permissions/requestApproval", "params": {}}),
    );
    let w = writes(&out);
    assert_eq!(w[0]["id"], 7);
    assert_eq!(w[0]["error"]["code"], crate::rpc::REFUSED);
    let out = feed(
        &mut core,
        json!({"id": 8, "method": "mcpServer/elicitation/request", "params": {}}),
    );
    assert_eq!(
        writes(&out)[0],
        json!({"id": 8, "result": {"action": "decline"}})
    );
    assert!(core.open_requests().is_empty());
    notify(&mut core, "brand/new", json!({}));
    feed(&mut core, json!({"what": 1}));
    assert_eq!(core.unknown_frames(), 2);
}

#[test]
fn interrupt_before_the_turn_id_is_held_then_sent() {
    let mut core = ready_core();
    assert_eq!(core.interrupt().unwrap_err(), AdapterError::NoTurn);
    let out = core.prompt("long", "in-1").unwrap();
    assert!(
        core.interrupt().unwrap().is_empty(),
        "trap 3: no turn id yet"
    );
    let after = reply(&mut core, &out, json!({"turn": {"id": "n1"}}));
    // Fixture interrupt_right_after_send: `turn/interrupt` before
    // `turn/started` fails with -32600 "no active turn to interrupt".
    assert!(writes(&after).is_empty(), "the turn id alone is not enough");
    assert!(core.interrupt().unwrap().is_empty(), "still held");
    let after = notify(
        &mut core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": "n1"}}),
    );
    let w = writes(&after);
    assert_eq!(w.len(), 1, "one interrupt, sent once");
    assert_eq!(w[0]["method"], "turn/interrupt");
    assert_eq!(w[0]["params"]["turnId"], "n1");
    assert!(
        core.interrupt().unwrap().is_empty(),
        "interrupt is idempotent"
    );
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::InputUpdated {
            state: InputState::Cancelled,
            ..
        }
    )));
    assert!(matches!(
        ev.last(),
        Some(CanonEvent::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        })
    ));
}

#[test]
fn exit_expires_requests_and_discards_follow_ups() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    core.prompt("later", "in-2").unwrap();
    command_approval(&mut core, json!(0), "call_1");
    let ev = emits(&core.on_exit(Some(1)));
    assert!(
        matches!(&ev[0], CanonEvent::ApprovalExpired { reason, .. } if reason == "process_lost")
    );
    assert!(ev.contains(&CanonEvent::ToolCompleted {
        node_id: "node-2".into(),
        status: ToolStatus::Failed
    }));
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::TurnCompleted {
            status: TurnStatus::Failed,
            ..
        }
    )));
    assert_eq!(core.input_state("in-2"), Some(InputState::Discarded));
    assert!(matches!(ev.last(), Some(CanonEvent::SessionEnded { .. })));
}

#[test]
fn set_mode_applies_with_the_next_accepted_turn() {
    let mut core = ready_core();
    assert!(matches!(
        core.set_mode("default"),
        Err(AdapterError::UnsupportedMode(_))
    ));
    assert!(
        core.set_mode("on-request/workspace-write")
            .unwrap()
            .is_empty()
    );
    let out = core.prompt("x", "in-1").unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(w[0]["params"]["sandboxPolicy"]["type"], "workspaceWrite");
    let ev = emits(&reply(&mut core, &out, json!({"turn": {"id": "n1"}})));
    assert!(ev.contains(&CanonEvent::ModeChanged {
        mode: "on-request/workspace-write".into()
    }));
    assert_eq!(core.mode().as_deref(), Some("on-request/workspace-write"));
}

#[test]
fn file_changes_come_from_items_and_the_turn_diff_once() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        json!({"threadId": T, "item": {"type": "fileChange", "id": "fc1", "status": "completed",
            "changes": [{"path": "a.txt", "kind": {"type": "add"}, "diff": ""}]}}),
    ));
    assert!(ev.contains(&CanonEvent::FileChanged {
        path: "a.txt".into(),
        change: spike_canon::FileChange::Added
    }));
    let diff = "diff --git a/a.txt b/a.txt\n+x\ndiff --git a/b.txt b/b.txt\n+y\n";
    let ev = emits(&notify(
        &mut core,
        "turn/diff/updated",
        json!({"threadId": T, "turnId": "n1", "diff": diff}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::FileChanged {
            path: "b.txt".into(),
            change: spike_canon::FileChange::Modified
        }]
    );
    assert_eq!(core.turn_diff(), Some(diff));
}

#[test]
fn usage_limit_errors_get_their_own_code() {
    let mut core = ready_core();
    let ev = emits(&notify(
        &mut core,
        "error",
        json!({"error": {"message": "limit", "codexErrorInfo": "usageLimitExceeded"}, "willRetry": false}),
    ));
    assert_eq!(
        ev,
        vec![CanonEvent::Error {
            code: "usage_limit".into(),
            message: "limit".into()
        }]
    );
}

#[test]
fn a_refused_turn_start_marks_the_input_refused() {
    let mut core = ready_core();
    let out = core.prompt("x", "in-1").unwrap();
    let id = writes(&out)[0]["id"].clone();
    let ev = emits(&feed(
        &mut core,
        json!({"id": id, "error": {"code": -32600, "message": "bad"}}),
    ));
    assert!(matches!(
        &ev[0],
        CanonEvent::InputUpdated {
            state: InputState::Refused,
            ..
        }
    ));
    assert!(core.is_idle());
}

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
