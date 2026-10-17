//! Fleet Git FG-8 on loopback (T3.5, design §11.12, owner acceptance A2): the
//! land flow from the board route to the bare repository. One deck, one
//! station, one bare `remote.git`. The station runs the real Git; a `git`
//! wrapper on its PATH can block one push (`common/fleet_git_land.rs`), and a
//! fake `glab` plays the forge. Every test lands a session through
//! `POST /api/sessions/{station}/{session}/land` and reads the row through
//! `GET /api/projects/{id}/lands`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::{Rig, commit_in, git};
use common::fleet_git_land::{
    self as fx, block_push, commit, forge, idle, kill_hung, land, land_row, options, publish_ref,
    pushes_of, release, restore_remote, unblock, wait_entered, wait_land, wait_state,
};
use fleetdeck_proto::fleet_git::PushState;
use serde_json::{Value, json};

/// A rig with one station and a project with `settings`.
fn one(settings: Value) -> (Rig, String) {
    let rig = Rig::with(&["alpha"], options(&["alpha"]));
    let project = rig.project(0, "App", settings);
    (rig, project)
}

fn id_of(s: &fleetdeck_proto::SessionSummary) -> &str {
    s.session_id.as_str()
}

/// Waits until `file` exists.
fn wait_file(file: &Path) {
    let deadline = Instant::now() + WAIT;
    while !file.exists() {
        assert!(Instant::now() < deadline, "no file {}", file.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_clean_merge_land_advances_main() {
    let (rig, project) = one(json!({}));
    let s = idle(&rig, 0);
    let c = commit(&s, "feature.txt", "feature\n");
    // `main` moves after the start, so the rebase rewrites the commit.
    let moved = rig.advance_remote("main.txt", "main moves");
    let land = land(&rig, 0, id_of(&s));
    let row = wait_state(&rig, &project, &land, "landed");

    let refs = rig.bare_refs();
    let main = refs["refs/heads/main"].clone();
    assert_ne!(main, c, "the land pushes the rebased commit");
    assert_eq!(row["rebased_oid"], main.as_str(), "{row}");
    assert_eq!(row["default_published_oid"], main.as_str(), "{row}");
    assert_eq!(row["source_oid"], c.as_str(), "{row}");
    assert_eq!(row["attempt"], 1);
    assert_eq!(git(&rig.remote, &["rev-parse", "main^"]), moved);
    assert_eq!(git(&rig.remote, &["show", "main:feature.txt"]), "feature");
    // The branch of the session is published with the same commit.
    assert_eq!(refs[&publish_ref(&s)], main);
    // The session worktree is never touched (O24).
    let work = PathBuf::from(s.worktree.clone().unwrap());
    assert_eq!(git(&work, &["rev-parse", "HEAD"]), c);
}

#[test]
fn a_conflict_stops_with_files_and_changes_nothing() {
    let (rig, project) = one(json!({}));
    let s = idle(&rig, 0);
    commit(&s, "README.md", "session\n");
    rig.advance_remote("README.md", "main change");
    let before = rig.bare_refs();
    let land = land(&rig, 0, id_of(&s));
    let row = wait_state(&rig, &project, &land, "conflict");

    assert_eq!(row["files"], json!(["README.md"]), "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.starts_with("Conflict in 1 files with origin/main ("),
        "{message}"
    );
    assert!(message.ends_with("Nothing was pushed."), "{message}");
    assert_eq!(rig.bare_refs(), before, "no ref of the bare repo changed");
}

#[test]
fn a_failed_check_stops_the_land() {
    let (rig, project) = one(json!({ "check_cmd": "echo checking; exit 3" }));
    let s = idle(&rig, 0);
    commit(&s, "feature.txt", "feature\n");
    let before = rig.bare_refs();
    let land = land(&rig, 0, id_of(&s));
    let row = wait_state(&rig, &project, &land, "check_failed");

    let message = row["message"].as_str().unwrap();
    assert!(
        message.starts_with("The check command failed (exit 3)."),
        "{message}"
    );
    assert!(message.contains("Nothing was pushed."), "{message}");
    assert_eq!(rig.bare_refs(), before, "no ref of the bare repo changed");
}

#[test]
fn a_pr_land_creates_one_pull_request() {
    let rig = Rig::with(&["alpha"], options(&["alpha"]));
    // The default mode is `pr` with a forge binding.
    let project = rig.project_with(0, "App", json!({}), forge(&rig));
    let s = idle(&rig, 0);
    commit(&s, "feature.txt", "feature\n");
    let main = rig.bare_refs()["refs/heads/main"].clone();
    let land = land(&rig, 0, id_of(&s));
    let row = wait_state(&rig, &project, &land, "landed");

    assert_eq!(row["pr_url"], "https://forge.example/mr/5", "{row}");
    assert_eq!(fx::log_lines(&rig, "creates"), 1, "exactly one create");
    let refs = rig.bare_refs();
    assert_eq!(refs[&publish_ref(&s)], row["rebased_oid"].as_str().unwrap());
    assert_eq!(refs["refs/heads/main"], main, "a PR land never moves main");
}

#[test]
fn a_restart_before_the_publication_ends_station_restarted() {
    let mut rig = Rig::with(&["alpha"], options(&["alpha"]));
    let started = fx::fx(&rig).join("check.started");
    let check = format!(
        "touch {}; i=0; while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done",
        started.display()
    );
    let project = rig.project(0, "App", json!({ "check_cmd": check }));
    let s = idle(&rig, 0);
    commit(&s, "feature.txt", "feature\n");
    let before = rig.bare_refs();
    let land = land(&rig, 0, id_of(&s));
    // The land sits in its check: nothing is published yet.
    wait_file(&started);
    wait_land(&rig, &project, &land, "the check", |r| {
        r["phase"] == "checking"
    });
    rig.kill_station(0);
    rig.start_station(0, &[]);
    let row = wait_state(&rig, &project, &land, "failed");

    assert_eq!(row["code"], "station_restarted", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("The station restarted"), "{message}");
    assert!(message.contains("Nothing was pushed."), "{message}");
    assert_eq!(rig.bare_refs(), before, "no ref of the bare repo changed");
}

/// A land whose push to `pattern` is applied and then hangs; the station is
/// killed and starts again. The reconciliation finds the applied push, and
/// the land ends without a second push.
fn restart_after_the_push(pattern: &str) {
    let (mut rig, project) = one(json!({}));
    let s = idle(&rig, 0);
    commit(&s, "feature.txt", "feature\n");
    block_push(&rig, "after", pattern);
    let land = land(&rig, 0, id_of(&s));
    wait_entered(&rig);
    rig.kill_station(0);
    kill_hung(&rig);
    unblock(&rig);
    rig.start_station(0, &[]);
    let row = wait_state(&rig, &project, &land, "landed");

    let refs = rig.bare_refs();
    let r = row["rebased_oid"].as_str().unwrap();
    assert_eq!(refs["refs/heads/main"], r, "{row}");
    assert_eq!(refs[&publish_ref(&s)], r, "{row}");
    assert_eq!(
        pushes_of(&rig, ":refs/heads/fd/"),
        1,
        "{:?}",
        fx::pushes(&rig)
    );
    assert_eq!(
        pushes_of(&rig, ":refs/heads/main"),
        1,
        "{:?}",
        fx::pushes(&rig)
    );
}

#[test]
fn a_restart_after_each_push_intent_reconciles_without_a_second_push() {
    // The branch push (the read finds the commit on the branch) ...
    restart_after_the_push(":refs/heads/fd/");
    // ... and the default push (the read finds it on `main`).
    restart_after_the_push(":refs/heads/main");
}

#[test]
fn a_second_attempt_conflict_names_the_published_branch() {
    let (rig, project) = one(json!({}));
    let s = idle(&rig, 0);
    commit(&s, "README.md", "session\n");
    // The default push waits; `main` moves under it with a conflicting edit.
    block_push(&rig, "fifo", ":refs/heads/main");
    let land = land(&rig, 0, id_of(&s));
    wait_entered(&rig);
    let moved = rig.advance_remote("README.md", "moved");
    unblock(&rig);
    release(&rig);
    let row = wait_state(&rig, &project, &land, "conflict");

    assert_eq!(row["attempt"], 2, "{row}");
    assert_eq!(row["files"], json!(["README.md"]), "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains(" on the second attempt."), "{message}");
    assert!(
        message.contains("The rebased branch fd/alpha/"),
        "{message}"
    );
    assert!(message.contains("did not change."), "{message}");
    assert!(!message.contains("Nothing was pushed."), "{message}");
    // The branch of attempt 1 stays on the remote; `main` is the other edit.
    let refs = rig.bare_refs();
    let published = row["branch_published_oid"].as_str().unwrap();
    assert_eq!(refs[&publish_ref(&s)], published, "{row}");
    assert_eq!(refs["refs/heads/main"], moved);
}

#[test]
fn two_lands_of_one_project_run_in_order() {
    let (rig, project) = one(json!({}));
    let a = idle(&rig, 0);
    let b = idle(&rig, 0);
    commit(&a, "a.txt", "a\n");
    commit(&b, "b.txt", "b\n");
    let first = land(&rig, 0, id_of(&a));
    let second = land(&rig, 0, id_of(&b));
    let first = wait_state(&rig, &project, &first, "landed");
    let second = wait_state(&rig, &project, &second, "landed");

    let (early, late) = if first["started_at"].as_i64() <= second["started_at"].as_i64() {
        (&first, &second)
    } else {
        (&second, &first)
    };
    assert!(
        late["started_at"].as_i64().unwrap() >= early["ended_at"].as_i64().unwrap(),
        "{early} then {late}"
    );
    // Both are on `main`, one after the other.
    let tree = git(&rig.remote, &["ls-tree", "--name-only", "main"]);
    assert!(tree.contains("a.txt") && tree.contains("b.txt"), "{tree}");
    assert_eq!(
        git(&rig.remote, &["rev-parse", "main"]),
        late["rebased_oid"].as_str().unwrap()
    );
}

#[test]
fn the_push_loop_never_pushes_the_old_tip_after_a_land() {
    let (rig, project) = one(json!({ "push": "on_turn" }));
    let s = idle(&rig, 0);
    let id = id_of(&s).to_owned();
    let work = PathBuf::from(s.worktree.clone().unwrap());
    let c1 = commit(&s, "one.txt", "one\n");
    rig.prompt(0, &id, "one");
    rig.wait_summary("the first push", 0, &id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&c1))
    });
    // `main` moves, so the land rebases `c1` and pushes another commit.
    rig.advance_remote("main.txt", "main moves");
    rig.wait_summary("idle", 0, &id, |s| s.current_turn.is_none());
    let land = land(&rig, 0, &id);
    let row = wait_state(&rig, &project, &land, "landed");
    let r = row["rebased_oid"].as_str().unwrap().to_owned();
    assert_ne!(r, c1);
    let done = rig.wait_summary("the push stops", 0, &id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Stopped)
    });
    assert_eq!(
        done.push.unwrap().stop_code.as_deref(),
        Some("land_published")
    );
    assert_eq!(rig.bare_refs()[&publish_ref(&s)], r);

    // The agent goes on with its old branch: a commit and a turn. The loop
    // must not push over the rebased branch.
    let c2 = commit_in(&work, "two.txt", "two\n", "two");
    rig.prompt(0, &id, "two");
    std::thread::sleep(Duration::from_millis(2500));
    let refs = rig.bare_refs();
    assert_eq!(refs[&publish_ref(&s)], r, "the rebased branch stays");
    assert!(!refs.values().any(|o| *o == c2), "the new tip is local");
    // The pushes to the session ref: the loop's `c1` and the land's `R`.
    let log = fx::pushes(&rig);
    let mine: Vec<_> = log
        .iter()
        .filter(|p| p.contains(&format!(":{}", publish_ref(&s))))
        .collect();
    assert_eq!(mine.len(), 2, "{log:?}");
    assert!(mine[1].contains(&r), "{log:?}");
}

