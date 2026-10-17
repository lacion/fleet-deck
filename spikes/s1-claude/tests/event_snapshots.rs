//! `insta` snapshots of the canonical event stream of every Claude fixture.
//!
//! A change in the adapter's mapping (PLAN §6.3) shows up as a snapshot diff for
//! each affected scenario, for our own recordings (`rust__*`) and for the
//! TypeScript SDK reference recordings (`sdk__*`). Review a diff with
//! `cargo insta review`; accept it only when the new mapping is intended.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use s1_claude::reference::replay_reference;
use s1_claude::replay::replay;
use spike_canon::CanonEvent;
use spike_canon::fixture::Fixture;

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(kind);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.to_string_lossy();
            n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl")
        })
        .collect();
    files.sort();
    files
}

fn snapshot_all(kind: &str, run: impl Fn(&Fixture) -> Vec<CanonEvent>) -> usize {
    let mut n = 0;
    for path in fixtures(kind) {
        let fx = Fixture::load(&path).unwrap();
        let events = run(&fx);
        insta::assert_json_snapshot!(format!("{kind}__{}", fx.header.scenario), events);
        n += 1;
    }
    n
}

#[test]
fn rust_fixture_event_streams() {
    let n = snapshot_all("rust", |fx| replay(fx).unwrap().events);
    assert!(n >= 12, "{n} Rust fixtures");
}

#[test]
fn sdk_fixture_event_streams() {
    let n = snapshot_all("sdk", |fx| replay_reference(fx).unwrap().events);
    assert!(n >= 10, "{n} SDK fixtures");
}
