//! Capability-matrix evidence (PLAN §6.2) from the extra Claude recordings:
//! fork, plan capture, subagent parent link, background work, file-change
//! telemetry, the question response channel, stop before the turn started
//! (trap 3), and reconciliation of a prompt from the CLI transcript (§6.4).
//! Each test name is cited in the matrix and in ADR-001.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use s1_claude::record::transcript::NOTE_PREFIX;
use s1_claude::replay::replay;
use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{CanonEvent, InputState, RequestKind, ResponseChannel, ToolKind, TurnStatus};

fn load(name: &str) -> Fixture {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust");
    Fixture::load(&dir.join(format!("{name}.jsonl"))).unwrap()
}

fn out_frames(fx: &Fixture) -> Vec<&Value> {
    fx.records
        .iter()
        .filter_map(|r| match r {
            Record::Out { frame } => Some(frame),
            _ => None,
        })
        .collect()
}

fn system<'a>(fx: &'a Fixture, subtype: &str) -> Vec<&'a Value> {
    out_frames(fx)
        .into_iter()
        .filter(|f| f["type"] == "system" && f["subtype"] == subtype)
        .collect()
}

fn session_ids(fx: &Fixture) -> Vec<String> {
    system(fx, "init")
        .iter()
        .filter_map(|f| f["session_id"].as_str().map(str::to_owned))
        .collect()
}

fn provider_turns(ev: &[CanonEvent]) -> usize {
    ev.iter()
        .filter(|e| {
            matches!(e, CanonEvent::TurnStarted { provider_refs, .. }
                if provider_refs.get("origin").and_then(Value::as_str) == Some("provider"))
        })
        .count()
}

/// Fork: `--resume <id> --fork-session` starts a new session id with the
/// source's context.
#[test]
fn fork_starts_a_new_session_with_the_old_context() {
    let fx = load("fork_session");
    let ids = session_ids(&fx);
    let first = ids.first().unwrap();
    let last = ids.last().unwrap();
    assert_ne!(first, last, "the fork has its own session id");
    let spawns: Vec<_> = fx
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Spawn { args } => Some(args.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(spawns.len(), 2);
    assert!(spawns[1].iter().any(|a| a == "--fork-session"));
    let ev = replay(&fx).unwrap().events;
    let last_reply = ev
        .iter()
        .rev()
        .find_map(|e| match e {
            CanonEvent::MessageCompleted { text, .. } => Some(text.to_lowercase()),
            _ => None,
        })
        .unwrap();
    assert!(last_reply.contains("maple"), "{last_reply}");
}

/// Plan capture: in `plan` mode the plan arrives as the `ExitPlanMode` tool input
/// (`plan` markdown) and as an approval; a denial keeps plan mode.
#[test]
fn plan_mode_captures_the_plan_from_exit_plan_mode() {
    let ev = replay(&load("plan_capture")).unwrap().events;
    let plan = ev.iter().find_map(|e| match e {
        CanonEvent::PlanProposed { markdown } => Some(markdown.clone()),
        _ => None,
    });
    assert!(plan.is_some_and(|p| p.contains("fd-plan.txt")));
    assert!(ev.iter().any(|e| matches!(e,
        CanonEvent::ApprovalRequested { title, .. } if title == "ExitPlanMode")));
    assert!(matches!(
        ev.iter().find(|e| matches!(e, CanonEvent::SessionStarted { .. })),
        Some(CanonEvent::SessionStarted { mode: Some(m), .. }) if m == "plan"
    ));
}

/// Subagent parent link: the `Agent` tool starts an async subagent; its frames
/// carry `parent_tool_use_id` = the `Agent` tool call. It reports after the
/// parent turn's `result` (trap 5) and starts a provider-origin turn.
#[test]
fn subagent_frames_link_to_the_parent_tool_call() {
    let fx = load("subagent");
    let frames = out_frames(&fx);
    let agent_id = frames
        .iter()
        .flat_map(|f| {
            f["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .find(|b| b["type"] == "tool_use" && b["name"] == "Agent")
        .and_then(|b| b["id"].as_str().map(str::to_owned))
        .unwrap();
    let first_result = frames.iter().position(|f| f["type"] == "result").unwrap();
    let child = frames
        .iter()
        .position(|f| f["parent_tool_use_id"].as_str() == Some(agent_id.as_str()))
        .unwrap();
    assert!(
        child > first_result,
        "the subagent outlives the parent turn"
    );
    assert_eq!(system(&fx, "task_started")[0]["task_type"], "local_agent");
    let ev = replay(&fx).unwrap().events;
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::ToolStarted {
            tool_kind: ToolKind::Subagent,
            ..
        }
    )));
    assert_eq!(provider_turns(&ev), 1);
}

/// Background work: a backgrounded Bash command is reported with
/// `background_tasks_changed` / `task_started`, finishes after the turn, and its
/// `task_notification` starts a provider-origin turn (trap 4).
#[test]
fn background_work_reports_after_the_turn() {
    let fx = load("background_task");
    assert_eq!(system(&fx, "task_started")[0]["is_backgrounded"], true);
    assert_eq!(system(&fx, "task_notification")[0]["status"], "completed");
    let ev = replay(&fx).unwrap().events;
    let updates: Vec<usize> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::BackgroundUpdated { tasks } => Some(tasks.len()),
            _ => None,
        })
        .collect();
    assert_eq!(updates, [1, 0], "pending, then done");
    let first_done = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .unwrap();
    let drained = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::BackgroundUpdated { tasks } if tasks.is_empty()))
        .unwrap();
    assert!(drained > first_done);
    assert_eq!(provider_turns(&ev), 1);
}

