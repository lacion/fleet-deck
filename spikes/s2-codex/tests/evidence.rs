//! Adapter evidence from the recorded Codex fixtures (PLAN §6.2, §6.4, M1
//! acceptance): what "prompt confirmed" means, whether open approvals survive a
//! provider restart, the native option ids, interrupt, queue and steer.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use s2_codex::replay::{ReplayReport, replay};
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{
    CanonEvent, FileChange, InputState, RequestKind, ResponseChannel, ToolStatus, TurnStatus,
};

fn load(name: &str) -> Fixture {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{name}.jsonl"));
    Fixture::load(&p).unwrap()
}

fn run(name: &str) -> ReplayReport {
    replay(&load(name)).unwrap()
}

fn notes(fx: &Fixture) -> Vec<String> {
    fx.records
        .iter()
        .filter_map(|r| match r {
            Record::Note { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn input_states(r: &ReplayReport, id: &str) -> Vec<InputState> {
    r.events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::InputUpdated {
                input_id, state, ..
            } if input_id == id => Some(*state),
            _ => None,
        })
        .collect()
}

fn turn_statuses(r: &ReplayReport) -> Vec<TurnStatus> {
    r.events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::TurnCompleted { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

/// E1: `turn/start` success = received; the `userMessage` item with the
/// client id = started (confirmed); `turn/completed` = completed.
#[test]
fn a_prompt_is_confirmed_by_its_user_message_item() {
    let fx = load("simple");
    let item_with_client_id = fx.records.iter().any(|r| match r {
        Record::Out { frame } => {
            frame["method"] == "item/started"
                && frame["params"]["item"]["type"] == "userMessage"
                && frame["params"]["item"]["clientId"] == "fd-in-1"
        }
        _ => false,
    });
    assert!(
        item_with_client_id,
        "the userMessage item carries the client id"
    );
    assert_eq!(
        input_states(&run("simple"), "fd-in-1"),
        [
            InputState::Received,
            InputState::Started,
            InputState::Completed
        ]
    );
}

/// E1: the rollout file keeps every client id, the reconciliation source of
/// §6.4 after a crash (one-sided: found = delivered).
#[test]
fn the_rollout_keeps_every_client_id() {
    for name in [
        "simple",
        "multi_turn",
        "approval_restart",
        "message_steering",
    ] {
        let rollout = notes(&load(name))
            .into_iter()
            .find(|n| n.starts_with("rollout "))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&rollout["rollout ".len()..]).unwrap();
        assert_eq!(v["readable"], true, "{name}");
        assert_eq!(v["client_ids_missing"], serde_json::json!([]), "{name}");
    }
}

/// E2: Codex has no stable follow-up queue; the host holds the second prompt
/// and sends it after the first turn (DL-015).
#[test]
fn queued_prompts_run_as_separate_turns() {
    let r = run("queued_turn");
    assert_eq!(turn_statuses(&r), [TurnStatus::Done, TurnStatus::Done]);
    assert!(notes(&load("queued_turn")).contains(&"pending inputs after two prompts: 2".into()));
}

/// E2: `turn/steer` folds the input into the running turn.
#[test]
fn a_steer_joins_the_running_turn() {
    let r = run("message_steering");
    assert_eq!(turn_statuses(&r), [TurnStatus::Done]);
    let started = input_states(&r, "fd-in-2");
    assert!(started.contains(&InputState::Started), "{started:?}");
}

/// E3: an open approval does not survive a provider restart. The resumed
/// process does not ask again; the core expires it as `process_lost`.
#[test]
fn an_open_approval_does_not_survive_a_restart() {
    let fx = load("approval_restart");
    let r = replay(&fx).unwrap();
    assert_eq!(r.processes, 2);
    let expired: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::ApprovalExpired { reason, .. } => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(expired, ["process_lost"]);
    assert!(
        notes(&fx)
            .contains(&"approval asked again by the resumed process without input: false".into())
    );
}

/// E4: option ids are the native decision values of the request.
#[test]
fn approval_option_ids_are_native_decisions() {
    let r = run("tool_call_approve");
    let options: Vec<String> = r
        .events
        .iter()
        .find_map(|e| match e {
            CanonEvent::ApprovalRequested { options, .. } => {
                Some(options.iter().map(|o| o.option_id.clone()).collect())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        options,
        ["accept", "acceptWithExecpolicyAmendment", "cancel"]
    );
    let deny = run("tool_call_deny");
    assert!(deny.events.iter().any(|e| matches!(
        e,
        CanonEvent::ToolCompleted {
            status: ToolStatus::Denied,
            ..
        }
    )));
}

/// L4: `item/tool/requestUserInput` is answered by an RPC reply.
#[test]
fn questions_use_the_rpc_reply_channel() {
    let r = run("question_rpc");
    assert!(r.events.iter().any(|e| matches!(
        e,
        CanonEvent::ApprovalRequested {
            response: ResponseChannel::RpcReply,
            ..
        }
    )));
}

/// Trap 3: an interrupt sent before `turn/started` is held and then sent once.
#[test]
fn an_interrupt_before_turn_started_is_held() {
    let r = run("interrupt_right_after_send");
    assert_eq!(
        turn_statuses(&r),
        [TurnStatus::Interrupted, TurnStatus::Done]
    );
    assert!(
        !r.events
            .iter()
            .any(|e| matches!(e, CanonEvent::Error { .. })),
        "no -32600 'no active turn to interrupt'"
    );
}

/// Interrupt during a running command ends the tool and the turn.
#[test]
fn an_interrupt_during_a_tool_ends_both() {
    let r = run("interrupt_during_tool");
    assert_eq!(turn_statuses(&r), [TurnStatus::Interrupted]);
    assert_eq!(
        input_states(&r, "fd-in-1").last(),
        Some(&InputState::Cancelled)
    );
}

/// S2 must prove `initialize` + `thread/start`: the core reads the provider
/// version from the `initialize` reply and starts the session on the reply to
/// `thread/start`.
#[test]
fn initialize_and_thread_start_open_the_session() {
    let r = run("simple");
    assert!(matches!(
        &r.events[0],
        CanonEvent::SessionStarted { provider, provider_version, mode, .. }
            if provider == "codex"
                && provider_version.as_deref() == Some("0.160.0")
                && mode.as_deref() == Some("untrusted/workspace-write")
    ));
}

/// S2 must prove `thread/resume`: a new process resumes the same thread id,
/// and the model keeps the context of the first process.
#[test]
fn thread_resume_keeps_the_thread_and_its_context() {
    let r = run("provider_thread_resume");
    assert_eq!(r.processes, 2);
    let threads: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::SessionStarted { provider_refs, .. } => {
                provider_refs.get("thread_id").cloned()
            }
            _ => None,
        })
        .collect();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0], threads[1], "the same thread after resume");
    let last = r.events.iter().rev().find_map(|e| match e {
        CanonEvent::MessageCompleted { text, .. } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(last, Some("maple"));
}

/// S2 must prove both approval requests: the file-change approval
/// (`item/fileChange/requestApproval`) has its own kind and native options.
#[test]
fn file_change_approvals_use_native_decisions() {
    let r = run("file_change");
    let (kind, options) = r
        .events
        .iter()
        .find_map(|e| match e {
            CanonEvent::ApprovalRequested {
                request_kind,
                options,
                ..
            } => Some((
                *request_kind,
                options
                    .iter()
                    .map(|o| o.option_id.clone())
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(kind, RequestKind::FileChange);
    assert!(
        options.starts_with(&["accept".to_owned(), "acceptForSession".to_owned()]),
        "{options:?}"
    );
    let command = run("tool_call_approve");
    assert!(command.events.iter().any(|e| matches!(
        e,
        CanonEvent::ApprovalRequested {
            request_kind: RequestKind::Command,
            ..
        }
    )));
}

/// S2 must prove diff notifications: `item/completed` (fileChange) and
/// `turn/diff/updated` report the same file; the core reports it once.
#[test]
fn diff_notifications_report_each_file_once() {
    let fx = load("file_change");
    let diffs = fx
        .records
        .iter()
        .filter(|r| matches!(r, Record::Out { frame } if frame["method"] == "turn/diff/updated"))
        .count();
    assert!(
        diffs >= 1,
        "the fixture has turn/diff/updated notifications"
    );
    let r = replay(&fx).unwrap();
    let changes: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::FileChanged { path, change } => Some((path.as_str(), *change)),
            _ => None,
        })
        .collect();
    assert_eq!(changes, [("<workdir>/fd-edit.txt", FileChange::Added)]);
}

/// Plan capture: plan mode (`collaborationMode`) reports the plan item.
#[test]
fn plan_mode_reports_the_plan() {
    let r = run("plan_capture");
    assert!(
        r.events
            .iter()
            .any(|e| matches!(e, CanonEvent::PlanProposed { markdown } if !markdown.is_empty()))
    );
}

/// §6.4 reconciliation source: the `thread/resume` reply lists the earlier
/// turns with their `userMessage` items and client ids (positive evidence).
#[test]
fn the_resume_reply_lists_earlier_prompts_by_client_id() {
    let fx = load("provider_thread_resume");
    let client_ids: Vec<String> = fx
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Out { frame } => frame["result"]["thread"]["turns"].as_array().cloned(),
            _ => None,
        })
        .flatten()
        .flat_map(|t| t["items"].as_array().cloned().unwrap_or_default())
        .filter(|i| i["type"] == "userMessage")
        .filter_map(|i| i["clientId"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(client_ids, ["fd-in-1"]);
}