/// A land of `s1` that sits in `branch_push_intent` with its push never
/// applied. The station was killed, the remote is gone, and the station is up
/// again: the reconciliation read fails, so the row says `reconciling`.
/// Returns the land and the place where the remote went.
fn reconciling_land(rig: &mut Rig, project: &str, s1: &str) -> (String, PathBuf) {
    block_push(rig, "before", ":refs/heads/fd/");
    let land = land(rig, 0, s1);
    wait_entered(rig);
    rig.kill_station(0);
    kill_hung(rig);
    unblock(rig);
    let gone = rig.remove_remote();
    rig.start_station(0, &[]);
    let row = wait_land(rig, project, &land, "reconciling", |r| {
        r["reconciling"] == true
    });
    assert_eq!(row["state"], "running", "{row}");
    assert_eq!(row["phase"], "branch_push_intent", "{row}");
    (land, gone)
}

#[test]
fn stop_checking_fences_a_land_on_a_dead_station() {
    let (mut rig, project) = one(json!({}));
    let s1 = idle(&rig, 0);
    let s2 = idle(&rig, 0);
    commit(&s1, "a.txt", "a\n");
    commit(&s2, "b.txt", "b\n");
    let (first, gone) = reconciling_land(&mut rig, &project, id_of(&s1));
    // A second land of the project waits behind the first.
    let second = land(&rig, 0, id_of(&s2));
    assert_eq!(
        land_row(&rig, &project, &second).unwrap()["state"],
        "queued"
    );

    // The station dies; the owner presses "Stop checking".
    rig.kill_station(0);
    let r = rig.board("POST", &format!("/api/lands/{first}/abandon"), None);
    assert_eq!(r.status, 202, "{}", r.body);
    std::thread::sleep(Duration::from_millis(1500));
    let row = land_row(&rig, &project, &first).unwrap();
    assert_eq!(row["state"], "running", "the queue stays occupied: {row}");
    assert_eq!(row["abandon_state"], "requested", "{row}");
    let queued = land_row(&rig, &project, &second).unwrap();
    assert_eq!(queued["state"], "queued", "{queued}");
    // The station is still paired, so the row cannot be forgotten.
    let r = rig.board("POST", &format!("/api/lands/{first}/forget"), None);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.body["error"], "station_paired", "{}", r.body);

    // The station comes back and acknowledges the fence.
    rig.start_station(0, &[]);
    let row = wait_state(&rig, &project, &first, "unresolved");
    assert_eq!(row["code"], "abandoned", "{row}");
    assert_eq!(row["abandon_state"], "fenced", "{row}");
    assert_eq!(row["abandon_write"], "none", "{row}");
    // Only now the second land is admitted (it fails: the remote is gone).
    let moved = wait_land(&rig, &project, &second, "the second land", |r| {
        r["state"] != "queued"
    });
    assert!(
        moved["started_at"].as_i64().unwrap() >= row["ended_at"].as_i64().unwrap(),
        "{row} then {moved}"
    );
    restore_remote(&rig, &gone);
}

