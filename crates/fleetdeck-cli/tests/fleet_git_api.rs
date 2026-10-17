//! Fleet Git API tests on loopback (T2.5, T4.6, design §13.1, §14.4, owner
//! acceptance A2 FG-10, FG-11): the branches and fetch routes of the board and
//! the config drift report. The coordinator tools of FG-10 are in
//! `fleet_git_coordinator.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::collections::BTreeMap;

use common::fleet_git::{Options, Rig, StationSetup};
use common::fleet_git_coord::{project_with_homes, pushed_session};
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

/// `GET /api/projects/{id}/branches`.
fn branches(rig: &Rig, project: &str) -> serde_json::Value {
    let r = rig.board("GET", &format!("/api/projects/{project}/branches"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

/// `POST /api/projects/{id}/fetch`.
fn fetch(rig: &Rig, project: &str) -> serde_json::Value {
    let path = format!("/api/projects/{project}/fetch");
    let r = rig.board("POST", &path, Some(json!({})));
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

// Two pushed sessions on bravo and charlie, the radar home on alpha: after one
// fetch the route lists both branches with their stations, sessions, ahead and
// behind (design 13.1).
#[test]
fn branches_route_matches_the_design() {
    let rig = Rig::new(&["alpha", "bravo", "charlie"]);
    let (project, folders) = project_with_homes(&rig, 3, json!({ "push": "on_turn" }));
    let (b, ob) = pushed_session(&rig, 1, &folders[1], &project, "b.txt");
    let (c, oc) = pushed_session(&rig, 2, &folders[2], &project, "c.txt");
    // `main` moves after the sessions started: each branch is behind by one.
    let main = rig.advance_remote("main.txt", "main moves");

    let before = now_ms();
    let fetched = fetch(&rig, &project);
    assert!(fetched.get("code").is_none_or(|c| c.is_null()), "{fetched}");
    let at = fetched["fetched_at"].as_i64().unwrap();
    assert!(at >= before, "{fetched}");

    let view = branches(&rig, &project);
    assert_eq!(view["remote"], "origin", "{view}");
    assert_eq!(view["default_branch"], "main", "{view}");
    assert_eq!(view["default_oid"], main.as_str(), "{view}");
    assert_eq!(view["fetched_at"].as_i64(), Some(at), "{view}");
    assert_eq!(view["truncated"], false, "{view}");
    let rows = view["branches"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{view}");
    for (i, s, oid, file) in [(1, &b, &ob, "b.txt"), (2, &c, &oc, "c.txt")] {
        let name = s.branch.clone().unwrap();
        let row = rows
            .iter()
            .find(|r| r["name"] == name.as_str())
            .unwrap_or_else(|| panic!("no row {name}: {view}"));
        let station = rig.stations[i].name.as_str();
        let callsign = s.callsign.clone().unwrap();
        assert_eq!(row["slug"], station, "{row}");
        assert_eq!(row["callsign"], callsign.as_str(), "{row}");
        assert_eq!(row["head"], oid.as_str(), "{row}");
        assert_eq!(row["ahead"], 1, "{row}");
        assert_eq!(row["behind"], 1, "{row}");
        assert_eq!(row["subject"], file, "{row}");
        assert_eq!(row["legacy"], false, "{row}");
        assert_eq!(row["station_id"], rig.station_id(i).as_str(), "{row}");
        assert_eq!(row["station_name"], station, "{row}");
        assert_eq!(row["session"]["session_id"], s.session_id.as_str(), "{row}");
        assert_eq!(
            row["session"]["station_id"],
            rig.station_id(i).as_str(),
            "{row}"
        );
        assert_eq!(row["conflict"], "none", "{row}");
    }
}

// `POST fetch` stamps `fetched_at`, and the next listing shows the branch that
// was pushed before it. The radar of the radar home also fetches (and stamps
// the time) on its own schedule, so the test checks only what holds with it:
// the time never goes back, and a fetch makes every pushed branch visible.
// That the listing itself runs no fetch is a station unit test (T4.3).
#[test]
fn fetch_route_stamps_fetched_at() {
    let rig = Rig::new(&["alpha", "bravo", "charlie"]);
    let (project, folders) = project_with_homes(&rig, 3, json!({ "push": "on_turn" }));
    let (b, _) = pushed_session(&rig, 1, &folders[1], &project, "b.txt");
    let first = fetch(&rig, &project)["fetched_at"].as_i64().unwrap();
    let view = branches(&rig, &project);
    assert!(view["fetched_at"].as_i64().unwrap() >= first, "{view}");
    let name_b = b.branch.clone().unwrap();
    let has = |view: &serde_json::Value, name: &str| {
        view["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == name)
    };
    assert!(has(&view, &name_b), "{view}");

    let (c, _) = pushed_session(&rig, 2, &folders[2], &project, "c.txt");
    let second = fetch(&rig, &project)["fetched_at"].as_i64().unwrap();
    assert!(second > first, "{first} then {second}");
    let view = branches(&rig, &project);
    assert!(view["fetched_at"].as_i64().unwrap() >= second, "{view}");
    assert!(has(&view, &name_b), "{view}");
    assert!(has(&view, &c.branch.clone().unwrap()), "{view}");
}
