//! The effect-aware discard and the station rules of the `MoveIn` effect (FG-9,
//! design §12.3 "Abort", §12.9 revision 4, T4.2d), on the fixture of
//! `move_in_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    MoveDiscardEffect, MoveDiscardState, MoveInState, MoveKind, code,
};
use fleetdeck_testkit::sim_claude::StdinLog;

use super::super::Fx;
use super::Mx;
use crate::store::EffectState;
use crate::store::move_runs::Tombstone;

#[tokio::test]
async fn a_discard_during_the_launch_gate_abandons_ends_the_process_and_deletes_no_file() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    mx.upload_claude("m1").await;
    mx.fx.manager.gate_hook().arm();
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    // The effect holds the per-move lock inside its gate.
    tokio::time::timeout(
        Duration::from_secs(10),
        mx.fx.manager.gate_hook().reached.notified(),
    )
    .await
    .expect("the effect reaches the gate");
    let manager = Arc::clone(&mx.fx.manager);
    let discard = tokio::spawn(async move { manager.move_discard("m1").await.unwrap() });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!discard.is_finished(), "the discard waits for the lock");
    mx.fx.manager.gate_hook().release.notify_one();
    let answer = discard.await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (
            MoveDiscardState::Abandoned,
            Some(MoveDiscardEffect::Settled)
        )
    );
    let row = mx.fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Abandoned);
    assert_eq!(row.code.as_deref(), Some("move_cancelled"));
    // A process was started and ended; no file is deleted.
    assert_eq!(StdinLog::read(&mx.log).pids.len(), 1);
    assert_eq!(row.imported.len(), 1);
    assert!(Path::new(&row.imported[0].path).is_file());
    assert_eq!(mx.fx.summary(&id).await.state, SessionState::Exited);
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_discard_before_the_admission_spawns_nothing_and_deletes_the_files_after_the_settle() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    mx.upload_claude("m1").await;
    mx.fx.manager.launch_hook().arm();
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        mx.fx.manager.launch_hook().reached.notified(),
    )
    .await
    .expect("the effect reaches its launch identity");
    let row = mx.fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Imported);
    let placed = row.imported[0].path.clone();
    assert!(Path::new(&placed).is_file());
    let manager = Arc::clone(&mx.fx.manager);
    let discard = tokio::spawn(async move { manager.move_discard("m1").await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    mx.fx.manager.launch_hook().release.notify_one();
    let answer = discard.await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (
            MoveDiscardState::Discarded,
            Some(MoveDiscardEffect::Settled)
        )
    );
    // The admission changed no row and no provider process was spawned.
    assert_eq!(mx.fx.row("m1").await.state, MoveInState::Discarded);
    assert!(!mx.log.exists());
    assert_eq!(mx.wait_effect(&id).await, EffectState::RetiredNotDispatched);
    // The files go by identity, only after the settle.
    assert!(!Path::new(&placed).exists());
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_discard_of_a_queued_effect_answers_cancelled_and_the_effect_never_runs() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    // With the worker stopped, the effect stays queued.
    mx.fx
        .manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
    let publication = mx.publication();
    let command = mx.command("m1", MoveKind::BranchOnly, "claude", &publication, None);
    let decided = mx.fx.send("c1", CommandOrigin::Board, None, command).await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let id = mx.fx.session_of(&decided).await;
    let effects = mx
        .fx
        .rig
        .store
        .session_effects(id.as_str().into())
        .await
        .unwrap();
    assert_eq!(effects[0].state, EffectState::Queued);
    let answer = mx.fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (
            MoveDiscardState::Discarded,
            Some(MoveDiscardEffect::Cancelled)
        )
    );
    let effects = mx
        .fx
        .rig
        .store
        .session_effects(id.as_str().into())
        .await
        .unwrap();
    assert_eq!(effects[0].state, EffectState::RetiredNotDispatched);
    assert_eq!(effects[0].reason.as_deref(), Some("move_cancelled"));
    assert!(!mx.worktree().exists());
}

#[tokio::test]
async fn a_launching_row_is_abandoned_at_the_station_start_and_other_rows_stay() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    fx.accepted("c2", fx.move_in("m2", MoveKind::BranchOnly))
        .await;
    fx.advance("m1", MoveInState::Staged, MoveInState::Launching)
        .await;
    fx.rig.store.recover(Vec::new()).await.unwrap();
    let row = fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Abandoned);
    assert_eq!(row.code.as_deref(), Some(code::MOVE_RESUME_FAILED));
    assert_eq!(
        row.message.as_deref(),
        Some("The station restarted while the session was resuming.")
    );
    // The hold stays and the other row is unchanged.
    let hold = fx
        .rig
        .store
        .session_hold(row.session_id.clone())
        .await
        .unwrap();
    assert!(hold.is_some());
    assert_eq!(fx.row("m2").await.state, MoveInState::Staged);
    fx.shutdown().await;
}

#[tokio::test]
async fn a_release_needs_a_committed_move_and_a_repeat_answers_the_stored_state() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    // Not committed: refused.
    let err = fx.manager.move_release("m1").await.unwrap_err();
    assert_eq!(err.code, "invalid_state");
    let err = fx.manager.move_release("never").await.unwrap_err();
    assert_eq!(err.code, "invalid_state");
    // A fail of a move that is not `resume_failed` answers its state.
    let answer = fx.manager.move_fail("m1").await.unwrap();
    assert_eq!(answer.state, MoveInState::Staged);
    let err = fx.manager.move_fail("never").await.unwrap_err();
    assert_eq!(err.code, "invalid_state");
    fx.shutdown().await;
}

#[tokio::test]
async fn each_transition_with_the_wrong_expected_state_changes_zero_rows() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    let store = &fx.rig.store;
    for (from, to) in [
        (MoveInState::Launching, MoveInState::Resumed),
        (MoveInState::Imported, MoveInState::Launching),
        (MoveInState::Released, MoveInState::Confirmed),
    ] {
        let moved = store
            .move_in_advance("m1".into(), from, to, None, Tombstone::Refuse)
            .await
            .unwrap();
        assert!(!moved, "{from:?} to {to:?}");
    }
    assert_eq!(fx.row("m1").await.state, MoveInState::Staged);
    fx.shutdown().await;
}
