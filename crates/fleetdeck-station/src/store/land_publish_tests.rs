//! The transactions of the land publication (design §11.5): the fence rule,
//! the pending operands, the branch success with its push row, the fence of
//! an abandonment, and "Push now" after a land.

use fleetdeck_proto::fleet_git::{LandPhase, LandState, PushState};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::land_runs::tests::{new_run, with_session};
use crate::store::land_runs::{PENDING_BRANCH_PUSH, insert};
use crate::store::{Store, call};

const R: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const DEST: &str = "https://h:443/acme/app";

async fn run(store: &Store, id: &str) {
    let new = new_run(id, &format!("cmd-{id}"), "s1");
    call(&store.inner.db, move |conn| {
        insert(conn, &new, 1_000)?;
        session_holds::insert(conn, &"s1".into(), "land", &new.land_id, 1_000)
    })
    .await
    .unwrap();
}

fn pending() -> Pending {
    Pending {
        action: PENDING_BRANCH_PUSH,
        attempt: 1,
        url: "https://h/acme/app".to_owned(),
        ref_name: "refs/heads/fd/alpha/s1".to_owned(),
        oid: R.to_owned(),
        lease: Some(String::new()),
        expect: None,
    }
}

fn intent() -> Step {
    Step {
        phase: Some(LandPhase::BranchPushIntent),
        pending: PendingChange::Set(pending()),
        outcome: OutcomeChange::Clear,
        require: Require::NoPending,
        event: true,
        ..Step::default()
    }
}

fn up(tip: Option<&str>, contains: bool, dest: &str) -> PushUpsert {
    PushUpsert {
        session_id: "s1".into(),
        target: "origin".to_owned(),
        target_dest: dest.to_owned(),
        publish_ref: "fd/alpha/s1".to_owned(),
        oid: R.to_owned(),
        tip: tip.map(str::to_owned),
        tip_contains: contains,
    }
}

#[tokio::test]
async fn an_intent_stores_the_phase_and_the_operands_once() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    assert!(
        store
            .land_step("land-1".to_owned(), intent())
            .await
            .unwrap()
    );
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.phase, LandPhase::BranchPushIntent);
    assert_eq!(row.pending_action.as_deref(), Some("branch_push"));
    assert_eq!(
        (
            row.pending_attempt,
            row.pending_oid.as_deref(),
            row.pending_lease.as_deref()
        ),
        (Some(1), Some(R), Some(""))
    );
    assert_eq!(row.last_write_outcome, None);
    // A first write needs no outstanding write; a recovery write needs one.
    assert!(
        !store
            .land_step("land-1".to_owned(), intent())
            .await
            .unwrap()
    );
    let recovery = Step {
        outcome: OutcomeChange::Clear,
        require: Require::Pending,
        ..Step::default()
    };
    assert!(
        store
            .land_step("land-1".to_owned(), recovery)
            .await
            .unwrap()
    );
    // An end with a pending write is refused until the write is cleared.
    let end = Step {
        end: Some(StepEnd {
            state: LandState::Failed,
            code: Some("push_failed".to_owned()),
            text: Some("t".to_owned()),
        }),
        ..Step::default()
    };
    assert!(
        !store
            .land_step("land-1".to_owned(), end.clone())
            .await
            .unwrap()
    );
    let end = Step {
        pending: PendingChange::Clear,
        ..end
    };
    assert!(store.land_step("land-1".to_owned(), end).await.unwrap());
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!((row.state, row.phase), (LandState::Failed, LandPhase::Done));
    assert!(row.ended_at.is_some() && row.pending_action.is_none());
    // The hold ended in the same transaction; a later step finds the run over.
    assert!(store.session_hold("s1".into()).await.unwrap().is_none());
    assert!(
        !store
            .land_step("land-1".to_owned(), intent())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_fenced_run_takes_no_step() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    call(&store.inner.db, |conn| {
        land_fences::insert(conn, "land-1", 5)
    })
    .await
    .unwrap();
    assert!(
        !store
            .land_step("land-1".to_owned(), intent())
            .await
            .unwrap()
    );
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!((row.phase, row.pending_action), (LandPhase::Admitted, None));
}

