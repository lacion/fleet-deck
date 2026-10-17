//! The recovery of the runs of an earlier process (FG-8, design §11.5): the
//! crash windows, an unreachable remote or forge across restarts (M1), and
//! the restart before any write. A crash is simulated by storing the journal
//! state that it leaves (the intent with its operands) and then running the
//! recovery, as a new process does at its start.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{LandPhase, LandState};
use tokio::sync::watch;

use super::rig::*;
use super::stage::*;
use crate::manager::land::oid7;
use crate::manager::land_run;
use crate::store::land_publish::Step;

async fn staged(rig: &Rig, land_id: &str) -> crate::store::land_runs::LandRunRow {
    rig.session("s1", false).await;
    move_main(rig);
    rig.admit("s1", land_id).await;
    checked(rig, land_id).await
}

fn hide_remote(rig: &Rig) {
    std::fs::rename(&rig.bare, rig.root.join("remote.gone")).unwrap();
}

fn show_remote(rig: &Rig) {
    std::fs::rename(rig.root.join("remote.gone"), &rig.bare).unwrap();
}

#[tokio::test]
async fn a_restart_before_any_write_ends_station_restarted() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let row = staged(&rig, "land-1").await;
    // A restart in `rebased` (and in each phase from `admitted` to `checked`).
    let step = Step {
        phase: Some(LandPhase::Rebased),
        event: true,
        ..Step::default()
    };
    assert!(
        rig.store
            .land_step(row.land_id.clone(), step)
            .await
            .unwrap()
    );
    let row = rig.reconcile("land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("station_restarted"));
    assert_eq!(
        row.message.as_deref(),
        Some("The station restarted during the land. Land again.\nNothing was pushed.")
    );
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
    assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), None);
}

#[tokio::test]
async fn a_restart_in_attempt_two_names_the_branch_that_attempt_one_published() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    staged(&rig, "land-1").await;
    let row = branch_done(&rig, "land-1").await;
    let r1 = row.branch_published_oid.clone().unwrap();
    // The movement transaction of attempt 2 is the last thing that ran.
    let step = Step {
        attempt: Some(2),
        phase: Some(LandPhase::Admitted),
        event: true,
        ..Step::default()
    };
    assert!(
        rig.store
            .land_step("land-1".to_owned(), step)
            .await
            .unwrap()
    );
    let row = rig.reconcile("land-1").await;
    assert_eq!(row.code.as_deref(), Some("station_restarted"));
    assert_eq!(
        row.message.as_deref(),
        Some(
            format!(
                "The station restarted during the land. Land again.\nThe rebased branch \
                 fd/alpha/s1 is on origin ({}); main did not change.",
                oid7(&r1)
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn a_crash_after_the_branch_intent_continues_with_the_push_done_or_not_done() {
    for done in [true, false] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        staged(&rig, "land-1").await;
        let row = branch_intent(&rig, "land-1").await;
        let r = row.rebased_oid.clone().unwrap();
        if done {
            rig.git(
                &rig.home,
                &[
                    "push",
                    "-q",
                    &rig.url,
                    &format!("{r}:refs/heads/fd/alpha/s1"),
                ],
            );
        }
        let before = rig.lines("pushes").len();
        let row = rig.reconcile("land-1").await;
        assert_eq!(row.state, LandState::Landed, "done={done}: {row:?}");
        assert_eq!(remote_ref(&rig, "refs/heads/main"), Some(r.clone()));
        assert_eq!(remote_ref(&rig, "refs/heads/fd/alpha/s1"), Some(r));
        // Done: the read found the commit and no branch push ran again. Not
        // done: the same operands were pushed once.
        let pushes = rig.lines("pushes").len() - before;
        assert_eq!(pushes, if done { 1 } else { 2 }, "done={done}");
    }
}

#[tokio::test]
async fn a_crash_after_the_default_intent_continues_with_the_push_done_or_not_done() {
    for done in [true, false] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        staged(&rig, "land-1").await;
        branch_done(&rig, "land-1").await;
        let row = default_intent(&rig, "land-1").await;
        let r = row.rebased_oid.clone().unwrap();
        if done {
            rig.git(
                &rig.home,
                &["push", "-q", &rig.url, &format!("{r}:refs/heads/main")],
            );
        }
        let before = rig.lines("pushes").len();
        let row = rig.reconcile("land-1").await;
        assert_eq!(row.state, LandState::Landed, "done={done}: {row:?}");
        assert_eq!(row.default_published_oid.as_deref(), Some(r.as_str()));
        assert_eq!(remote_ref(&rig, "refs/heads/main"), Some(r));
        // Done: no second push. Not done: it pushed again.
        assert_eq!(rig.lines("pushes").len() - before, usize::from(!done));
    }
}

#[tokio::test]
async fn a_default_push_that_the_remote_moved_past_lands_or_starts_attempt_two() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    staged(&rig, "land-1").await;
    branch_done(&rig, "land-1").await;
    default_intent(&rig, "land-1").await;
    // The remote's default branch moved to a commit that does not contain R.
    let other = rig.root.join("mover");
    rig.commit(&other, "n.txt", "again");
    rig.git(&other, &["push", "-q", "origin", "main"]);
    let (shared, _tx) = rig.shared();
    let row = rig.row("land-1").await;
    land_run::drive_loop(&shared, row).await;
    let row = rig.row("land-1").await;
    // Attempt 2 fetched, rebased onto the new tip and landed.
    assert_eq!((row.state, row.attempt), (LandState::Landed, 2), "{row:?}");
    assert_eq!(remote_ref(&rig, "refs/heads/main"), row.rebased_oid.clone());
}

