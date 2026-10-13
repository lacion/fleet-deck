//! Unit tests of the Codex core (ported from the S2 spike, mapped to the
//! production event types).

use fleetdeck_proto::command::{ApprovalDecision, UserInput};
use fleetdeck_proto::event::{
    ExpiredReason, FileChange, InputState, RequestKind, ResolvedBy, ResponseChannel, RunOrigin,
    ToolStatus, TurnStatus,
};
use fleetdeck_proto::{Event, InputId, NodeId, Operation, RequestId, TurnId};
use serde_json::{Value, json};

use super::rpc::parse_value;
use super::*;
use crate::adapter::{Answer, HostInput, ProviderError};

pub(super) const T: &str = "thr-1";

pub(super) fn emits(out: &[Output]) -> Vec<Event> {
    out.iter()
        .filter_map(|o| match o {
            Output::Emit(e) => Some(e.clone()),
            Output::Write(_) => None,
        })
        .collect()
}

pub(super) fn writes(out: &[Output]) -> Vec<Value> {
    out.iter()
        .filter_map(|o| match o {
            Output::Write(v) => Some(v.clone()),
            Output::Emit(_) => None,
        })
        .collect()
}

pub(super) fn feed(core: &mut CodexCore, v: Value) -> Vec<Output> {
    core.on_frame(parse_value(v))
}

pub(super) fn notify(core: &mut CodexCore, method: &str, params: Value) -> Vec<Output> {
    feed(core, json!({"method": method, "params": params}))
}

/// Replies to the last host request in `out` with `result`.
pub(super) fn reply(core: &mut CodexCore, out: &[Output], result: Value) -> Vec<Output> {
    let w = writes(out);
    let id = w.last().unwrap()["id"].clone();
    feed(core, json!({"id": id, "result": result}))
}

pub(super) fn input(text: &str, id: &str) -> HostInput {
    HostInput {
        id: InputId::new(id),
        origin: RunOrigin::Board,
        input: UserInput::new(text),
    }
}

pub(super) fn ans(request: &RequestId, option: &str) -> Answer {
    Answer {
        request: request.clone(),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: option.into(),
            updated_input: None,
            text: None,
        },
        by: ResolvedBy::Board,
    }
}

pub(super) fn config(cwd: &str) -> CoreConfig {
    CoreConfig {
        provider_instance: "codex".into(),
        cwd: cwd.into(),
        capabilities: capabilities(false),
        resumed: None,
        client: ClientInfo::fleetdeck(),
        request_models: false,
    }
}

pub(super) fn ready_core_with(spec: &SessionSpec) -> CodexCore {
    let mut core = CodexCore::new(config("/w"));
    let out = core.initialize().unwrap();
    assert_eq!(writes(&out)[0]["params"]["clientInfo"]["name"], "fleetdeck");
    let after = reply(
        &mut core,
        &out,
        json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
    );
    assert_eq!(writes(&after)[0], json!({"method": "initialized"}));
    let out = core.open_thread(spec).unwrap();
    let ev = emits(&reply(
        &mut core,
        &out,
        json!({"thread": {"id": T}, "model": "m"}),
    ));
    assert!(
        matches!(&ev[0], Event::SessionStarted { provider_version, mode, model, .. }
        if provider_version.as_deref() == Some("0.160.0")
            && mode.as_deref() == Some(DEFAULT_MODE)
            && model.as_deref() == Some("m")),
        "{ev:?}"
    );
    core
}

pub(super) fn ready_core() -> CodexCore {
    ready_core_with(&SessionSpec::default())
}

/// Sends a prompt and accepts it as native turn `native`.
pub(super) fn start_turn(core: &mut CodexCore, id: &str, native: &str) -> Vec<Event> {
    let out = core.prompt(&input("hi", id)).unwrap();
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
    ev.extend(user_item(core, id));
    ev
}

pub(super) fn user_item(core: &mut CodexCore, client: &str) -> Vec<Event> {
    emits(&notify(
        core,
        "item/started",
        json!({"threadId": T, "item": {"type": "userMessage", "id": format!("u-{client}"), "clientId": client}}),
    ))
}

pub(super) fn complete(core: &mut CodexCore, native: &str, status: &str) -> Vec<Output> {
    notify(
        core,
        "turn/completed",
        json!({"threadId": T, "turn": {"id": native, "status": status}}),
    )
}

pub(super) fn command_approval(core: &mut CodexCore, id: Value, item: &str) -> Event {
    notify(
        core,
        "item/started",
        json!({"threadId": T, "item": {"type": "commandExecution", "id": item, "command": "touch x"}}),
    );
    let ev = emits(&feed(
        core,
        json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {
            "threadId": T, "turnId": "n1", "itemId": item, "startedAtMs": 1, "command": "touch x",
            "cwd": "/w", "reason": "needs a file",
            "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "x"]}}, "cancel"]
        }}),
    ));
    ev.into_iter().next().unwrap()
}

