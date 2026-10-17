//! Fleet Git FG-10 on loopback (T4.6, design §13.2, §13.3, owner acceptance A2
//! FG-10, revision 3 M14): a coordinator session on station alpha with a fleet
//! grant over alpha, bravo and charlie calls the tools of the architect across
//! the fleet over the real MCP endpoint of its station. `project_branches` and
//! `project_fetch` answer as the board routes; `land_session` lands a bravo
//! session in merge mode; a revocation before the admission of a queued land
//! ends it with `authority_lost` and changes no ref; `move_session` is in the
//! catalog (its moves are tested in `fleet_git_move.rs`). The harness is in
//! `common/fleet_git_coord.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::Rig;
use common::fleet_git_coord::{
    Coord, fenced_subject, idle_in, options, project_with_homes, pushed_session,
    wait_deck_caught_up, wait_deck_knows,
};
use common::fleet_git_land::{
    block_push, commit, land, pushes_of, release, unblock, wait_entered, wait_land, wait_state,
};
use serde_json::{Value, json};

/// A rig with alpha, bravo and charlie, a project with a home on each, and a
/// coordinator on alpha whose grant (`inspect`, `manage`) covers all three.
fn world(settings: Value) -> (Rig, tempfile::TempDir, String, Vec<PathBuf>, Coord) {
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::with(
        &["alpha", "bravo", "charlie"],
        options(dir.path(), &["alpha", "bravo", "charlie"]),
    );
    let (project, folders) = project_with_homes(&rig, 3, settings);
    let coord = Coord::start(&rig, dir.path(), 0, &["inspect", "manage"]);
    coord.widen(&mut rig, &[0, 1, 2]);
    (rig, dir, project, folders, coord)
}

fn id_of(s: &fleetdeck_proto::SessionSummary) -> &str {
    s.session_id.as_str()
}

/// The id of the land of the project that is not `not` (waits for it).
fn other_land(rig: &Rig, project: &str, not: &[&str]) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let r = rig.board("GET", &format!("/api/projects/{project}/lands"), None);
        assert_eq!(r.status, 200, "{}", r.body);
        let found = ["queue", "recent"]
            .iter()
            .flat_map(|k| r.body[k].as_array().cloned().unwrap_or_default())
            .map(|row| row["land_id"].as_str().unwrap().to_owned())
            .find(|id| !not.contains(&id.as_str()));
        if let Some(id) = found {
            return id;
        }
        assert!(Instant::now() < deadline, "no new land: {}", r.body);
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn coordinator_project_branches_and_fetch() {
    let (rig, _dir, project, folders, coord) = world(json!({ "push": "on_turn" }));
    let (_b, _) = pushed_session(&rig, 1, &folders[1], &project, "b.txt");
    let (_c, _) = pushed_session(&rig, 2, &folders[2], &project, "c.txt");
    rig.advance_remote("main.txt", "main moves");

    // `project_fetch` (inspect) fetches on the radar home and answers
    // `fetched_at`.
    let fetched = coord
        .call("project_fetch", json!({ "project_id": project }))
        .json();
    assert_eq!(fetched["project_id"], project.as_str(), "{fetched}");
    assert!(fetched.get("code").is_none(), "{fetched}");
    let at = fetched["fetched_at"].as_i64().unwrap();

    // `project_branches` is the board route with the Git text fenced and no
    // session title.
    let route = rig.board("GET", &format!("/api/projects/{project}/branches"), None);
    assert_eq!(route.status, 200, "{}", route.body);
    let mut want = route.body.clone();
    assert_eq!(want["fetched_at"].as_i64(), Some(at), "{want}");
    assert_eq!(want["branches"].as_array().unwrap().len(), 2, "{want}");
    for row in want["branches"].as_array_mut().unwrap() {
        let subject = fenced_subject(
            &project,
            row["name"].as_str().unwrap(),
            row["subject"].as_str().unwrap(),
        );
        row["subject"] = json!(subject);
        row["session"].as_object_mut().unwrap().remove("title");
    }
    let got = coord
        .call("project_branches", json!({ "project_id": project }))
        .json();
    assert_eq!(got, want);
}

#[test]
fn coordinator_land_session_lands() {
    let (rig, _dir, project, folders, coord) = world(json!({}));
    let s = idle_in(&rig, 1, &folders[1], &project);
    wait_deck_knows(&rig, &[&s]);
    let c = commit(&s, "feature.txt", "feature\n");
    let moved = rig.advance_remote("main.txt", "main moves");

    let answer = coord.call(
        "land_session",
        json!({ "station": rig.station_id(1), "session_id": id_of(&s) }),
    );
    eprintln!("land_session answered: {}", answer.text);
    assert!(!answer.is_error, "{}", answer.text);
    let land_id = other_land(&rig, &project, &[]);
    let row = wait_state(&rig, &project, &land_id, "landed");

    let refs = rig.bare_refs();
    let main = refs["refs/heads/main"].clone();
    assert_ne!(main, c, "the land pushes the rebased commit");
    assert_eq!(row["rebased_oid"], main.as_str(), "{row}");
    assert_eq!(row["source_oid"], c.as_str(), "{row}");
    assert_eq!(
        common::fleet_git::git(&rig.remote, &["rev-parse", "main^"]),
        moved
    );
    assert_eq!(
        common::fleet_git::git(&rig.remote, &["show", "main:feature.txt"]),
        "feature"
    );
}

