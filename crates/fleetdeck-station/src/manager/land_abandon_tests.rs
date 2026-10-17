//! `fleet_git.land_abandon` and `fleet_git.land_status` (FG-8, design §11.2,
//! revision 4, M3): the fence is recorded only after the active write ended
//! and its outcome is stored, so no write of an abandoned run can start
//! after the acknowledgement, and none that started is still running.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    LandAbandonAnswer, LandPhase, LandState, LandStatusAnswer, LandWrite,
};

use super::rig::*;
use super::stage::*;
use crate::manager::fleet_git_writer::{LandTimeouts, WriteJob};
use crate::manager::land_abandon::{abandon, status};
use crate::store::land_publish::Step;

async fn running_reconciliation(rig: &Rig) {
    rig.session("s1", false).await;
    move_main(rig);
    rig.admit("s1", "land-1").await;
    checked(rig, "land-1").await;
    branch_intent(rig, "land-1").await;
    let step = Step {
        reconcile: crate::store::land_publish::ReconcileChange::Set {
            attempts: 1,
            next_at: rig.store.now_ms(),
        },
        event: true,
        ..Step::default()
    };
    assert!(
        rig.store
            .land_step("land-1".to_owned(), step)
            .await
            .unwrap()
    );
}

fn fenced(write: LandWrite, run_state: LandState) -> LandAbandonAnswer {
    LandAbandonAnswer::Fenced { write, run_state }
}

#[tokio::test]
async fn an_unknown_land_id_is_fenced_and_answers_the_same_again() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let engine = rig.engine();
    let first = abandon(&engine, "land-x").await.unwrap();
    assert_eq!(first, fenced(LandWrite::None, LandState::Unresolved));
    assert!(rig.store.land_fenced("land-x".to_owned()).await.unwrap());
    assert_eq!(abandon(&engine, "land-x").await.unwrap(), first);
    // A fence without a run answers `Fenced`; a stranger answers `Unknown`.
    assert_eq!(
        status(&rig.store, "land-x").await.unwrap(),
        LandStatusAnswer::Fenced
    );
    assert_eq!(
        status(&rig.store, "land-y").await.unwrap(),
        LandStatusAnswer::Unknown
    );
    // The lock of the id is forgotten.
    assert_eq!(rig.locks.open_locks(), 0);
}

#[tokio::test]
async fn an_abandoned_run_ends_unresolved_frees_the_session_and_stays_known() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    let engine = rig.engine();
    let answer = abandon(&engine, "land-1").await.unwrap();
    assert_eq!(answer, fenced(LandWrite::None, LandState::Unresolved));
    let row = rig.row("land-1").await;
    assert_eq!(
        (row.state, row.phase),
        (LandState::Unresolved, LandPhase::Done)
    );
    assert_eq!(row.code.as_deref(), Some("abandoned"));
    assert_eq!(row.abandon_write.as_deref(), Some("none"));
    assert_eq!(
        row.message.as_deref(),
        Some(
            "Fleet Deck could not confirm the result of this land. Check origin/the default \
             branch, the branch fd/alpha/s1 and the pull requests yourself."
        )
    );
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
    // A run answers `Known` also when it is fenced (decision Q14 B).
    let LandStatusAnswer::Known(known) = status(&rig.store, "land-1").await.unwrap() else {
        panic!("a run with a fence is known");
    };
    assert_eq!(known.state, LandState::Unresolved);
    assert_eq!(known.code.as_deref(), Some("abandoned"));
    // The same answer again, and the fence stops the engine.
    assert_eq!(abandon(&engine, "land-1").await.unwrap(), answer);
    let after = rig.drive("land-1").await;
    assert_eq!(after.state, LandState::Unresolved);
    assert!(rig.lines("pushes").is_empty());
}

#[tokio::test]
async fn a_writer_job_queued_before_the_fence_does_not_push() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    move_main(&rig);
    rig.admit("s1", "land-1").await;
    checked(&rig, "land-1").await;
    let refs = rig.refs();
    let engine = rig.engine();
    abandon(&engine, "land-1").await.unwrap();
    let deps = rig.deps();
    for job in [
        WriteJob::LandBranch {
            land_id: "land-1".to_owned(),
        },
        WriteJob::LandDefault {
            land_id: "land-1".to_owned(),
        },
    ] {
        assert!(deps.writer.run(job).await);
    }
    assert_eq!(rig.refs(), refs);
    assert!(rig.lines("pushes").is_empty());
}