pub(super) fn tool_completed(node: &str, status: ToolStatus) -> Event {
    Event::ToolCompleted {
        node_id: NodeId::new(node),
        status,
        files: Vec::new(),
        exit_code: None,
    }
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
fn capabilities_offer_full_access_only_when_allowed() {
    let safe = capabilities(false);
    assert!(safe.modes.iter().all(|m| !m.ends_with(FULL_ACCESS_SANDBOX)));
    assert_eq!(safe.modes.first().map(String::as_str), Some(SAFEST_MODE));
    assert!(safe.modes.contains(&DEFAULT_MODE.to_owned()));
    assert!(!safe.native_follow_up && !safe.edit_input && safe.steer);
    let full = capabilities(true);
    assert!(full.modes.contains(&"never/danger-full-access".to_owned()));
    assert_eq!(full.modes.len(), safe.modes.len() + APPROVAL_POLICIES.len());
}

#[test]
fn a_prompt_is_confirmed_only_by_its_own_user_message() {
    let mut core = ready_core();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert_eq!(writes(&out)[0]["params"]["clientUserMessageId"], "in-1");
    let ev = emits(&reply(&mut core, &out, json!({"turn": {"id": "n1"}})));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::InputUpdated {
            state: InputState::Received,
            ..
        }
    )));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::RunStarted { origin: RunOrigin::Board, input_summary, .. } if input_summary == "hi"
    )));
    assert!(ev.iter().any(|e| matches!(e, Event::TurnStarted { .. })));
    let id = InputId::new("in-1");
    assert_eq!(
        core.input_state(&id),
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
    assert_eq!(core.input_state(&id), Some(InputState::Received));
    let ev = user_item(&mut core, "in-1");
    assert!(
        matches!(&ev[0], Event::InputUpdated { state: InputState::Started, turn_id: Some(t), .. } if t.as_str() == "turn-1")
    );
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(matches!(
        &ev[0],
        Event::InputUpdated {
            state: InputState::Completed,
            ..
        }
    ));
    let n = ev.len();
    assert!(matches!(
        &ev[n - 2],
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        }
    ));
    assert!(matches!(&ev[n - 1], Event::RunCompleted { .. }));
    assert!(core.is_idle());
}

#[test]
fn inputs_need_text_and_a_fresh_id() {
    let mut core = ready_core();
    assert!(matches!(
        core.prompt(&input("", "in-0")),
        Err(ProviderError::InvalidInput(_))
    ));
    start_turn(&mut core, "in-1", "n1");
    assert!(matches!(
        core.prompt(&input("again", "in-1")),
        Err(ProviderError::InvalidInput(_))
    ));
}

#[test]
fn a_busy_prompt_waits_host_side_and_runs_as_its_own_turn() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let out = core.prompt(&input("second", "in-2")).unwrap();
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
        ev.iter().any(
            |e| matches!(e, Event::TurnStarted { turn_id, .. } if turn_id.as_str() == "turn-2")
        )
    );
}

