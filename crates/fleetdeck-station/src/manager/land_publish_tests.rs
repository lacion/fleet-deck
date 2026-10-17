//! The publication of a land (FG-8, design §11.5 steps 5 to 7): real Git, a
//! bare remote, a fake `gh`, and a fake `git` that can fail, hang or move the
//! remote (`land_rig.rs`). The run is driven by hand.

use std::time::Duration;

use fleetdeck_proto::fleet_git::{LandPhase, LandState, PushState};

use super::rig::*;
use super::stage::*;
use crate::manager::fleet_git_writer::{LandLocks, LandTimeouts};
use crate::manager::land::oid7;

async fn landed(rig: &Rig, session: &str, land_id: &str) -> crate::store::land_runs::LandRunRow {
    rig.session(session, false).await;
    rig.admit(session, land_id).await;
    rig.run_all(land_id).await
}

#[tokio::test]
async fn a_clean_merge_land_advances_the_remote_default_branch_and_stops_the_push_row() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    move_main(&rig);
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!((row.state, row.phase), (LandState::Landed, LandPhase::Done));
    let r = row.rebased_oid.clone().unwrap();
    assert_eq!(remote_ref(&rig, "refs/heads/main"), Some(r.clone()));
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r.clone()));
    assert_eq!(row.branch_published_oid.as_deref(), Some(r.as_str()));
    assert_eq!(row.default_published_oid.as_deref(), Some(r.as_str()));
    assert_eq!((row.pending_action.clone(), row.reconciling), (None, false));
    assert_eq!(row.last_write_outcome.as_deref(), Some("applied"));
    assert_eq!(row.code, None);
    // The hold ends with the run, in the same transaction.
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
    let phases: Vec<LandPhase> = rig.phases().await.into_iter().map(|p| p.1).collect();
    assert_eq!(
        phases,
        vec![
            LandPhase::Admitted,
            LandPhase::Captured,
            LandPhase::WorktreeReady,
            LandPhase::Rebased,
            LandPhase::Checked,
            LandPhase::BranchPushIntent,
            LandPhase::BranchPushed,
            LandPhase::DefaultPushIntent,
            LandPhase::Done,
        ]
    );
    // The default push has no `+` and the lease of the captured base (H16).
    let pushes = rig.lines("pushes");
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    assert!(pushes[0].contains("--force-with-lease=refs/heads/fd/alpha/s1:"));
    let base = row.base_oid.clone().unwrap();
    assert!(
        pushes[1].contains(&format!("--force-with-lease=refs/heads/main:{base}")),
        "{}",
        pushes[1]
    );
    assert!(!pushes[1].contains(":+"), "{}", pushes[1]);
    // The branch push left the push row stopped: the old local tip is never
    // pushed (decision 53).
    let push = rig.store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Stopped);
    assert_eq!(push.stop_code.as_deref(), Some("land_published"));
    assert_eq!(push.last_pushed_oid.as_deref(), Some(r.as_str()));
    assert_eq!(push.ref_name, "fd/alpha/s1");
    let old_tip = rig.git(&rig.wt("s1"), &["rev-parse", "HEAD"]);
    rig.store
        .git_push_trigger("s1".into(), old_tip.clone())
        .await
        .unwrap();
    let push = rig.store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Stopped);
    assert_eq!(push.desired_oid.as_deref(), Some(old_tip.as_str()));
    assert!(
        rig.store
            .git_push_due(rig.store.now_ms() + 1_000_000)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_remote_moved_once_lands_on_attempt_two_and_twice_gives_remote_moved() {
    for (moves, lands) in [(1, true), (2, false)] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        rig.bump(moves, "bumped.txt");
        let row = landed(&rig, "s1", "land-1").await;
        assert_eq!(row.attempt, 2);
        let r2 = row.rebased_oid.clone().unwrap();
        // The branch of attempt 1 stays on the remote until attempt 2 pushes.
        assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r2.clone()));
        if lands {
            assert_eq!(row.state, LandState::Landed, "{row:?}");
            assert_eq!(remote_ref(&rig, "refs/heads/main"), Some(r2.clone()));
            // The same source commit, a fresh base: R2 sits on the new tip.
            let parent = rig.git(&rig.home, &["rev-parse", &format!("{r2}^")]);
            assert_eq!(row.base_oid.as_deref(), Some(parent.as_str()));
            assert_eq!(rig.lines("pushes").len(), 4);
        } else {
            assert_eq!(row.state, LandState::RemoteMoved, "{row:?}");
            let text = format!(
                "origin/main moved twice during the land. Land again.\nThe rebased branch \
                 fd/alpha/s1 is on origin ({}); main did not change.",
                oid7(&r2)
            );
            assert_eq!(row.message.as_deref(), Some(text.as_str()));
            assert_ne!(remote_ref(&rig, "refs/heads/main"), Some(r2));
        }
        assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
        // Each attempt has its events and its own temporary folder.
        let mut attempts = rig.attempts().await;
        attempts.dedup();
        assert_eq!(attempts, vec![1, 2]);
        assert!(
            rig.store
                .land_temp("land-1".to_owned(), 2)
                .await
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn a_conflict_in_attempt_two_keeps_the_branch_of_attempt_one() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    // The remote's own commit adds the file that the session adds.
    rig.bump(1, "work.txt");
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!(
        (row.state, row.attempt),
        (LandState::Conflict, 2),
        "{row:?}"
    );
    let r1 = row.branch_published_oid.clone().unwrap();
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r1.clone()));
    let text = row.message.clone().unwrap();
    assert!(
        text.starts_with("Conflict in 1 files with origin/main ("),
        "{text}"
    );
    assert!(text.contains(") on the second attempt."), "{text}");
    assert!(
        text.ends_with(&format!(
            "The rebased branch fd/alpha/s1 is on origin ({}); main did not change.",
            oid7(&r1)
        )),
        "{text}"
    );
    assert_eq!(row.files, vec!["work.txt".to_owned()]);
}