#[test]
fn stop_checking_waits_for_the_running_reconciliation_push() {
    let (mut rig, project) = one(json!({}));
    let s1 = idle(&rig, 0);
    let s2 = idle(&rig, 0);
    commit(&s1, "a.txt", "a\n");
    commit(&s2, "b.txt", "b\n");
    let (first, gone) = reconciling_land(&mut rig, &project, id_of(&s1));
    let second = land(&rig, 0, id_of(&s2));

    // The remote is reachable again. The restart reconciles: the read finds
    // that the push did not apply, and the recovery push blocks on the FIFO.
    restore_remote(&rig, &gone);
    block_push(&rig, "fifo", ":refs/heads/fd/");
    rig.restart_station(0, &[]);
    wait_entered(&rig);

    // "Stop checking": the row stays `running` while the push blocks.
    let r = rig.board("POST", &format!("/api/lands/{first}/abandon"), None);
    assert_eq!(r.status, 202, "{}", r.body);
    std::thread::sleep(Duration::from_millis(1500));
    let row = land_row(&rig, &project, &first).unwrap();
    assert_eq!(row["state"], "running", "{row}");
    assert_eq!(row["abandon_state"], "requested", "{row}");
    let queued = land_row(&rig, &project, &second).unwrap();
    assert_eq!(queued["state"], "queued", "{queued}");

    // The push ends; the station answers `fenced` with the write `applied`.
    unblock(&rig);
    release(&rig);
    let row = wait_state(&rig, &project, &first, "unresolved");
    assert_eq!(row["code"], "abandoned", "{row}");
    assert_eq!(row["abandon_state"], "fenced", "{row}");
    assert_eq!(row["abandon_write"], "applied", "{row}");
    assert!(
        rig.bare_refs().contains_key(&publish_ref(&s1)),
        "the recovery push applied"
    );
    let main = rig.bare_refs()["refs/heads/main"].clone();

    // Only after that transaction the second land is admitted and lands.
    let late = wait_state(&rig, &project, &second, "landed");
    assert!(
        late["started_at"].as_i64().unwrap() >= row["ended_at"].as_i64().unwrap(),
        "{row} then {late}"
    );
    assert_ne!(rig.bare_refs()["refs/heads/main"], main);
}
