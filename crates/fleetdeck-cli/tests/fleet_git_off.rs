//! Fleet Git A4 on loopback (T1.7, O1): a project with `sync = false` starts
//! exactly as before: no fetch, the branch `fd/<callsign>`, no push row, no
//! Fleet Git event, and nothing reaches the remote.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::fleet_git::{Rig, commit_in, git};
use fleetdeck_proto::event::Event;
use serde_json::json;

#[tokio::test]
async fn sync_off_changes_no_session() {
    let mut rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    let before = rig.bare_refs();
    let local_head = git(&home, &["rev-parse", "HEAD"]);
    let tracking = git(&home, &["rev-parse", "origin/main"]);
    // A project that never saved its Git settings, and one that saved other
    // settings with sync off: both are plain.
    let id = rig.create_project(0, "App");
    let r = rig.save_git(&id, json!({ "cleanup_after_hours": 12 }));
    assert_eq!(r.status, 200, "{}", r.body);
    let p = rig.project_of(&id);
    assert_eq!(p["git"]["sync"], false);

    // The remote moves on; a fetch would show it.
    let remote_head = rig.advance_remote("two.txt", "two");
    assert_ne!(remote_head, local_head);

    let started = rig.start(0);
    let sid = started.session_id.as_str().to_owned();
    let s = rig.wait_summary("the first turn", 0, &sid, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    });
    // The old branch name and the local HEAD as the base: no fetch.
    let callsign = s.callsign.clone().unwrap();
    assert_eq!(s.branch.as_deref(), Some(format!("fd/{callsign}").as_str()));
    assert_eq!(s.base.clone().unwrap().oid, local_head);
    assert!(!s.base_stale);
    assert_eq!(git(&home, &["rev-parse", "origin/main"]), tracking);

    // A commit and one more turn: still no push row and no ref on the remote.
    let work = std::path::PathBuf::from(s.worktree.clone().unwrap());
    commit_in(&work, "x.txt", "x", "x");
    rig.prompt(0, &sid, "again");
    rig.wait_summary("the second turn", 0, &sid, |s| {
        s.current_turn.is_none() && s.usage.output_tokens.unwrap_or(0) >= 4
    });
    let s = rig.summary(0, &sid);
    assert!(s.push.is_none() && s.cleanup.is_none());
    let mut after = rig.bare_refs();
    assert_eq!(after.remove("refs/heads/main"), Some(remote_head));
    assert_eq!(
        after,
        before
            .into_iter()
            .filter(|(k, _)| k != "refs/heads/main")
            .collect()
    );

    // The session log has no Fleet Git event.
    let events = rig.events_after_stop(0).await;
    let fleet_git: Vec<_> = events
        .iter()
        .filter(|(_, e)| {
            matches!(
                e,
                Event::WorkspaceBaseStale { .. }
                    | Event::WorkspaceIncludeSkipped { .. }
                    | Event::SessionPush { .. }
                    | Event::SessionCleanup { .. }
            )
        })
        .collect();
    assert!(fleet_git.is_empty(), "{fleet_git:?}");
}