#[tokio::test]
async fn a_failed_check_in_attempt_two_names_the_published_branch() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    rig.session("s1", false).await;
    let mut plan = rig.plan("s1");
    plan.check_cmd = Some("test ! -f bumped.txt".to_owned());
    rig.admit_plan("s1", "land-1", plan).await;
    rig.bump(1, "bumped.txt");
    let row = rig.run_all("land-1").await;
    assert_eq!(
        (row.state, row.attempt),
        (LandState::CheckFailed, 2),
        "{row:?}"
    );
    let r1 = row.branch_published_oid.clone().unwrap();
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r1.clone()));
    let text = row.message.clone().unwrap();
    assert!(
        text.starts_with("The check command failed (exit 1) on the second attempt."),
        "{text}"
    );
    assert!(text.ends_with(&format!(
        "The rebased branch fd/alpha/s1 is on origin ({}); main did not change.",
        oid7(&r1)
    )));
}

#[tokio::test]
async fn a_push_that_fails_without_an_answer_is_repeated_once() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    // The first branch push fails with a transport error, the next works.
    rig.flag("fail_push_once", true);
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_eq!(rig.lines("pushes").len(), 3);
    // A push that always fails ends after the repeat, confirmed not applied.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.flag("fail_push", true);
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("push_failed"));
    assert_eq!(rig.lines("pushes").len(), 2);
    assert!(!row.reconciling && row.pending_action.is_none());
    let text = row.message.unwrap();
    assert!(text.starts_with("Push failed: "), "{text}");
    assert!(text.ends_with("Nothing was pushed."), "{text}");
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), None);
}