/// File-change telemetry: an `Edit`/`Write` tool result maps to `file.changed`
/// with the tool's `file_path`. The CLI sends no separate diff event.
#[test]
fn write_tool_results_map_to_file_changes() {
    let ev = replay(&load("file_edit")).unwrap().events;
    assert!(ev.iter().any(|e| matches!(e,
        CanonEvent::FileChanged { path, .. } if path.ends_with("fd-edit.txt"))));
}

/// Question response channel (L4): `AskUserQuestion` arrives as a `can_use_tool`
/// request; the answer is an RPC reply (`allow` + `updatedInput.answers`).
#[test]
fn questions_are_answered_by_rpc_reply() {
    let fx = load("ask_user_question");
    let ev = replay(&fx).unwrap().events;
    assert!(ev.iter().any(|e| matches!(
        e,
        CanonEvent::ApprovalRequested {
            request_kind: RequestKind::Question,
            response: ResponseChannel::RpcReply,
            ..
        }
    )));
    let answer = fx
        .records
        .iter()
        .find_map(|r| match r {
            Record::In { frame } if frame["type"] == "control_response" => Some(frame),
            _ => None,
        })
        .unwrap();
    let body = &answer["response"]["response"];
    assert_eq!(body["behavior"], "allow");
    assert!(body["updatedInput"]["answers"].is_object());
    let reply = ev
        .iter()
        .rev()
        .find_map(|e| match e {
            CanonEvent::MessageCompleted { text, .. } => Some(text.to_lowercase()),
            _ => None,
        })
        .unwrap();
    assert!(reply.contains("blue"), "{reply}");
}

/// Trap 3: an interrupt written right after the prompt, before any turn frame,
/// ends that input `cancelled` and its turn `interrupted`; the session accepts
/// the next prompt.
#[test]
fn interrupt_before_the_turn_started_cancels_the_input() {
    let ev = replay(&load("interrupt_right_after_send")).unwrap().events;
    let statuses: Vec<TurnStatus> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::TurnCompleted { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [TurnStatus::Interrupted, TurnStatus::Done]);
    let states: Vec<InputState> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::InputUpdated { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert!(states.contains(&InputState::Cancelled));
    assert_eq!(states.last(), Some(&InputState::Completed));
}

/// §6.4 reconciliation: the CLI persists each prompt in its session transcript
/// under the client `uuid`. After a crash, a station can look up an
/// `outcome_unknown` prompt by its `uuid` instead of resending it.
#[test]
fn the_transcript_keeps_prompts_under_the_client_uuid() {
    let fx = load("approval_restart");
    let note = fx
        .records
        .iter()
        .find_map(|r| match r {
            Record::Note { text } => text.strip_prefix(NOTE_PREFIX),
            _ => None,
        })
        .unwrap();
    let note: Value = serde_json::from_str(note).unwrap();
    let persisted: Vec<&str> = note["prompt_uuids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let sent: Vec<String> = fx
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Host { cmd } if cmd["op"] == "prompt" => {
                cmd["input_id"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(persisted, sent, "both prompts, in order, under their uuids");
}