// A deck that restarts with its cursor at the head of the station log gets no
// event on the new link, so it sends no `ack`. The cursor of the replay
// acknowledges the events, and the coordinator call leaves at once (it does
// not stay `pending` behind the lifecycle fence).
#[test]
fn a_caught_up_deck_restart_does_not_fence_a_coordinator_call() {
    let (mut rig, _dir, project, folders, coord) = world(json!({}));
    let s = idle_in(&rig, 1, &folders[1], &project);
    wait_deck_knows(&rig, &[&s]);
    commit(&s, "feature.txt", "feature\n");
    wait_deck_caught_up(&rig, 0);
    rig.restart_deck_with(|_| {});

    let args = json!({
        "station": rig.station_id(1),
        "session_id": id_of(&s),
        "idempotency_key": "land-after-restart",
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let answer = loop {
        let answer = coord.call("land_session", args.clone());
        eprintln!("land_session answered: {}", answer.text);
        assert!(!answer.is_error, "{}", answer.text);
        if !answer.text.contains("\"pending\"") || Instant::now() > deadline {
            break answer;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(answer.json()["state"], "accepted", "{}", answer.text);
}

#[test]
fn move_session_is_in_the_catalog() {
    let (_rig, _dir, _project, _folders, coord) = world(json!({}));
    let tools = coord.tools();
    for name in [
        "project_branches",
        "project_fetch",
        "land_session",
        "move_session",
    ] {
        assert!(tools.iter().any(|t| t == name), "{name} in {tools:?}");
    }
}

// A land of the owner sits in its push to `main`. The coordinator queues a
// second land behind it; the owner revokes the grant. When the first land ends,
// the deck checks the stored authority of the second one at its admission: it
// ends `authority_lost`, and nothing of it reaches the bare repository.
#[test]
fn authority_loss_before_admission_of_a_queued_land() {
    let (rig, _dir, project, folders, coord) = world(json!({}));
    let s1 = idle_in(&rig, 1, &folders[1], &project);
    let s2 = idle_in(&rig, 1, &folders[1], &project);
    wait_deck_knows(&rig, &[&s1, &s2]);
    commit(&s1, "a.txt", "a\n");
    commit(&s2, "b.txt", "b\n");

    block_push(&rig, "fifo", ":refs/heads/main");
    let first = land(&rig, 1, id_of(&s1));
    wait_entered(&rig);

    // A call can stay `pending` while the link of the widened grant comes
    // back; the same idempotency key reads the row again without a new send.
    let args = json!({
        "station": rig.station_id(1),
        "session_id": id_of(&s2),
        "idempotency_key": "land-s2",
    });
    let deadline = Instant::now() + Duration::from_secs(90);
    let answer = loop {
        let answer = coord.call("land_session", args.clone());
        eprintln!("land_session answered: {}", answer.text);
        assert!(!answer.is_error, "{}", answer.text);
        if !answer.text.contains("\"pending\"") || Instant::now() > deadline {
            break answer;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(!answer.text.contains("\"pending\""), "{}", answer.text);
    let second = other_land(&rig, &project, &[&first]);
    let queued = wait_land(&rig, &project, &second, "queued", |r| {
        r["state"] == "queued"
    });
    assert_eq!(queued["state"], "queued", "{queued}");

    let r = rig.board(
        "POST",
        &format!("/api/coordinator/grants/{}/revoke", coord.grant_id),
        Some(json!({})),
    );
    assert_eq!(r.status, 200, "{}", r.body);

    unblock(&rig);
    release(&rig);
    let first = wait_state(&rig, &project, &first, "landed");
    let lost = wait_land(&rig, &project, &second, "authority_lost", |r| {
        r["code"] == "authority_lost"
    });
    assert_ne!(lost["state"], "landed", "{lost}");

    // Only the first land changed the bare repository.
    let refs = rig.bare_refs();
    assert_eq!(
        refs["refs/heads/main"],
        first["rebased_oid"].as_str().unwrap(),
        "{first}"
    );
    assert_eq!(pushes_of(&rig, ":refs/heads/main"), 1);
    let published = format!("refs/heads/{}", s2.branch.clone().unwrap());
    assert!(!refs.contains_key(&published), "{refs:?}");
}