#[tokio::test]
async fn the_reconciliation_columns_follow_the_step() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    let wait = Step {
        reconcile: ReconcileChange::Set {
            attempts: 2,
            next_at: 77,
        },
        event: true,
        ..Step::default()
    };
    store.land_step("land-1".to_owned(), wait).await.unwrap();
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert!(row.reconciling);
    assert_eq!(
        (row.reconcile_attempts, row.next_reconcile_at),
        (2, Some(77))
    );
    let clear = Step {
        reconcile: ReconcileChange::Clear,
        ..Step::default()
    };
    store.land_step("land-1".to_owned(), clear).await.unwrap();
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert!(!row.reconciling);
    assert_eq!(row.next_reconcile_at, None);
}

#[tokio::test]
async fn a_published_branch_stops_the_push_row_unless_the_tip_contains_it() {
    // The tip does not contain R: the row is created `stopped`.
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    let stored = store
        .land_branch_applied(
            "land-1".to_owned(),
            OutcomeChange::Set(WRITE_APPLIED),
            up(Some(TIP), false, DEST),
            "moved".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(stored, BranchStored::Stored);
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.phase, LandPhase::BranchPushed);
    assert_eq!(row.branch_published_oid.as_deref(), Some(R));
    assert_eq!(row.last_write_outcome.as_deref(), Some("applied"));
    assert!(row.pending_action.is_none() && !row.reconciling);
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Stopped);
    assert_eq!(push.stop_code.as_deref(), Some(STOP_LAND_PUBLISHED));
    assert_eq!(
        (
            push.last_pushed_oid.as_deref(),
            push.target.as_str(),
            push.target_dest.as_str()
        ),
        (Some(R), "origin", DEST)
    );

    // The tip is R itself: nothing to push, the row is `pushed`.
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    store
        .land_branch_applied(
            "land-1".to_owned(),
            OutcomeChange::Keep,
            up(Some(R), true, DEST),
            String::new(),
        )
        .await
        .unwrap();
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Pushed);

    // The tip is above R: the row is due again with the lease R.
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    store
        .land_branch_applied(
            "land-1".to_owned(),
            OutcomeChange::Keep,
            up(Some(TIP), true, DEST),
            String::new(),
        )
        .await
        .unwrap();
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Due);
    assert_eq!(push.desired_oid.as_deref(), Some(TIP));
}

#[tokio::test]
async fn a_push_row_of_another_destination_is_not_overwritten() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    call(&store.inner.db, |conn| {
        conn.execute(
            "INSERT INTO git_pushes (session_id, target, target_dest, ref, state, updated_at)
             VALUES ('s1', 'origin', 'https://other:443/x/y', 'fd/alpha/s1', 'idle', 1)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let stored = store
        .land_branch_applied(
            "land-1".to_owned(),
            OutcomeChange::Set(WRITE_APPLIED),
            up(Some(TIP), false, DEST),
            "The branch moved.".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(stored, BranchStored::Moved);
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.code.as_deref()),
        (LandState::Failed, Some("branch_moved"))
    );
    assert_eq!(row.message.as_deref(), Some("The branch moved."));
    assert_eq!(row.branch_published_oid.as_deref(), Some(R));
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.target_dest, "https://other:443/x/y");
    assert_eq!(push.last_pushed_oid, None);
}

