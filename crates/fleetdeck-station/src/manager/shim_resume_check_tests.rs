//! Resume on the shim host (DL-182, design §5 and §4.2): the hibernation of
//! a running shim session, the owner's execution check and the owner's
//! reconciliation. See `shim_resume_tests.rs`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState, StopMode};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, EventBody, RejectCode};
use fleetdeck_providers::host::shim::identity::identity_of;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;

use super::shim_resume_rig::*;
use super::*;
use crate::coordinator::Bindings;
use crate::store::{NewShim, RunningFacts, ShimState};

// --- hibernation (test 14) ----------------------------------------------

#[tokio::test]
async fn a_hibernation_stops_the_running_execution_but_not_behind_an_unresolved_earlier_row() {
    let mut rig = Rig::direct(None).await;
    let normal = rig.start("claude").await;
    let behind = rig.start("claude").await;
    let id = dead_identity(&real_boot());
    // `normal` runs under one shim; `behind` has an unresolved earlier one.
    rig.row_for(
        &behind,
        "early",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(id.clone()),
        None,
        None,
    )
    .await;
    for (session, shim) in [(&normal, "run-a"), (&behind, "run-b")] {
        rig.store()
            .shim_starting(NewShim {
                shim_id: shim.to_owned(),
                session_id: session.clone(),
                socket_path: PathBuf::from(format!("/nonexistent/{shim}.s")),
                launch_boot_id: Some(real_boot()),
                native_target: None,
            })
            .await
            .unwrap();
        rig.store()
            .shim_running_with(shim.to_owned(), id.clone(), RunningFacts::default())
            .await
            .unwrap();
    }
    rig.det
        .advance(Duration::from_millis(u64::from(AFTER) * 60_000 + 1));
    let (store, policy) = (rig.store(), rig.manager.policy());
    let swept = hibernate::sweep(&store, &policy, &Bindings::default(), AFTER)
        .await
        .unwrap();
    assert_eq!(
        swept, 1,
        "only the session without an earlier unresolved row"
    );
    rig.wait_state(&normal, SessionState::Exited).await;
    assert!(
        store
            .summary(normal.clone())
            .await
            .unwrap()
            .unwrap()
            .hibernated
    );
    assert!(
        !store
            .summary(behind.clone())
            .await
            .unwrap()
            .unwrap()
            .hibernated
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

// --- the owner's check and reconciliation (tests 8, 19, 20) -------------

/// A provider group: the leader is dead, a member lives on in its group.
struct Orphan {
    pgid: u32,
    identity: ProviderIdentity,
}

impl Orphan {
    fn start() -> Self {
        use std::os::unix::process::CommandExt as _;
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 120 & wait"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pgid = leader.id();
        // Wait until the member runs, then end the leader.
        let identity = identity_of(pgid).unwrap();
        for _ in 0..200 {
            if fleetdeck_providers::host::shim::inspect::group_members(pgid) == Some(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        leader.kill().unwrap();
        leader.wait().unwrap();
        Self { pgid, identity }
    }

    fn end(&self) {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.pgid)])
            .status();
    }
}

impl Drop for Orphan {
    fn drop(&mut self) {
        self.end();
    }
}

#[tokio::test]
async fn the_owner_check_lists_what_still_runs_and_the_reconciliation_waits_for_it_to_end() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    let orphan = Orphan::start();
    let mut identity = orphan.identity.clone();
    identity.boot_id = real_boot();
    rig.row(
        &session,
        "gone",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(identity),
    )
    .await;
    let (code, _) = rig.refused(resume(&session)).await;
    assert_eq!(code, RejectCode::InvalidState);

    let check = rig.manager.execution_check(&session).await.unwrap();
    assert_eq!(check.shim_id.as_deref(), Some("gone"));
    assert!(check.reason.contains("execution_unknown"), "{check:?}");
    assert!(check.same_boot && check.complete, "{check:?}");
    assert!(!check.identity_alive);
    assert_eq!(check.group_members.len(), 1, "{check:?}");
    assert_eq!(check.group_members[0].command, "sleep");
    // The reconciliation is refused while the member lives.
    let (code, message) = rig.refused(reconcile(&session, "gone")).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("still has processes"), "{message}");
    assert_eq!(rig.rows(&session).await[0].proof, None);

    orphan.end();
    let deadline = tokio::time::Instant::now() + WAIT;
    while !rig
        .manager
        .execution_check(&session)
        .await
        .unwrap()
        .group_members
        .is_empty()
    {
        assert!(tokio::time::Instant::now() < deadline, "the member lives");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let d = rig.accepted(reconcile(&session, "gone")).await;
    // The receipt, the event and the proof; state and reason stay.
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert!(d.events.iter().any(|e| matches!(
        &e.event,
        EventBody::Known(Event::ExecutionReconciled { shim_id }) if shim_id == "gone"
    )));
    let row = &rig.rows(&session).await[0];
    assert_eq!(row.proof.as_deref(), Some("reconciled_by_owner"));
    assert_eq!(
        (row.state, row.reason.as_deref()),
        (ShimState::Lost, Some("execution_unknown"))
    );
    assert_eq!(
        rig.manager.execution_check(&session).await.unwrap().shim_id,
        None,
        "nothing is unresolved any more"
    );
    // A repeated reconciliation is refused; the session resumes.
    let (code, message) = rig.refused(reconcile(&session, "gone")).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("already proven"), "{message}");
    rig.accepted(resume(&session)).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_reconciliation_refuses_a_running_row_a_foreign_origin_and_unknown_names() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    let other = rig.ended("claude").await;
    let id = dead_identity(&real_boot());
    rig.row(
        &session,
        "gone",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(id.clone()),
    )
    .await;
    // A running row of the same session is never reconciled.
    rig.store()
        .shim_starting(NewShim {
            shim_id: "running".to_owned(),
            session_id: session.clone(),
            socket_path: PathBuf::from("/nonexistent/running.s"),
            launch_boot_id: Some(real_boot()),
            native_target: None,
        })
        .await
        .unwrap();
    rig.store()
        .shim_running_with("running".to_owned(), id, RunningFacts::default())
        .await
        .unwrap();
    let (code, message) = rig.refused(reconcile(&session, "running")).await;
    assert_eq!(
        (code, message.as_str()),
        (RejectCode::InvalidState, "the shim is running")
    );
    // Only the board and the local socket reconcile.
    for origin in [CommandOrigin::Mail, CommandOrigin::Coordinator] {
        let d = rig.submit_as(origin, reconcile(&session, "gone")).await;
        assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    }
    let d = rig
        .submit_as(CommandOrigin::Board, reconcile(&session, "gone"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    // A shim of another session, an unknown shim and an unknown session.
    let (code, message) = rig.refused(reconcile(&other, "gone")).await;
    assert_eq!(
        (code, message.as_str()),
        (RejectCode::InvalidCommand, "unknown shim")
    );
    let (code, _) = rig.refused(reconcile(&session, "nope")).await;
    assert_eq!(code, RejectCode::InvalidCommand);
    let (code, _) = rig.refused(reconcile(&"nope".into(), "gone")).await;
    assert_eq!(code, RejectCode::UnknownSession);
    // The check ignores the running row.
    assert_eq!(
        rig.manager.execution_check(&session).await.unwrap().shim_id,
        None
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_legacy_clean_exit_is_unverified_and_one_complete_check_makes_it_resumable() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    // `exited/exited` without the inspection version (test 19).
    rig.row(
        &session,
        "legacy",
        ShimState::Exited,
        "exited",
        0,
        Some(dead_identity(&real_boot())),
    )
    .await;
    let (code, message) = rig.refused(resume(&session)).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(
        message.contains("shim legacy is exited (exit_unverified)"),
        "{message}"
    );
    let check = rig.manager.execution_check(&session).await.unwrap();
    assert!(
        check.complete && check.same_boot && !check.still_runs(),
        "{check:?}"
    );
    rig.accepted(reconcile(&session, "legacy")).await;
    rig.accepted(resume(&session)).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_incomplete_check_never_reconciles() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    // The same boot, but no provider identity: nothing to look for.
    rig.row_for(
        &session,
        "ghost",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
        Some(&real_boot()),
        None,
    )
    .await;
    let check = rig.manager.execution_check(&session).await.unwrap();
    assert!(!check.complete, "{check:?}");
    assert!(check.error.is_some());
    let (code, message) = rig.refused(reconcile(&session, "ghost")).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(
        message.contains("the process check is incomplete"),
        "{message}"
    );
    // No boot evidence at all is incomplete too.
    rig.row_for(
        &session,
        "blank",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
        None,
        None,
    )
    .await;
    let (code, message) = rig.refused(reconcile(&session, "blank")).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(
        message.contains("the process check is incomplete"),
        "{message}"
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn on_another_boot_the_check_has_no_members_and_is_complete() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    rig.row(
        &session,
        "old",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity("an-older-boot")),
    )
    .await;
    let check = rig.manager.execution_check(&session).await.unwrap();
    assert!(!check.same_boot && check.complete, "{check:?}");
    assert!(!check.identity_alive && check.group_members.is_empty());
    assert!(check.session_members.is_empty() && check.error.is_none());
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_session_without_an_unresolved_row_has_nothing_to_check() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    let check = rig.manager.execution_check(&session).await.unwrap();
    assert_eq!(check.shim_id, None);
    assert_eq!(check.reason, "nothing unresolved");
    assert!(check.complete);
    rig.manager.shutdown(StopMode::Kill).await;
}