/// The fence waits for the push of an executing reconciliation write, and
/// answers with the outcome that the writer stored.
async fn abandon_during_a_push(
    rig: Arc<Rig>,
    release: bool,
) -> (LandAbandonAnswer, Arc<Rig>, Option<String>) {
    running_reconciliation(&rig).await;
    let main_before = remote_ref(&rig, "refs/heads/main");
    rig.flag("block_push", true);
    let work = {
        let rig = Arc::clone(&rig);
        tokio::spawn(async move { rig.reconcile("land-1").await })
    };
    // The recovery read found the lease and the recovery push is blocked.
    rig.entered("push").await;
    let answer = {
        let engine = rig.engine();
        tokio::spawn(async move { abandon(&engine, "land-1").await.unwrap() })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!answer.is_finished(), "the handler waits for the write");
    if release {
        rig.flag("block_push", false);
    }
    let answer = answer.await.unwrap();
    rig.flag("block_push", false);
    work.await.unwrap();
    (answer, rig, main_before)
}

#[tokio::test]
async fn the_abandonment_waits_for_a_push_that_applies() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let rig = Arc::new(rig);
    let (answer, rig, main_before) = abandon_during_a_push(rig, true).await;
    assert_eq!(answer, fenced(LandWrite::Applied, LandState::Unresolved));
    let row = rig.row("land-1").await;
    assert_eq!(row.state, LandState::Unresolved);
    assert_eq!(row.code.as_deref(), Some("abandoned"));
    assert_eq!(row.abandon_write.as_deref(), Some("applied"));
    // The writer recorded the applied branch before the fence.
    assert_eq!(row.branch_published_oid, row.rebased_oid);
    assert!(row.branch_published_oid.is_some());
    assert!(rig.store.land_fenced("land-1".to_owned()).await.unwrap());
    // A queued default push does not push.
    let deps = rig.deps();
    assert!(
        deps.writer
            .run(WriteJob::LandDefault {
                land_id: "land-1".to_owned()
            })
            .await
    );
    assert_eq!(remote_ref(&rig, "refs/heads/main"), main_before);
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn the_abandonment_waits_for_a_push_that_times_out() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.set_timeouts(LandTimeouts {
        push: Duration::from_secs(1),
        lock_wait: Duration::from_secs(30),
        ..LandTimeouts::default()
    });
    let rig = Arc::new(rig);
    // The push never ends: its time limit gives the unknown outcome.
    let (answer, rig, main_before) = abandon_during_a_push(rig, false).await;
    assert_eq!(answer, fenced(LandWrite::Unknown, LandState::Unresolved));
    let row = rig.row("land-1").await;
    assert_eq!(row.state, LandState::Unresolved);
    assert_eq!(row.abandon_write.as_deref(), Some("unknown"));
    assert_eq!(row.last_write_outcome.as_deref(), Some("unknown"));
    assert!(row.pending_action.is_none() && !row.reconciling);
    assert_eq!(remote_ref(&rig, "refs/heads/main"), main_before);
}

#[tokio::test]
async fn a_lock_that_is_not_free_in_time_answers_busy_and_records_nothing() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.set_timeouts(LandTimeouts {
        push: Duration::from_secs(60),
        lock_wait: Duration::from_secs(1),
        ..LandTimeouts::default()
    });
    let rig = Arc::new(rig);
    running_reconciliation(&rig).await;
    rig.flag("block_push", true);
    let work = {
        let rig = Arc::clone(&rig);
        tokio::spawn(async move { rig.reconcile("land-1").await })
    };
    rig.entered("push").await;
    let engine = rig.engine();
    assert_eq!(
        abandon(&engine, "land-1").await.unwrap(),
        LandAbandonAnswer::Busy
    );
    assert!(!rig.store.land_fenced("land-1".to_owned()).await.unwrap());
    assert_eq!(rig.row("land-1").await.state, LandState::Running);
    // The write ends; the run goes on; the next call answers.
    rig.flag("block_push", false);
    let row = work.await.unwrap();
    assert_eq!(row.state, LandState::Landed, "{row:?}");
    assert_eq!(
        abandon(&engine, "land-1").await.unwrap(),
        fenced(LandWrite::None, LandState::Landed)
    );
    assert!(rig.store.land_fenced("land-1".to_owned()).await.unwrap());
}
