//! Fleet Git API tests on loopback (T2.5, design §14.4, owner acceptance A2
//! FG-11): the config drift report. The branches route and the coordinator
//! tests of FG-10 are added in phase 4.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::collections::BTreeMap;

use common::fleet_git::{Options, Rig, StationSetup};
use fleetdeck_proto::fleet_git::ConfigDigest;
use serde_json::json;

/// The items of both stations (the same four files below HOME), written
/// before the first start.
fn files() -> Vec<(String, Vec<u8>)> {
    [
        (".claude/CLAUDE.md", "rules\n"),
        (".claude/agents/review.md", "review\n"),
        (".claude/skills/lint/SKILL.md", "lint\n"),
        (".codex/AGENTS.md", "codex rules\n"),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect()
}

/// The hash of each item, by its name, of a digest.
fn hashes(digest: &ConfigDigest) -> BTreeMap<String, String> {
    digest
        .items
        .iter()
        .map(|item| (format!("{:?}", item.name), item.hash.clone()))
        .collect()
}

#[test]
fn drift_same_and_differs() {
    let setup = StationSetup {
        files: files(),
        ..StationSetup::default()
    };
    let mut rig = Rig::with(
        &["alpha", "bravo"],
        Options {
            setup: BTreeMap::from([
                ("alpha".to_owned(), setup.clone()),
                ("bravo".to_owned(), setup),
            ]),
            ..Options::default()
        },
    );
    let digest = |rig: &Rig, i: usize, after: i64| {
        let info = rig.wait_info("the config digest", i, |info| {
            info.config.as_ref().is_some_and(|c| c.computed_at > after)
        });
        info.config.unwrap()
    };

    // Equal homes: equal digests, and the files exist (no `absent`).
    let (a, b) = (digest(&rig, 0, 0), digest(&rig, 1, 0));
    assert_eq!(a.items.len(), 4);
    assert_eq!(hashes(&a), hashes(&b));
    for hash in hashes(&a).values() {
        assert_eq!(hash.len(), 64, "{hash}");
    }
    assert_eq!(a.claude_version, b.claude_version);

    // The drift reference is a preference of the deck.
    let alpha = rig.station_id(0);
    let r = rig.board(
        "PUT",
        "/api/fleet-git/drift-reference",
        Some(json!({ "station": alpha })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let prefs = rig.board("GET", "/api/fleet-git/prefs", None);
    assert_eq!(prefs.status, 200, "{}", prefs.body);
    assert_eq!(prefs.body["drift_reference"], alpha);

    // A new file in the skills of bravo: after its next digest (a restart
    // makes it at once), only the skills hash of bravo differs.
    let skill = rig.stations[1].station.node.home().join(".claude/skills");
    std::fs::create_dir_all(skill.join("deploy")).unwrap();
    std::fs::write(skill.join("deploy/SKILL.md"), "deploy\n").unwrap();
    rig.restart_station(1, &[]);
    let b2 = digest(&rig, 1, b.computed_at);
    let (ha, hb) = (hashes(&a), hashes(&b2));
    let differing: Vec<_> = ha.keys().filter(|k| ha[*k] != hb[*k]).collect();
    assert_eq!(differing, ["ClaudeSkills"], "{ha:?} {hb:?}");
    // The reference station is unchanged.
    assert_eq!(hashes(&digest(&rig, 0, 0)), ha);
}
