//! The station settle check at resume (DL-432, B13): an `exited` row with
//! the reason `exit_unverified` is proven ended by the station's own check
//! when nothing of the run lives. See `shim_resume_check_tests.rs` for the
//! owner's check.

use fleetdeck_proto::command::{SessionCommand, StopMode, UserInput};
use fleetdeck_proto::event::{ReconciledBy, SessionState};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use crate::manager::shim_resume_check_tests::Orphan;
use crate::manager::shim_resume_rig::*;
use crate::store::ShimState;

/// The station proofs and the reconciliation events of `session`.
async fn settled(
    rig: &Rig,
    session: &SessionId,
) -> (Vec<Option<String>>, Vec<Option<ReconciledBy>>) {
    let proofs = rig
        .rows(session)
        .await
        .into_iter()
        .map(|r| r.station_proof)
        .collect();
    let events = rig
        .events_of(Some(session))
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::ExecutionReconciled { by, .. } => Some(by),
            _ => None,
        })
        .collect();
    (proofs, events)
}

/// An ended session with one row `(state, reason, inspection)` whose
/// provider identity is dead and on `boot`.
async fn with_row(
    rig: &mut Rig,
    (state, reason, inspection): (ShimState, &str, u32),
    boot: &str,
) -> SessionId {
    let session = rig.ended("claude").await;
    rig.row(
        &session,
        "run",
        state,
        reason,
        inspection,
        Some(dead_identity(boot)),
    )
    .await;
    session
}

/// A refused resume leaves no station proof and no reconciliation event.
async fn refused_without_proof(rig: &mut Rig, session: &SessionId) -> String {
    let (code, message) = rig.refused(resume(session)).await;
    assert_eq!(code, RejectCode::InvalidState, "{message}");
    assert!(message.contains("not_resumable"), "{message}");
    assert_eq!(settled(rig, session).await, (vec![None], vec![]));
    message
}

#[tokio::test]
async fn a_resume_after_an_unverified_exit_with_nothing_alive_is_admitted_and_records_the_station_proof()
 {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Exited, "exit_unverified", 1);
    let session = with_row(&mut rig, row, &real_boot()).await;
    rig.accepted(resume(&session)).await;
    let rows = rig.rows(&session).await;
    assert_eq!(rows[0].station_proof.as_deref(), Some("checked_after_exit"));
    assert!(rows[0].station_proof_at.is_some());
    assert_eq!(rows[0].proof, None);
    assert_eq!(
        (rows[0].state, rows[0].reason.as_deref()),
        (ShimState::Exited, Some("exit_unverified"))
    );
    let (_, events) = settled(&rig, &session).await;
    assert_eq!(events, vec![Some(ReconciledBy::Station)]);
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_live_group_member_keeps_the_resume_refused() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    let orphan = Orphan::start();
    let mut identity = orphan.identity.clone();
    identity.boot_id = real_boot();
    rig.row(
        &session,
        "run",
        ShimState::Exited,
        "exit_unverified",
        1,
        Some(identity),
    )
    .await;
    refused_without_proof(&mut rig, &session).await;
    drop(orphan);
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_exit_with_descendants_remaining_is_not_settled_by_the_station() {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Exited, "exited_descendants_remaining", 1);
    let session = with_row(&mut rig, row, &real_boot()).await;
    let message = refused_without_proof(&mut rig, &session).await;
    assert!(
        message.contains("exited_descendants_remaining"),
        "{message}"
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_lost_row_is_not_settled_by_the_station() {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Lost, "execution_unknown", 0);
    let session = with_row(&mut rig, row, &real_boot()).await;
    refused_without_proof(&mut rig, &session).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_row_of_another_boot_is_not_settled_by_the_check() {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Exited, "exit_unverified", 1);
    let session = with_row(&mut rig, row, "boot-old").await;
    refused_without_proof(&mut rig, &session).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_legacy_exit_without_inspection_is_settled() {
    let mut rig = Rig::direct(None).await;
    let session = with_row(&mut rig, (ShimState::Exited, "exited", 0), &real_boot()).await;
    rig.accepted(resume(&session)).await;
    let (proofs, events) = settled(&rig, &session).await;
    assert_eq!(proofs, vec![Some("checked_after_exit".to_owned())]);
    assert_eq!(events, vec![Some(ReconciledBy::Station)]);
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_prompt_to_a_hibernated_session_settles_and_resumes() {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Exited, "exit_unverified", 1);
    let session = with_row(&mut rig, row, &real_boot()).await;
    rig.store()
        .append(
            session.clone(),
            vec![Event::SessionHibernated { after_min: 30 }.into()],
        )
        .await
        .unwrap();
    rig.accepted(SessionCommand::Prompt {
        session_id: session.clone(),
        input: UserInput::new("again".to_owned()),
    })
    .await;
    let (proofs, events) = settled(&rig, &session).await;
    assert_eq!(proofs, vec![Some("checked_after_exit".to_owned())]);
    assert_eq!(events, vec![Some(ReconciledBy::Station)]);
    rig.wait_state(&session, SessionState::Running).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_owner_reconcile_still_writes_no_by_field() {
    let mut rig = Rig::direct(None).await;
    let row = (ShimState::Lost, "execution_unknown", 0);
    let session = with_row(&mut rig, row, &real_boot()).await;
    // The census can be incomplete on a busy machine: try again.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let d = rig.send(reconcile(&session, "run")).await;
        let incomplete = d
            .receipt
            .reject
            .as_ref()
            .is_some_and(|r| r.message.contains("incomplete"));
        if !incomplete {
            assert!(d.receipt.reject.is_none(), "{:?}", d.receipt);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{:?}", d.receipt);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let events = rig.events_of(Some(&session)).await;
    let event = events
        .iter()
        .find(|e| matches!(e, Event::ExecutionReconciled { .. }))
        .unwrap();
    let json = serde_json::to_value(event).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "session.execution_reconciled", "shim_id": "run"})
    );
    assert_eq!(settled(&rig, &session).await.0, vec![None]);
    rig.manager.shutdown(StopMode::Kill).await;
}