#[tokio::test]
async fn a_rejected_push_that_the_read_confirms_ends_push_rejected_not_reconciling() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.flag("reject_push", true);
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("push_rejected"));
    assert!(!row.reconciling && row.pending_action.is_none());
    assert_eq!(row.last_write_outcome.as_deref(), Some("rejected"));
    assert!(
        row.message
            .as_deref()
            .unwrap()
            .starts_with("Push was rejected: ")
    );
    // A rejected push is not repeated.
    assert_eq!(rig.lines("pushes").len(), 1);
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), None);
}

#[tokio::test]
async fn a_push_that_applies_and_then_times_out_is_found_by_the_read() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    // Each push reaches the remote and then does not end: the time limit
    // kills it, the outcome is unknown, the read finds the commit.
    rig.flag("hang_after_push", true);
    rig.set_timeouts(LandTimeouts {
        push: Duration::from_secs(2),
        ..LandTimeouts::default()
    });
    let row = landed(&rig, "s1", "land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    let r = row.rebased_oid.clone().unwrap();
    assert_eq!(remote_ref(&rig, "refs/heads/main"), Some(r));
    // One branch push and one default push; nothing was pushed twice.
    assert_eq!(rig.lines("pushes").len(), 2);
    assert_eq!(row.last_write_outcome.as_deref(), Some("unknown"));
    // No failed event came before the end.
    let states: Vec<LandState> = rig.phases().await.into_iter().map(|p| p.0).collect();
    assert!(!states.contains(&LandState::Failed));
}

#[tokio::test]
async fn a_branch_that_moved_on_the_remote_ends_branch_moved() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    move_main(&rig);
    // Another machine pushed the session branch meanwhile.
    rig.git(
        &rig.wt("s1"),
        &["push", "-q", "origin", "HEAD:refs/heads/fd/alpha/s1"],
    );
    let other = remote_ref(&rig, "refs/heads/fd/alpha/s1").unwrap();
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Failed, "{row:?}");
    // The lease was '' (no push row), the ref exists: the push is refused,
    // the read finds another tip.
    assert_eq!(row.code.as_deref(), Some("branch_moved"));
    assert_eq!(
        row.message.as_deref(),
        Some("The branch fd/alpha/s1 changed on origin during the land.\nNothing was pushed.")
    );
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(other));
}

#[tokio::test]
async fn a_pull_request_land_pushes_the_branch_and_creates_one_request() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_eq!(row.pr_url.as_deref(), Some("https://forge.example/pull/5"));
    assert_eq!(row.pr_number, Some(5));
    assert_eq!(row.default_published_oid, None);
    // The branch is on the remote; the default branch did not change.
    assert!(remote_ref(&rig, "refs/heads/fd/alpha/s1").is_some());
    assert_eq!(
        remote_ref(&rig, "refs/heads/main"),
        Some(rig.git(&rig.home, &["rev-parse", "main"]))
    );
    assert_eq!(rig.lines("creates").len(), 1);
    let calls = rig.lines("forge_calls");
    assert!(calls[0].contains("-X GET"), "{calls:?}");
    assert!(calls[0].contains("head=tmp%3Afd%2Falpha%2Fs1"), "{calls:?}");
    assert!(calls[1].contains("-X POST"), "{calls:?}");
    let phases: Vec<LandPhase> = rig.phases().await.into_iter().map(|p| p.1).collect();
    assert!(phases.contains(&LandPhase::PrIntent), "{phases:?}");
}

#[tokio::test]
async fn an_existing_pull_request_is_the_result_and_two_are_ambiguous() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    std::fs::write(
        rig.fx("prs"),
        format!("[{}]", rig.pr_item(9, "https://forge.example/pull/9")),
    )
    .unwrap();
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Landed);
    assert_eq!(row.pr_url.as_deref(), Some("https://forge.example/pull/9"));
    assert!(rig.lines("creates").is_empty());

    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    std::fs::write(
        rig.fx("prs"),
        format!("[{},{}]", rig.pr_item(1, "u1"), rig.pr_item(2, "u2")),
    )
    .unwrap();
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("pr_ambiguous"));
    let text = row.message.unwrap();
    assert!(
        text.starts_with(
            "More than one open pull request has the head fd/alpha/s1 and the base main. \
             Close the extra ones and land again.\nThe rebased branch fd/alpha/s1 is on origin"
        ),
        "{text}"
    );
    assert!(rig.lines("creates").is_empty());
}