#[tokio::test]
async fn an_unreachable_remote_keeps_the_run_reconciling_across_restarts() {
    for which in ["branch", "default"] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        staged(&rig, "land-1").await;
        let intent = if which == "branch" {
            branch_intent(&rig, "land-1").await
        } else {
            branch_done(&rig, "land-1").await;
            default_intent(&rig, "land-1").await
        };
        hide_remote(&rig);
        let first = rig.reconcile("land-1").await;
        assert_eq!(first.state, LandState::Running, "{which}");
        assert!(first.reconciling && first.pending_action.is_some());
        assert_eq!(first.reconcile_attempts, 1);
        assert_eq!(first.phase, intent.phase);
        // The next time follows the FG-2 backoff: one minute, then two.
        assert_eq!(
            first.next_reconcile_at,
            Some(rig.store.now_ms() + 60_000),
            "{which}"
        );
        let hold = rig.store.session_hold("s1".into()).await.unwrap();
        assert!(hold.is_some(), "the hold stays");
        // A second restart: the same operands, the same phase.
        let second = rig.reconcile("land-1").await;
        assert_eq!(second.reconcile_attempts, 2);
        assert_eq!(second.next_reconcile_at, Some(rig.store.now_ms() + 120_000));
        assert_eq!(
            (
                &second.pending_url,
                &second.pending_ref,
                &second.pending_oid,
                second.phase
            ),
            (
                &intent.pending_url,
                &intent.pending_ref,
                &intent.pending_oid,
                intent.phase
            )
        );
        // The remote is back: the same result as without the restarts.
        show_remote(&rig);
        let done = rig.reconcile("land-1").await;
        assert_eq!(done.state, LandState::Landed, "{which}: {done:?}");
        assert!(!done.reconciling && done.pending_action.is_none());
        assert_eq!(
            remote_ref(&rig, "refs/heads/main"),
            done.rebased_oid.clone()
        );
    }
}

#[tokio::test]
async fn an_unreachable_forge_keeps_a_pull_request_land_reconciling_until_it_answers() {
    for lost in [false, true] {
        let Some(rig) = Rig::new(false).await else {
            return;
        };
        rig.fake_forge();
        rig.session("s1", false).await;
        move_main(&rig);
        rig.admit_plan("s1", "land-1", rig.pr_plan("s1")).await;
        checked(&rig, "land-1").await;
        branch_done(&rig, "land-1").await;
        let intent = pr_intent(&rig, "land-1").await;
        rig.flag("find_fail", true);
        let first = rig.reconcile("land-1").await;
        assert!(
            first.reconciling && first.pending_action.is_some(),
            "{first:?}"
        );
        assert_eq!(first.phase, intent.phase);
        let second = rig.reconcile("land-1").await;
        assert_eq!(second.reconcile_attempts, 2);
        assert_eq!(second.phase, LandPhase::PrIntent);
        // The forge is back. With `lost` the create had worked before the
        // restart: the lookup finds it and nothing is created again.
        rig.flag("find_fail", false);
        if lost {
            std::fs::write(
                rig.fx("prs"),
                r#"[{"number":5,"html_url":"https://forge.example/pull/5"}]"#,
            )
            .unwrap();
        }
        let done = rig.reconcile("land-1").await;
        assert_eq!(done.state, LandState::Landed, "lost={lost}: {done:?}");
        assert_eq!(done.pr_url.as_deref(), Some("https://forge.example/pull/5"));
        assert_eq!(rig.lines("creates").len(), usize::from(!lost));
    }
}

#[tokio::test]
async fn the_engine_recovers_at_its_start_and_again_when_the_time_has_come() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    staged(&rig, "land-1").await;
    branch_intent(&rig, "land-1").await;
    hide_remote(&rig);
    rig.det.advance(Duration::from_secs(10));
    let (stop, rx) = watch::channel(false);
    let options = rig.options();
    let (_engine, tasks) = crate::manager::land::start(
        rig.store.clone(),
        Arc::clone(&rig.hub),
        Arc::new(move || options.clone()),
        rx,
        rig.deps(),
    );
    // The first pass recovers the run at once; the remote is away.
    wait_for(&rig, |r| r.reconcile_attempts == 1).await;
    show_remote(&rig);
    // Not due yet: nothing happens until its time has come.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.row("land-1").await.state, LandState::Running);
    rig.det.advance(Duration::from_secs(120));
    wait_for(&rig, |r| r.state == LandState::Landed).await;
    stop.send(true).unwrap();
    for task in tasks {
        let _ = task.await;
    }
}

async fn wait_for(rig: &Rig, done: impl Fn(&crate::store::land_runs::LandRunRow) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if done(&rig.row("land-1").await) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the engine did not recover the run"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
