//! Fleet Git FG-2 on loopback (T1.7, owner acceptance A2): the station pushes
//! only `refs/heads/fd/<own-slug>/*` of its own sessions, with the last
//! pushed commit as the lease (O10), and a failed push retries with backoff
//! (O11). One deck, two stations, one bare repository.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::PathBuf;

use common::fleet_git::{Options, Rig, commit_in, git};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::fleet_git::PushState;
use serde_json::json;

/// Starts a worktree session on station `i` and waits until its first turn
/// is over, so that the next turn is the one that a test triggers.
fn start_idle(rig: &Rig, i: usize) -> SessionSummary {
    let id = rig.start(i).session_id.as_str().to_owned();
    rig.wait_summary("the first turn", i, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

fn worktree_of(s: &SessionSummary) -> PathBuf {
    PathBuf::from(s.worktree.clone().unwrap())
}

/// The ref that the station publishes for the session.
fn ref_of(s: &SessionSummary) -> String {
    format!("refs/heads/{}", s.branch.clone().unwrap())
}

/// Commits in the worktree, then runs one turn (the `on_turn` trigger).
fn commit_and_turn(rig: &Rig, i: usize, s: &SessionSummary, file: &str) -> String {
    let oid = commit_in(&worktree_of(s), file, file, file);
    rig.prompt(i, s.session_id.as_str(), file);
    oid
}

fn wait_pushed(rig: &Rig, i: usize, s: &SessionSummary, oid: &str) -> SessionSummary {
    let want = oid.to_owned();
    rig.wait_summary("the push", i, s.session_id.as_str(), move |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&want))
    })
}

fn on_turn() -> serde_json::Value {
    json!({ "push": "on_turn" })
}

#[test]
fn two_stations_with_one_callsign_push_without_collision() {
    // The test seam of the station (feature `fault-injection`) makes both
    // stations pick the same callsign for their first session.
    let rig = Rig::with(
        &["alpha", "bravo"],
        Options {
            station_env: vec![("FLEETDECK_TEST_CALLSIGN".to_owned(), "falcon".to_owned())],
            ..Options::default()
        },
    );
    rig.project(0, "App alpha", on_turn());
    rig.project(1, "App bravo", on_turn());

    let a = start_idle(&rig, 0);
    let b = start_idle(&rig, 1);
    assert_eq!(a.callsign.as_deref(), Some("falcon"));
    assert_eq!(b.callsign.as_deref(), Some("falcon"));
    assert_eq!(a.branch.as_deref(), Some("fd/alpha/falcon"));
    assert_eq!(b.branch.as_deref(), Some("fd/bravo/falcon"));

    let oa = commit_and_turn(&rig, 0, &a, "a.txt");
    let ob = commit_and_turn(&rig, 1, &b, "b.txt");
    wait_pushed(&rig, 0, &a, &oa);
    wait_pushed(&rig, 1, &b, &ob);

    let refs = rig.bare_refs();
    assert_eq!(refs["refs/heads/fd/alpha/falcon"], oa);
    assert_eq!(refs["refs/heads/fd/bravo/falcon"], ob);
    let under_fd: Vec<_> = refs.keys().filter(|r| r.contains("/fd/")).collect();
    assert_eq!(
        under_fd,
        ["refs/heads/fd/alpha/falcon", "refs/heads/fd/bravo/falcon"],
        "{refs:?}"
    );
}

#[test]
fn a_revert_then_a_push_succeeds_with_the_lease() {
    let rig = Rig::new(&["alpha"]);
    rig.project(0, "App", on_turn());
    let s = start_idle(&rig, 0);
    let work = worktree_of(&s);
    let base = s.base.clone().unwrap().oid;

    let c1 = commit_and_turn(&rig, 0, &s, "one.txt");
    wait_pushed(&rig, 0, &s, &c1);
    assert_eq!(rig.bare_refs()[&ref_of(&s)], c1);

    // A revert rewrites the history: back to the base, then another commit
    // that is no descendant of `c1`. Only a push over the last pushed commit
    // (the lease) can take it.
    git(&work, &["reset", "-q", "--hard", &base]);
    let c2 = commit_and_turn(&rig, 0, &s, "two.txt");
    assert_ne!(c1, c2);
    git(&work, &["merge-base", "--is-ancestor", &base, &c2]);
    assert!(
        !common::fleet_git::git_out(&work, &["merge-base", "--is-ancestor", &c1, &c2])
            .status
            .success()
    );
    let done = wait_pushed(&rig, 0, &s, &c2);
    assert_eq!(done.push.unwrap().attempts, 0);
    assert_eq!(rig.bare_refs()[&ref_of(&s)], c2);
}