#[test]
fn steer_targets_the_running_turn() {
    let mut core = ready_core();
    assert!(matches!(
        core.steer(&input("x", "s-0")),
        Err(ProviderError::NoTurn)
    ));
    start_turn(&mut core, "in-1", "n1");
    let out = core.steer(&input("also say banana", "s-1")).unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["method"], "turn/steer");
    assert_eq!(w[0]["params"]["expectedTurnId"], "n1");
    reply(&mut core, &out, json!({"turnId": "n1"}));
    let ev = user_item(&mut core, "s-1");
    assert!(
        matches!(&ev[0], Event::InputUpdated { state: InputState::Started, turn_id: Some(t), .. } if t.as_str() == "turn-1")
    );
    let ev = emits(&complete(&mut core, "n1", "completed"));
    let completed = ev
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::InputUpdated {
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
    let Event::ApprovalRequested {
        request_id,
        node_id,
        options,
        response,
        request_kind,
        details,
        ..
    } = command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    assert_eq!(request_kind, RequestKind::Command);
    assert_eq!(response, ResponseChannel::RpcReply);
    assert_eq!(details, "needs a file in /w");
    assert_eq!(node_id.as_ref().map(NodeId::as_str), Some("node-2"));
    let ids: Vec<_> = options.iter().map(|o| o.option_id.as_str()).collect();
    assert_eq!(ids, ["accept", "acceptWithExecpolicyAmendment", "cancel"]);
    assert!(matches!(
        core.answer(&ans(&request_id, "decline")),
        Err(ProviderError::UnknownOption { .. })
    ));
    let mut edited = ans(&request_id, "accept");
    edited.decision.updated_input = Some(json!({"command": "ls"}));
    assert!(matches!(
        core.answer(&edited),
        Err(ProviderError::Unsupported(u)) if u.op == Operation::EditInput
    ));
    let out = core
        .answer(&ans(&request_id, "acceptWithExecpolicyAmendment"))
        .unwrap();
    assert_eq!(
        writes(&out)[0],
        json!({"id": 0, "result": {"decision": {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "x"]}}}})
    );
    assert!(emits(&out).iter().any(|e| matches!(
        e,
        Event::ApprovalResolved { option_id, by: ResolvedBy::Board, .. } if option_id == "acceptWithExecpolicyAmendment"
    )));
    assert!(core.answer(&ans(&request_id, "cancel")).unwrap().is_empty());
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
    let Event::ApprovalRequested { request_id, .. } =
        command_approval(&mut core, json!(0), "call_1")
    else {
        panic!()
    };
    core.answer(&ans(&request_id, "cancel")).unwrap();
    let Event::ApprovalRequested { request_id: r2, .. } =
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
        vec![Event::ApprovalExpired {
            request_id: r2,
            reason: ExpiredReason::CancelledByProvider,
        }]
    );
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    assert!(ev.contains(&tool_completed("node-2", ToolStatus::Denied)));
    assert!(ev.contains(&tool_completed("node-3", ToolStatus::Failed)));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        }
    )));
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
    // Codex starts no item for the question: the core opens its node
    // (DL-068), and the request names it.
    let Event::ToolStarted {
        node_id: question,
        title: tool,
        input_summary,
        ..
    } = &ev[0]
    else {
        panic!("{ev:?}")
    };
    assert_eq!(
        (tool.as_str(), input_summary.as_str()),
        ("Question", "Which color?")
    );
    let Event::ApprovalRequested {
        request_id,
        request_kind,
        title,
        node_id,
        ..
    } = &ev[1]
    else {
        panic!()
    };
    assert_eq!(node_id.as_ref(), Some(question));
    assert_eq!(*request_kind, RequestKind::Question);
    assert_eq!(title, "Which color?");
    assert!(matches!(
        core.answer(&ans(request_id, "answer")),
        Err(ProviderError::InvalidAnswer(_))
    ));
    let answers = json!({"answers": {"color": {"answers": ["blue"]}}});
    let mut answer = ans(request_id, "answer");
    answer.decision.updated_input = Some(answers.clone());
    let out = core.answer(&answer).unwrap();
    assert_eq!(writes(&out)[0], json!({"id": 3, "result": answers}));
    // The app-server's resolution is the evidence that it took the answer.
    let ev = emits(&notify(
        &mut core,
        "serverRequest/resolved",
        json!({"threadId": T, "requestId": 3}),
    ));
    assert!(
        matches!(&ev[..], [Event::ToolCompleted { node_id, status: ToolStatus::Ok, .. }] if node_id == question),
        "{ev:?}"
    );
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
    assert!(matches!(&ev[0], Event::MessageCompleted { .. }));
    let Event::ApprovalRequested {
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
            .any(|e| matches!(e, Event::ApprovalExpired { .. })),
        "the question outlives the turn"
    );
    let out = core.answer(&ans(&request_id, "blue")).unwrap();
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
    assert_eq!(w[0]["error"]["code"], rpc::REFUSED);
    assert!(emits(&out).iter().any(|e| matches!(
        e,
        Event::Error { code, .. } if code == "unsupported_request"
    )));
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
    assert!(matches!(core.interrupt(None), Err(ProviderError::NoTurn)));
    let out = core.prompt(&input("long", "in-1")).unwrap();
    assert!(
        core.interrupt(Some("ctl-1".into())).unwrap().is_empty(),
        "trap 3: no turn id yet"
    );
    let after = reply(&mut core, &out, json!({"turn": {"id": "n1"}}));
    // Fixture interrupt_right_after_send: `turn/interrupt` before
    // `turn/started` fails with -32600 "no active turn to interrupt".
    assert!(writes(&after).is_empty(), "the turn id alone is not enough");
    let again = emits(&core.interrupt(Some("ctl-2".into())).unwrap());
    assert!(
        matches!(&again[..], [Event::InterruptResult { accepted: false, control_id: Some(c), .. }] if c == "ctl-2"),
        "a second interrupt is refused: {again:?}"
    );
    let after = notify(
        &mut core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": "n1"}}),
    );
    let w = writes(&after);
    assert_eq!(w.len(), 1, "one interrupt, sent once");
    assert_eq!(w[0]["method"], "turn/interrupt");
    assert_eq!(w[0]["params"]["turnId"], "n1");
    let ev = emits(&reply(&mut core, &after, json!({})));
    assert!(
        matches!(&ev[..], [Event::InterruptResult { accepted: true, control_id: Some(c), .. }] if c == "ctl-1")
    );
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::InputUpdated {
            state: InputState::Cancelled,
            ..
        }
    )));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        }
    )));
}

