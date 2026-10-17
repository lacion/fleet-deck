//! Every recorded Codex fixture replays offline with identical host writes and
//! no unknown frame (PLAN §13, S2-2).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use s2_codex::replay::{ReplayReport, replay};
use spike_canon::fixture::Fixture;

pub const SHARED: &[&str] = &[
    "simple",
    "multi_turn",
    "queued_turn",
    "turn_interrupt",
    "message_steering",
    "provider_thread_resume",
    "tool_call_approve",
    "tool_call_deny",
];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.to_string_lossy();
            n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl")
        })
        .collect();
    v.sort();
    v
}

fn run(name: &str) -> ReplayReport {
    let fx = Fixture::load(&dir().join(format!("{name}.jsonl"))).unwrap();
    replay(&fx).unwrap()
}

#[test]
fn every_fixture_replays_with_identical_host_writes() {
    let files = files();
    let mut names = Vec::new();
    for f in &files {
        let fx = Fixture::load(f).unwrap();
        let r = replay(&fx).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        assert_eq!(r.unknown_frames, 0, "{}", f.display());
        assert!(r.writes_checked > 0, "{}", f.display());
        names.push(fx.header.scenario);
    }
    for s in SHARED {
        assert!(
            names.iter().any(|n| n == s),
            "shared scenario {s} has no fixture"
        );
    }
}

#[test]
fn no_fixture_is_left_failed() {
    let failed: Vec<_> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".failed.jsonl"))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
}

#[test]
fn a_changed_host_write_fails_the_replay() {
    let mut fx = Fixture::load(&dir().join("simple.jsonl")).unwrap();
    let rec = fx
        .records
        .iter_mut()
        .find_map(|r| match r {
            spike_canon::fixture::Record::In { frame } if frame["method"] == "turn/start" => {
                Some(frame)
            }
            _ => None,
        })
        .unwrap();
    rec["params"]["effort"] = "high".into();
    assert!(replay(&fx).is_err());
}

#[test]
fn simple_replays_to_one_completed_turn() {
    let r = run("simple");
    let kinds: Vec<_> = r
        .events
        .iter()
        .map(spike_canon::CanonEvent::kind_name)
        .collect();
    assert_eq!(kinds.first(), Some(&"session.started"));
    assert!(kinds.contains(&"turn.completed"));
    assert_eq!(kinds.last(), Some(&"session.ended"));
}