#[test]
fn a_stale_lease_fails_and_retries() {
    let rig = Rig::new(&["alpha"]);
    rig.project(0, "App", on_turn());
    let s = start_idle(&rig, 0);
    let work = worktree_of(&s);
    let id = s.session_id.as_str().to_owned();

    let c1 = commit_and_turn(&rig, 0, &s, "one.txt");
    wait_pushed(&rig, 0, &s, &c1);

    // Somebody else moves the ref: the lease of the station is stale.
    let other = rig.advance_remote("other.txt", "other");
    git(&rig.remote, &["update-ref", &ref_of(&s), &other, &c1]);
    let c2 = commit_and_turn(&rig, 0, &s, "two.txt");
    let failing = rig.wait_summary("the failed push", 0, &id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Failing && p.attempts >= 1)
    });
    let push = failing.push.unwrap();
    assert_eq!(push.code.as_deref(), Some("push_rejected"), "{push:?}");
    assert_eq!(push.desired_oid.as_deref(), Some(c2.as_str()));
    // The backoff is visible: the next try is in the future (1 minute).
    let next = push.next_attempt_at.expect("a next attempt");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(next > now, "{next} > {now}");
    assert_eq!(rig.bare_refs()[&ref_of(&s)], other, "nothing was forced");
    assert!(git(&work, &["rev-parse", "HEAD"]) == c2);

    // The ref goes back to the commit that the station pushed; "Push now"
    // makes the retry due at once and it succeeds with the same lease.
    git(&rig.remote, &["update-ref", &ref_of(&s), &c1, &other]);
    let r = rig.board(
        "POST",
        &format!("/api/sessions/alpha/{id}/push"),
        Some(json!({})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let done = wait_pushed(&rig, 0, &s, &c2);
    assert_eq!(done.push.unwrap().attempts, 0);
    assert_eq!(rig.bare_refs()[&ref_of(&s)], c2);
}

#[test]
fn no_ref_outside_the_own_slug_is_pushed() {
    let rig = Rig::new(&["alpha"]);
    rig.project(0, "App", on_turn());
    let home = rig.stations[0].home.clone();
    // Refs of the home that are not session branches.
    git(&home, &["branch", "side"]);
    git(&home, &["tag", "v1"]);
    commit_in(&home, "home.txt", "home", "home work");

    // A session on an owner branch is never pushed (O10).
    let owner = rig.start_with(
        0,
        json!({ "kind": "worktree", "branch": "feature-x" }),
        serde_json::Value::Null,
    );
    rig.wait_summary("the owner branch", 0, owner.session_id.as_str(), |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    });
    let s = start_idle(&rig, 0);
    let c = commit_and_turn(&rig, 0, &s, "own.txt");
    wait_pushed(&rig, 0, &s, &c);
    // The owner session ends a turn too; its commit stays local.
    let owner_work = PathBuf::from(rig.summary(0, owner.session_id.as_str()).worktree.unwrap());
    let oc = commit_in(&owner_work, "owner.txt", "owner", "owner work");
    rig.prompt(0, owner.session_id.as_str(), "owner turn");
    // A second turn of the own session gives the push loop time to act on
    // any wrong row.
    let c2 = commit_and_turn(&rig, 0, &s, "own2.txt");
    wait_pushed(&rig, 0, &s, &c2);

    let refs = rig.bare_refs();
    let own = format!("refs/heads/{}", s.branch.clone().unwrap());
    for name in refs.keys() {
        assert!(
            name == "refs/heads/main" || name == &own,
            "an unexpected ref {name}: {refs:?}"
        );
    }
    assert_eq!(refs[&own], c2);
    assert!(
        !refs.values().any(|o| *o == oc),
        "the owner commit is local"
    );
}
