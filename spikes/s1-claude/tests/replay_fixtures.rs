//! Offline replay of every recorded Rust fixture (no network, no `claude` binary).

// Test helpers outside `#[test]` functions: a panic is the wanted failure.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use s1_claude::replay::{ReplayError, replay};
use spike_canon::CanonEvent;
use spike_canon::fixture::{Fixture, Record};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust")
}

fn load(name: &str) -> Fixture {
    Fixture::load(&fixture_dir().join(format!("{name}.jsonl"))).unwrap()
}

#[test]
fn every_fixture_replays_with_identical_host_writes() {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(fixture_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.to_string_lossy();
        if !name.ends_with(".jsonl") || name.ends_with(".failed.jsonl") {
            continue;
        }
        let fx = Fixture::load(&path).unwrap();
        let report = replay(&fx).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(report.writes_checked > 0, "{}", path.display());
        assert_eq!(report.unknown_frames, 0, "{}", path.display());
        names.push(fx.header.scenario.clone());
    }
    for shared in [
        "simple",
        "multi_turn",
        "queued_turn",
        "turn_interrupt",
        "message_steering",
        "provider_thread_resume",
        "tool_call_approve",
        "tool_call_deny",
    ] {
        assert!(
            names.iter().any(|n| n == shared),
            "missing fixture {shared}"
        );
    }
}

fn completed(events: &[CanonEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            CanonEvent::TurnCompleted { status, .. } => Some(format!("{status:?}")),
            _ => None,
        })
        .collect()
}

#[test]
fn queued_turn_runs_as_two_turns() {
    let r = replay(&load("queued_turn")).unwrap();
    assert_eq!(completed(&r.events), ["Done", "Done"]);
}

#[test]
fn steering_folds_into_the_running_turn() {
    let r = replay(&load("message_steering")).unwrap();
    assert_eq!(
        completed(&r.events),
        ["Done"],
        "the steer message joins the running turn"
    );
}

#[test]
fn interrupts_end_the_turn_as_interrupted() {
    for name in ["turn_interrupt", "interrupt_during_tool"] {
        let r = replay(&load(name)).unwrap();
        assert_eq!(
            completed(&r.events).first().map(String::as_str),
            Some("Interrupted"),
            "{name}"
        );
    }
}

#[test]
fn open_approval_does_not_survive_a_restart() {
    let r = replay(&load("approval_restart")).unwrap();
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
    let requested = r
        .events
        .iter()
        .filter(|e| matches!(e, CanonEvent::ApprovalRequested { .. }))
        .count();
    assert_eq!(
        requested, 2,
        "the resumed process asks again only after a new prompt"
    );
}

fn without_in_record(mut fx: Fixture, nth: usize) -> Fixture {
    let pos = fx
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, Record::In { .. }))
        .nth(nth)
        .map(|(i, _)| i)
        .unwrap();
    fx.records.remove(pos);
    fx
}

#[test]
fn a_write_missing_from_the_recording_fails_before_exit() {
    // The last write of `tool_call_deny` is the deny answer; without its `in` record
    // the core's write stays unmatched until the process exits.
    let fx = load("tool_call_deny");
    let ins = fx
        .records
        .iter()
        .filter(|r| matches!(r, Record::In { .. }))
        .count();
    let err = replay(&without_in_record(fx, ins - 1)).unwrap_err();
    assert!(
        matches!(&err, ReplayError::ExtraWrites { extra, .. } if extra[0]["type"] == "control_response"),
        "{err}"
    );
}

#[test]
fn a_dropped_write_in_a_restarted_session_is_not_hidden_by_the_exit() {
    // `approval_restart` kills the first process; the prompt write of that process
    // must still be matched before the `exit` record.
    let fx = load("approval_restart");
    let err = replay(&without_in_record(fx, 1)).unwrap_err();
    assert!(
        matches!(
            err,
            ReplayError::ExtraWrites { .. } | ReplayError::WriteMismatch { .. }
        ),
        "{err}"
    );
}

#[test]
fn a_recorded_write_that_the_core_does_not_make_fails() {
    let mut fx = load("simple");
    let pos = fx
        .records
        .iter()
        .position(|r| matches!(r, Record::Exit { .. }))
        .unwrap();
    fx.records.insert(
        pos,
        Record::In {
            frame: serde_json::json!({"type":"user","message":{"role":"user","content":"ghost"}}),
        },
    );
    let err = replay(&fx).unwrap_err();
    assert!(matches!(err, ReplayError::MissingWrite { .. }), "{err}");
}

#[test]
fn a_changed_write_fails() {
    let mut fx = load("tool_call_edit_input");
    let host = fx
        .records
        .iter_mut()
        .find(|r| matches!(r, Record::Host { cmd } if cmd["op"] == "answer"))
        .unwrap();
    let Record::Host { cmd } = host else {
        unreachable!()
    };
    cmd["updated_input"]["command"] = "rm -r somewhere".into();
    let err = replay(&fx).unwrap_err();
    assert!(matches!(err, ReplayError::WriteMismatch { .. }), "{err}");
}
