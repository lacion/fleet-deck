//! Replays the frames that the TypeScript Agent SDK recorded (D8, PLAN §14 S1:
//! "TS SDK frame recording + replay") and compares them with our own fixtures.
//! No network and no `claude` binary are needed.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use s1_claude::reference::{ReferenceReport, replay_reference};
use s1_claude::replay::{ReplayError, replay};
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{CanonEvent, InputState, TurnStatus};

const SHARED: &[&str] = &[
    "simple",
    "multi_turn",
    "queued_turn",
    "turn_interrupt",
    "message_steering",
    "provider_thread_resume",
    "tool_call_approve",
    "tool_call_deny",
];

fn dir(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(kind)
}

fn load(kind: &str, name: &str) -> Fixture {
    Fixture::load(&dir(kind).join(format!("{name}.jsonl"))).unwrap()
}

fn sdk(name: &str) -> ReferenceReport {
    replay_reference(&load("sdk", name)).unwrap()
}

fn turn_statuses(events: &[CanonEvent]) -> Vec<TurnStatus> {
    events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::TurnCompleted { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

#[test]
fn every_sdk_fixture_replays_semantically() {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir("sdk")).unwrap() {
        let path = entry.unwrap().path();
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        if !file.ends_with(".jsonl") || file.ends_with(".failed.jsonl") {
            continue;
        }
        let fx = Fixture::load(&path).unwrap();
        assert!(fx.header.recorder.starts_with("sdk-recorder"), "{file}");
        let r = replay_reference(&fx).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(r.unknown_frames, 0, "{file}");
        assert!(r.writes_checked >= 2, "{file}");
        assert!(
            r.sdk_only_requests.is_empty(),
            "{file}: {:?}",
            r.sdk_only_requests
        );
        names.push(fx.header.scenario);
    }
    for s in SHARED {
        assert!(names.iter().any(|n| n == s), "missing SDK fixture {s}");
    }
}

/// The same scenario gives the same turn outcomes from the SDK frames and from
/// our own frames.
#[test]
fn sdk_and_rust_recordings_end_turns_the_same_way() {
    for name in SHARED
        .iter()
        .copied()
        .chain(["tool_call_edit_input", "set_permission_mode"])
    {
        let ours = replay(&load("rust", name)).unwrap();
        let theirs = sdk(name);
        assert_eq!(
            turn_statuses(&ours.events),
            turn_statuses(&theirs.events),
            "{name}"
        );
    }
}

/// The SDK sends user messages without a `uuid`, so the CLI sends no
/// `command_lifecycle` frames: the SDK stream cannot confirm a prompt (DL-006).
/// Its turns are provider-origin turns and the inputs stay unconfirmed.
#[test]
fn sdk_prompts_are_never_confirmed() {
    let r = sdk("multi_turn");
    assert!(!r.events.iter().any(|e| matches!(
        e,
        CanonEvent::InputUpdated {
            state: InputState::Started,
            ..
        }
    )));
    let origins: Vec<_> = r
        .events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::TurnStarted { provider_refs, .. } => {
                provider_refs.get("origin").and_then(|v| v.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(origins, ["provider", "provider"]);
    assert!(
        r.key_differences.contains("user: core only uuid"),
        "{:?}",
        r.key_differences
    );
}

/// The core writes the same permission answers as the SDK, including `toolUseID`
/// (DL-007) and the edited input.
#[test]
fn approval_answers_match_the_sdk() {
    for name in [
        "tool_call_approve",
        "tool_call_deny",
        "tool_call_edit_input",
    ] {
        let r = sdk(name);
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, CanonEvent::ApprovalResolved { .. })),
            "{name}"
        );
        assert!(
            !r.key_differences
                .iter()
                .any(|k| k.contains("toolUseID") || k.contains("updatedInput")),
            "{name}: {:?}",
            r.key_differences
        );
    }
}

fn with_in_frame(
    mut fx: Fixture,
    pred: impl Fn(&serde_json::Value) -> bool,
    edit: impl Fn(&mut serde_json::Value),
) -> Fixture {
    let rec = fx
        .records
        .iter_mut()
        .find_map(|r| match r {
            Record::In { frame } if pred(frame) => Some(frame),
            _ => None,
        })
        .unwrap();
    edit(rec);
    fx
}

#[test]
fn a_different_sdk_answer_fails() {
    let fx = with_in_frame(
        load("sdk", "tool_call_edit_input"),
        |f| f["type"] == "control_response",
        |f| f["response"]["response"]["toolUseID"] = "toolu_other".into(),
    );
    let err = replay_reference(&fx).unwrap_err();
    assert!(matches!(err, ReplayError::WriteMismatch { .. }), "{err}");
}

#[test]
fn an_answer_to_an_unknown_cli_request_fails() {
    let fx = with_in_frame(
        load("sdk", "tool_call_approve"),
        |f| f["type"] == "control_response",
        |f| f["response"]["request_id"] = "not-a-cli-request".into(),
    );
    let err = replay_reference(&fx).unwrap_err();
    assert!(matches!(err, ReplayError::MissingWrite { .. }), "{err}");
}
