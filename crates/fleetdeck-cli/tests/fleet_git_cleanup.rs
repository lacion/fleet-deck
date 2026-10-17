//! Fleet Git FG-3 on loopback (T1.7, owner acceptance A2): a merged, clean,
//! pushed worktree of an ended session, old enough, goes to the trash; each
//! condition of O13, when false, keeps it. The cleanup loop runs through the
//! test seam of design decision 84 (station feature `fault-injection`, which
//! the test build enables): `FLEETDECK_TEST_CLEANUP_DELAY_MS`,
//! `FLEETDECK_TEST_CLEANUP_INTERVAL_MS` and `FLEETDECK_TEST_CLOCK_OFFSET_MS`
//! (25 hours, so a session that ended a second ago is "old").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::{Path, PathBuf};

use common::fleet_git::{Options, Rig, commit_in, git};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::fleet_git::{CleanupState, PushState};
use serde_json::{Value, json};

/// 25 hours in milliseconds.
const OFFSET_25H: &str = "90000000";

fn rig(offset: &str) -> Rig {
    let env = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    Rig::with(
        &["alpha"],
        Options {
            station_env: vec![
                env("FLEETDECK_TEST_CLEANUP_DELAY_MS", "300"),
                env("FLEETDECK_TEST_CLEANUP_INTERVAL_MS", "500"),
                env("FLEETDECK_TEST_CLOCK_OFFSET_MS", offset),
            ],
            ..Options::default()
        },
    )
}

fn settings() -> Value {
    json!({ "push": "on_exit", "cleanup": "merged", "cleanup_after_hours": 24 })
}

/// A live session whose first turn is over; the loop skips a live session.
fn live(rig: &Rig) -> SessionSummary {
    let id = rig.start(0).session_id.as_str().to_owned();
    rig.wait_summary("the first turn", 0, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

fn worktree_of(s: &SessionSummary) -> PathBuf {
    PathBuf::from(s.worktree.clone().unwrap())
}

/// Commits in the worktree and moves `main` of the remote to the commit.
fn commit_and_merge(work: &Path) -> String {
    let oid = commit_in(work, "feature.txt", "feature", "feature");
    git(work, &["push", "-q", "origin", "HEAD:main"]);
    oid
}

/// Waits until the cleanup state of the session satisfies `f`.
fn wait_cleanup(
    rig: &Rig,
    id: &str,
    f: impl Fn(&fleetdeck_proto::fleet_git::CleanupInfo) -> bool,
) -> fleetdeck_proto::fleet_git::CleanupInfo {
    rig.wait_summary("the cleanup state", 0, id, |s| {
        s.cleanup.as_ref().is_some_and(&f)
    })
    .cleanup
    .unwrap()
}

/// The session ends; the cleanup keeps the worktree for `reason`.
fn kept(rig: &Rig, s: &SessionSummary, reason: &str) {
    let id = s.session_id.as_str();
    rig.stop_session(0, id);
    let info = wait_cleanup(rig, id, |c| c.state == CleanupState::Kept);
    assert_eq!(info.reason.as_deref(), Some(reason), "{info:?}");
    // The folder is still there, and it stays so on the next runs.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(worktree_of(s).is_dir(), "the worktree was moved");
    assert_eq!(
        rig.summary(0, id).cleanup.unwrap().state,
        CleanupState::Kept
    );
}

#[test]
fn a_merged_clean_pushed_old_worktree_goes_to_the_trash() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    let id = s.session_id.as_str().to_owned();
    let work = worktree_of(&s);
    let branch = s.branch.clone().unwrap();
    let c = commit_and_merge(&work);

    rig.stop_session(0, &id);
    // The `on_exit` push comes first; then the loop takes the worktree.
    rig.wait_summary("the push", 0, &id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&c))
    });
    wait_cleanup(&rig, &id, |c| c.state == CleanupState::Trashed);
    assert!(!work.exists(), "the folder moved to the trash");
    let trash = work.parent().unwrap().join(".fleetdeck-trash");
    assert!(trash.is_dir(), "the trash folder exists");
    // The branch is never deleted.
    let home = &rig.stations[0].home;
    let tip = git(
        home,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    );
    assert_eq!(tip, c);
}

#[test]
fn too_new_keeps_it() {
    let rig = rig("0");
    rig.project(0, "App", settings());
    let s = live(&rig);
    let id = s.session_id.as_str().to_owned();
    commit_and_merge(&worktree_of(&s));
    rig.stop_session(0, &id);
    let info = wait_cleanup(&rig, &id, |c| c.state == CleanupState::Waiting);
    // The reason of `waiting` is the due time in unix ms, a day away.
    let due: i64 = info.reason.unwrap().parse().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(due > now + 23 * 3_600_000, "{due} vs {now}");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(worktree_of(&s).is_dir());
}

#[test]
fn not_merged_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    // A commit that is not on the remote default branch.
    commit_in(&worktree_of(&s), "local.txt", "local", "local");
    kept(&rig, &s, "not_merged");
}

#[test]
fn untracked_file_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    commit_and_merge(&worktree_of(&s));
    std::fs::write(worktree_of(&s).join("junk.txt"), "junk").unwrap();
    kept(&rig, &s, "dirty");
}

#[test]
fn unpushed_commit_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    let work = worktree_of(&s);
    let base = s.base.clone().unwrap().oid;
    commit_and_merge(&work);
    // The branch of the session exists on the remote at another commit, so
    // the station's first push (an empty lease) is rejected for good.
    git(
        &rig.remote,
        &[
            "update-ref",
            &format!("refs/heads/{}", s.branch.clone().unwrap()),
            &base,
        ],
    );
    kept(&rig, &s, "not_pushed");
    let push = rig.summary(0, s.session_id.as_str()).push.unwrap();
    assert_ne!(push.state, PushState::Pushed, "{push:?}");
}

#[test]
fn a_trash_check_refusal_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", json!({ "push": "off", "cleanup": "merged" }));
    let s = live(&rig);
    let work = worktree_of(&s);
    commit_and_merge(&work);
    // A second session is live in the same worktree: the trash flow refuses.
    let other = rig.start_in(0, &work, Value::Null, Value::Null);
    rig.wait_summary("the second session", 0, other.session_id.as_str(), |s| {
        s.current_turn.is_none() && s.usage.input_tokens.is_some()
    });
    let id = s.session_id.as_str();
    rig.stop_session(0, id);
    let info = wait_cleanup(&rig, id, |c| c.state == CleanupState::Kept);
    let reason = info.reason.unwrap();
    assert!(reason.starts_with("trash_refused: "), "{reason}");
    assert!(reason.contains("is live in the same worktree"), "{reason}");
    assert!(work.is_dir());
}

#[test]
fn a_branch_switch_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    let work = worktree_of(&s);
    commit_and_merge(&work);
    git(&work, &["switch", "-q", "-c", "other"]);
    kept(&rig, &s, "branch_changed");
}

#[test]
fn a_detached_head_keeps_it() {
    let rig = rig(OFFSET_25H);
    rig.project(0, "App", settings());
    let s = live(&rig);
    let work = worktree_of(&s);
    commit_and_merge(&work);
    git(&work, &["checkout", "-q", "--detach"]);
    kept(&rig, &s, "detached");
}