#[tokio::test]
async fn the_fence_of_an_abandonment_is_recorded_once_and_answers_the_same() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    store
        .land_write_outcome("land-1".to_owned(), WRITE_UNKNOWN)
        .await
        .unwrap();
    // The lock was free: no write waited, whatever the last outcome was.
    let none = store
        .land_abandon("land-1".to_owned(), false, |_, w| format!("text {w}"))
        .await
        .unwrap();
    assert_eq!(
        (none.write, none.run_state),
        ("none", LandState::Unresolved)
    );
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.message.as_deref(), Some("text none"));
    assert_eq!(row.code.as_deref(), Some("abandoned"));
    assert!(row.pending_action.is_none() && !row.reconciling);
    assert!(store.land_fenced("land-1".to_owned()).await.unwrap());
    // The repeat does not change the first values (also with `waited`).
    let again = store
        .land_abandon("land-1".to_owned(), true, |_, w| format!("other {w}"))
        .await
        .unwrap();
    assert_eq!(
        (again.write, again.run_state),
        ("none", LandState::Unresolved)
    );
    assert_eq!(
        store
            .land_run("land-1".to_owned())
            .await
            .unwrap()
            .unwrap()
            .message
            .as_deref(),
        Some("text none")
    );
}

#[tokio::test]
async fn the_outcome_of_the_awaited_write_becomes_the_abandon_write() {
    for (outcome, want) in [
        (WRITE_APPLIED, "applied"),
        (WRITE_REJECTED, "rejected"),
        (WRITE_UNKNOWN, "unknown"),
        (WRITE_NOT_STARTED, "none"),
    ] {
        let det = Det::new();
        let store = with_session(&det, "s1").await;
        run(&store, "land-1").await;
        store
            .land_step("land-1".to_owned(), intent())
            .await
            .unwrap();
        store
            .land_write_outcome("land-1".to_owned(), outcome)
            .await
            .unwrap();
        let got = store
            .land_abandon("land-1".to_owned(), true, |_, w| w.to_owned())
            .await
            .unwrap();
        assert_eq!(got.write, want);
    }
    // A write that waited but has no stored outcome counts as unknown.
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    let got = store
        .land_abandon("land-1".to_owned(), true, |_, w| w.to_owned())
        .await
        .unwrap();
    assert_eq!(got.write, "unknown");
}

#[tokio::test]
async fn a_terminal_run_gets_the_fence_and_keeps_its_state() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    let end = Step {
        end: Some(StepEnd {
            state: LandState::Landed,
            code: None,
            text: None,
        }),
        ..Step::default()
    };
    assert!(store.land_step("land-1".to_owned(), end).await.unwrap());
    let got = store
        .land_abandon("land-1".to_owned(), true, |_, w| w.to_owned())
        .await
        .unwrap();
    assert_eq!(got.run_state, LandState::Landed);
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!((row.state, row.code), (LandState::Landed, None));
    assert!(store.land_fenced("land-1".to_owned()).await.unwrap());
}

#[tokio::test]
async fn push_now_after_a_land_needs_a_stopped_land_published_row_and_a_new_tip() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    run(&store, "land-1").await;
    store
        .land_step("land-1".to_owned(), intent())
        .await
        .unwrap();
    store
        .land_branch_applied(
            "land-1".to_owned(),
            OutcomeChange::Keep,
            up(Some(TIP), false, DEST),
            String::new(),
        )
        .await
        .unwrap();
    // The tip that equals R changes nothing.
    assert_eq!(
        store
            .land_push_reopen("s1".into(), R.to_owned())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .land_push_reopen("s1".into(), TIP.to_owned())
            .await
            .unwrap(),
        Some(PushState::Due)
    );
    let push = store.git_push("s1".into()).await.unwrap().unwrap();
    assert_eq!(push.state, PushState::Due);
    assert_eq!(push.stop_code, None);
    assert_eq!(push.desired_oid.as_deref(), Some(TIP));
    // The lease is the last pushed commit, R.
    assert_eq!(push.last_pushed_oid.as_deref(), Some(R));
    // A row that is not stopped by a land is left alone.
    assert_eq!(
        store
            .land_push_reopen("s1".into(), TIP.to_owned())
            .await
            .unwrap(),
        None
    );
}