#[tokio::test]
async fn a_pull_request_from_a_fork_is_not_the_result_of_the_land() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    // Somebody else's fork has a request with the same head branch name.
    let fork = rig
        .pr_item(8, "https://forge.example/pull/8")
        .replace("\"full_name\":\"", "\"full_name\":\"mallory-");
    std::fs::write(rig.fx("prs"), format!("[{fork}]")).unwrap();
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    // The lookup found no request of ours: the land created its own.
    assert_eq!(row.pr_url.as_deref(), Some("https://forge.example/pull/5"));
    assert_eq!(rig.lines("creates").len(), 1);
}

#[tokio::test]
async fn a_lookup_with_more_than_ten_pages_ends_pr_ambiguous() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    rig.flag("link_next", true);
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Failed, "{row:?}");
    assert_eq!(row.code.as_deref(), Some("pr_ambiguous"));
    assert!(rig.lines("creates").is_empty());
    let gets = rig
        .lines("forge_calls")
        .iter()
        .filter(|l| l.contains("-X GET"))
        .count();
    assert_eq!(gets, 10);
}

#[tokio::test]
async fn a_create_whose_answer_is_lost_or_a_502_is_found_by_the_lookup() {
    for mode in ["drop", "bad502"] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        rig.fake_forge();
        std::fs::write(rig.fx("create_mode"), mode).unwrap();
        rig.session("s1", false).await;
        rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
        let row = rig.run_all("land-1").await;
        assert_eq!(row.state, LandState::Landed, "{mode}: {row:?}");
        assert_eq!(row.pr_url.as_deref(), Some("https://forge.example/pull/5"));
        // The fake forge saw exactly one create.
        assert_eq!(rig.lines("creates").len(), 1, "{mode}");
        assert_eq!(row.last_write_outcome.as_deref(), Some("unknown"));
    }
}

#[tokio::test]
async fn a_422_with_no_request_found_ends_pr_failed_and_a_dead_lookup_before_any_write_too() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    std::fs::write(rig.fx("create_mode"), "r422").unwrap();
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("pr_failed"));
    let text = row.message.unwrap();
    assert!(
        text.starts_with("The pull request failed: HTTP 422: "),
        "{text}"
    );
    assert!(
        text.contains("Validation Failed") && text.contains("The rebased branch"),
        "{text}"
    );
    assert_eq!(row.last_write_outcome.as_deref(), Some("rejected"));
    assert_eq!(rig.lines("creates").len(), 1);

    // The lookup before any write cannot reach the forge: nothing was sent.
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.fake_forge();
    rig.flag("find_fail", true);
    rig.session("s1", false).await;
    rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
    let row = rig.run_all("land-1").await;
    assert_eq!(row.code.as_deref(), Some("pr_failed"));
    assert!(rig.lines("creates").is_empty());
}

#[tokio::test]
async fn the_per_land_lock_is_taken_freed_and_forgotten() {
    let locks = LandLocks::new();
    let held = locks.try_lock("land-1").unwrap();
    assert!(locks.try_lock("land-1").is_none());
    assert!(locks.try_lock("land-2").is_some());
    let wait = locks
        .lock_within("land-1", Duration::from_millis(100))
        .await;
    assert!(wait.is_none());
    // A lock that is held is not forgotten.
    locks.release("land-1");
    assert!(locks.try_lock("land-1").is_none());
    drop(held);
    assert!(locks.try_lock("land-1").is_some());
    locks.release("land-1");
    locks.release("land-2");
    assert_eq!(locks.open_locks(), 0);
    assert!(
        locks
            .lock_within("land-1", Duration::from_millis(100))
            .await
            .is_some()
    );
}
