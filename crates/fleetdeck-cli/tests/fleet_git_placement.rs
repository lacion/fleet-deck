//! Fleet Git FG-6 on loopback (T2.5, design §9.5, owner acceptance A2):
//! placement by station traits through `POST /api/placement`. Station A has
//! the label `gpu`, B has `always_on`, C has none.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::fleet_git::{Options, Rig, StationSetup};
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

fn labelled(labels: &str) -> StationSetup {
    StationSetup {
        toml: format!("traits = [{labels}]\n"),
        ..StationSetup::default()
    }
}

/// A provider program that answers `auth status` as signed in (the fake
/// Claude of the `sim` scenario fails it, and a placement needs a signed-in
/// provider) and runs the fake Claude for everything else.
fn signed_in_program(dir: &Path) -> PathBuf {
    let fake = FakeAgent::Claude.locate().unwrap();
    let script = dir.join("provider.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = auth ]; then\n  echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}}'\n  exit 0\nfi\nexec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Stations `alpha` (`gpu`), `bravo` (`always_on`) and `charlie` (none), all
/// reachable with their traits and a signed-in provider in `station.info`.
fn rig(dir: &Path) -> Rig {
    let rig = Rig::with(
        &["alpha", "bravo", "charlie"],
        Options {
            provider: Some(signed_in_program(dir)),
            setup: BTreeMap::from([
                ("alpha".to_owned(), labelled("\"gpu\"")),
                ("bravo".to_owned(), labelled("\"always_on\"")),
            ]),
            ..Options::default()
        },
    );
    for i in 0..3 {
        let info = rig.wait_info("the traits", i, |info| {
            info.traits.is_some() && info.providers.iter().any(|p| p.auth == "signed_in")
        });
        let labels = info.traits.unwrap().labels;
        let want: &[&str] = [&["gpu"][..], &["always_on"][..], &[][..]][i];
        assert_eq!(labels, want, "station {i}");
    }
    rig
}

/// `POST /api/placement`; the station ids of the answer, as names.
fn place(rig: &Rig, body: Value) -> Vec<String> {
    let r = rig.board("POST", "/api/placement", Some(body));
    assert_eq!(r.status, 200, "{}", r.body);
    r.body["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            let name = (0..3).find(|i| rig.station_id(*i) == id.as_str().unwrap());
            rig.stations[name.expect("a known station")].name.clone()
        })
        .collect()
}

#[test]
fn requires_places_on_the_labelled_station() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(dir.path());
    assert_eq!(
        place(&rig, json!({ "count": 1, "requires": ["gpu"] })),
        ["alpha"]
    );
    // The label is a requirement, not a preference: no other station fits.
    let r = rig.board(
        "POST",
        "/api/placement",
        Some(json!({ "count": 1, "requires": ["tpu"] })),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.body["error"], "no_station");
    assert_eq!(r.body["message"], "No station matches: tpu.");
    // A real start on the placed station.
    let s = rig.start(0);
    assert!(s.worktree.is_some(), "{s:?}");
}

#[test]
fn prefer_always_on_wins() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(dir.path());
    assert_eq!(
        place(&rig, json!({ "count": 1, "prefer": ["always_on"] })),
        ["bravo"]
    );
}

#[test]
fn spread_uses_three_stations() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(dir.path());
    let mut names = place(&rig, json!({ "count": 3, "spread": true }));
    names.sort();
    assert_eq!(names, ["alpha", "bravo", "charlie"]);
}
