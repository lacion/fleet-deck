//! PLAN §6.2 capability-matrix cells that no other test names directly:
//! resume, live deltas, terminal status quality, edited input, permission
//! modes, native ids, and usage. Each test name is cited in the matrix.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use s1_claude::replay::replay;
use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{CanonEvent, TurnStatus};

fn load(name: &str) -> Fixture {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust");
    Fixture::load(&dir.join(format!("{name}.jsonl"))).unwrap()
}

fn events(name: &str) -> Vec<CanonEvent> {
    replay(&load(name)).unwrap().events
}

fn fixture_names() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl"))
        .map(|n| n.trim_end_matches(".jsonl").to_owned())
        .collect();
    names.sort();
    names
}

/// Resume: `--resume <session_id>` keeps the same session id and the context.
#[test]
fn resume_keeps_the_session_id_and_the_context() {
    let ev = events("provider_thread_resume");
    let sessions: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::SessionStarted { provider_refs, .. } => {
                provider_refs.get("session_id").and_then(Value::as_str)
            }
            _ => None,
        })
        .collect();
    assert_eq!(sessions.len(), 2, "two processes");
    assert_eq!(sessions[0], sessions[1], "resume keeps the session id");
    let last = ev
        .iter()
        .rev()
        .find_map(|e| match e {
            CanonEvent::MessageCompleted { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(last.to_lowercase().contains("walnut"), "{last}");
}

/// Live deltas: `stream_event` text deltas arrive before the completed
/// message, and they add up to it.
#[test]
fn live_deltas_arrive_before_the_completed_message() {
    let ev = events("simple");
    let first_delta = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::MessageDelta { .. }))
        .unwrap();
    let completed = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::MessageCompleted { .. }))
        .unwrap();
    assert!(first_delta < completed);
    let joined: String = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::MessageDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let CanonEvent::MessageCompleted { text, .. } = &ev[completed] else {
        unreachable!()
    };
    assert_eq!(&joined, text);
}

/// Terminal status quality is strong: in every recording, every turn that
/// starts ends with exactly one `turn.completed` from the `result` frame, with
/// a definite status.
#[test]
fn every_turn_ends_with_one_definite_terminal_status() {
    let names = fixture_names();
    assert!(names.len() >= 19);
    for name in names {
        let ev = events(&name);
        let started = ev
            .iter()
            .filter(|e| matches!(e, CanonEvent::TurnStarted { .. }))
            .count();
        let completed: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                CanonEvent::TurnCompleted { status, .. } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(started, completed.len(), "{name}");
        // A killed process (approval_restart) ends its turn as failed.
        assert!(
            completed.iter().all(|s| matches!(
                s,
                TurnStatus::Done | TurnStatus::Interrupted | TurnStatus::Failed
            )),
            "{name}: {completed:?}"
        );
    }
}

/// Edit input before allow: the allow answer carries `updatedInput`, and the
/// CLI runs the edited command, not the requested one.
#[test]
fn an_edited_input_runs_instead_of_the_requested_one() {
    let fx = load("tool_call_edit_input");
    let answer = fx
        .records
        .iter()
        .find_map(|r| match r {
            Record::In { frame } if frame["type"] == "control_response" => Some(frame),
            _ => None,
        })
        .unwrap();
    let edited = answer["response"]["response"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    assert!(edited.contains("edited-ok"), "{edited}");
    let ran_edited = fx.records.iter().any(|r| match r {
        Record::Out { frame } if frame["type"] == "user" => frame.to_string().contains("edited-ok"),
        _ => false,
    });
    assert!(
        ran_edited,
        "the tool result shows the edited command's output"
    );
    replay(&fx).unwrap();
}

/// Permission modes: the process starts with the native mode the host passed
/// (`--permission-mode default`), and `set_permission_mode` changes it with a
/// `mode.changed`. The CLI sends `system/init` only with the first turn, so
/// `session.started` already reports the new mode.
#[test]
fn modes_are_native_and_change_on_request() {
    let fx = load("set_permission_mode");
    let args = fx
        .records
        .iter()
        .find_map(|r| match r {
            Record::Spawn { args } => Some(args.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--permission-mode" && w[1] == "default")
    );
    let ev = replay(&fx).unwrap().events;
    assert!(
        ev.iter()
            .any(|e| matches!(e, CanonEvent::ModeChanged { mode } if mode == "plan"))
    );
    assert!(ev.iter().any(
        |e| matches!(e, CanonEvent::SessionStarted { mode, .. } if mode.as_deref() == Some("plan"))
    ));
}

/// Native ids: the session id, the tool-use id, and the control request id
/// are kept in `provider_refs`; the app ids are ordinals (L6).
#[test]
fn native_ids_are_kept_beside_ordinal_app_ids() {
    let ev = events("tool_call_approve");
    let mut seen = Vec::new();
    for e in &ev {
        match e {
            CanonEvent::SessionStarted { provider_refs, .. } => {
                assert!(provider_refs["session_id"].is_string());
                seen.push("session");
            }
            CanonEvent::ToolStarted {
                node_id,
                provider_refs,
                ..
            } => {
                assert!(node_id.starts_with("node-"));
                assert!(
                    provider_refs["tool_use_id"]
                        .as_str()
                        .unwrap()
                        .starts_with("toolu_")
                );
                seen.push("tool");
            }
            CanonEvent::ApprovalRequested {
                request_id,
                provider_refs,
                ..
            } => {
                assert!(request_id.starts_with("req-"));
                assert!(!provider_refs.is_empty(), "the native request id is kept");
                seen.push("request");
            }
            CanonEvent::TurnStarted { turn_id, .. } => {
                assert!(turn_id.starts_with("turn-"));
                seen.push("turn");
            }
            _ => {}
        }
    }
    for kind in ["session", "turn", "tool", "request"] {
        assert!(seen.contains(&kind), "{kind} missing");
    }
}

/// Usage: the `result` frame gives token counts and the cost in USD.
#[test]
fn usage_comes_with_every_result() {
    let ev = events("multi_turn");
    let usage: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            CanonEvent::UsageUpdated { usage } => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 2, "one per turn");
    for u in usage {
        assert!(u.input_tokens.unwrap() > 0);
        assert!(u.output_tokens.unwrap() > 0);
        assert!(u.cost_usd.unwrap() > 0.0);
    }
}