#[test]
fn a_held_interrupt_is_refused_when_the_turn_ends_first() {
    let mut core = ready_core();
    let out = core.prompt(&input("short", "in-1")).unwrap();
    core.interrupt(Some("ctl-1".into())).unwrap();
    reply(&mut core, &out, json!({"turn": {"id": "n1"}}));
    let ev = emits(&complete(&mut core, "n1", "completed"));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::InterruptResult { accepted: false, control_id: Some(c), .. } if c == "ctl-1"
    )));
}

#[test]
fn exit_expires_requests_and_discards_follow_ups() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    core.prompt(&input("later", "in-2")).unwrap();
    command_approval(&mut core, json!(0), "call_1");
    let ev = emits(&core.on_exit(Some(1)));
    assert!(matches!(
        &ev[0],
        Event::ApprovalExpired {
            reason: ExpiredReason::ProcessLost,
            ..
        }
    ));
    assert!(ev.contains(&tool_completed("node-2", ToolStatus::Failed)));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Failed,
            ..
        }
    )));
    assert_eq!(
        core.input_state(&InputId::new("in-2")),
        Some(InputState::Discarded)
    );
    assert!(matches!(ev.last(), Some(Event::SessionEnded { .. })));
    assert!(core.has_ended());
    assert!(matches!(
        core.prompt(&input("x", "in-3")),
        Err(ProviderError::SessionEnded)
    ));
}

#[test]
fn set_mode_and_model_apply_with_the_next_accepted_turn() {
    let mut core = ready_core();
    assert!(matches!(
        core.set_mode("default"),
        Err(ProviderError::InvalidMode(_))
    ));
    assert!(matches!(
        core.set_mode("never/danger-full-access"),
        Err(ProviderError::InvalidMode(_))
    ));
    assert!(
        core.set_mode("on-request/workspace-write")
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        core.set_model(""),
        Err(ProviderError::InvalidModel(_))
    ));
    assert!(matches!(
        core.set_model("two words"),
        Err(ProviderError::InvalidModel(_))
    ));
    assert!(core.set_model("gpt-next").unwrap().is_empty());
    let out = core.prompt(&input("x", "in-1")).unwrap();
    let w = writes(&out);
    assert_eq!(w[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(w[0]["params"]["sandboxPolicy"]["type"], "workspaceWrite");
    assert_eq!(w[0]["params"]["model"], "gpt-next");
    let ev = emits(&reply(&mut core, &out, json!({"turn": {"id": "n1"}})));
    assert!(ev.contains(&Event::ModeChanged {
        mode: "on-request/workspace-write".into()
    }));
    assert!(ev.contains(&Event::ModelChanged {
        model: "gpt-next".into()
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
            "changes": [{"path": "/w/a.txt", "kind": {"type": "add"}, "diff": ""}]}}),
    ));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::FileChanged { path, change: FileChange::Added, node_id: Some(_) } if path == "/w/a.txt"
    )));
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::ToolCompleted { files, status: ToolStatus::Ok, .. } if files == &["/w/a.txt".to_owned()]
    )));
    let diff = "diff --git a/a.txt b/a.txt\n+x\ndiff --git a/b.txt b/b.txt\n+y\n";
    let ev = emits(&notify(
        &mut core,
        "turn/diff/updated",
        json!({"threadId": T, "turnId": "n1", "diff": diff}),
    ));
    assert_eq!(
        ev,
        vec![Event::FileChanged {
            path: "/w/b.txt".into(),
            change: FileChange::Modified,
            node_id: None,
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
        vec![Event::Error {
            code: "usage_limit".into(),
            message: "limit".into(),
            retry_at: None,
        }]
    );
}

#[test]
fn a_refused_turn_start_marks_the_input_refused() {
    let mut core = ready_core();
    let out = core.prompt(&input("x", "in-1")).unwrap();
    let id = writes(&out)[0]["id"].clone();
    let ev = emits(&feed(
        &mut core,
        json!({"id": id, "error": {"code": -32600, "message": "bad"}}),
    ));
    assert!(matches!(
        &ev[0],
        Event::InputUpdated {
            state: InputState::Refused,
            ..
        }
    ));
    assert!(core.is_idle());
    assert_eq!(core.current_turn(), None::<&TurnId>);
}
