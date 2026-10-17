//! Fleet Git FG-1 on loopback (T1.7, owner acceptance A2): a sync start
//! fetches the remote first and starts from the fetched default branch; with
//! the remote gone it starts from the local `HEAD` and says so
//! (`workspace.base_stale`, O7, O8).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::fleet_git::{Rig, git};
use fleetdeck_proto::event::Event;
use serde_json::json;

#[tokio::test]
async fn an_old_local_head_starts_from_the_fetched_remote() {
    let rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    rig.project(0, "App", json!({}));
    let local_head = git(&home, &["rev-parse", "HEAD"]);

    // The remote moves on; the home does not fetch by itself.
    let remote_head = rig.advance_remote("two.txt", "two");
    assert_ne!(local_head, remote_head);
    assert_eq!(git(&home, &["rev-parse", "origin/main"]), local_head);

    let started = rig.start(0);
    let id = started.session_id.as_str();
    let s = rig.wait_summary("the base", 0, id, |s| s.base.is_some());
    assert_eq!(
        s.base.unwrap().oid,
        remote_head,
        "the fetched remote commit"
    );
    assert!(!s.base_stale);
    let branch = s.branch.unwrap();
    let callsign = s.callsign.unwrap();
    assert_eq!(branch, format!("fd/alpha/{callsign}"));
    // The home itself is untouched, but its remote-tracking ref moved.
    assert_eq!(git(&home, &["rev-parse", "HEAD"]), local_head);
    assert_eq!(git(&home, &["rev-parse", "origin/main"]), remote_head);
    // The worktree is on the new branch at the fetched commit.
    let worktree = s.worktree.unwrap();
    assert_eq!(
        git(std::path::Path::new(&worktree), &["rev-parse", "HEAD"]),
        remote_head
    );
}

#[tokio::test]
async fn a_gone_remote_starts_from_head_with_base_stale() {
    let mut rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    rig.project(0, "App", json!({}));
    let local_head = git(&home, &["rev-parse", "HEAD"]);
    rig.remove_remote();

    let started = rig.start(0);
    let id = started.session_id.as_str().to_owned();
    let s = rig.wait_summary("the base", 0, &id, |s| s.base.is_some() && s.base_stale);
    assert_eq!(s.base.unwrap().oid, local_head, "the local HEAD");
    assert!(s.branch.unwrap().starts_with("fd/alpha/"));

    let events = rig.events_after_stop(0).await;
    let stale: Vec<_> = events
        .iter()
        .filter(|(session, _)| *session == id)
        .filter_map(|(_, e)| match e {
            Event::WorkspaceBaseStale { code, text } => Some((code.clone(), text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(stale.len(), 1, "{stale:?}");
    assert_eq!(stale[0].0, "fetch_failed");
    assert!(
        stale[0].1.contains("started from the local HEAD"),
        "{stale:?}"
    );
}
