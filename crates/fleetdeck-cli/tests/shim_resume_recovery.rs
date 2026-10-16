//! Resume on the shim host after a loss (DL-182, design §4, tests 6, 8, 10,
//! 12): a shim that a crash or a reboot took away, the termination
//! evidence that recovery records, and the owner's reconciliation. Real
//! shims and the fake agents.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fleetdeck_proto::command::{CommandOrigin, SessionCommand, StopMode};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, EventBody, RejectCode};
use fleetdeck_station::store::{EffectPayload, EffectState, ShimState};

mod shim_resume_rig;
use shim_resume_rig::*;

/// Design §7 test 6 (a verified reboot) and 12 (the proof holds at the
/// dispatch): the row becomes `ended_by_reboot`, the session stays `lost`
/// and resumes under a new shim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gone_shim_after_a_reboot_is_proven_ended_and_its_session_resumes() {
    let mut rig = Rig::new();
    let (session, shim_id) = lost_under_a_dead_shim(&mut rig, true).await;
    let st = rig.station().await;
    let row = st.store.shim(shim_id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShimState::Lost);
    assert_eq!(
        row.reason.as_deref(),
        Some("execution_unknown"),
        "the state stays"
    );
    assert_eq!(row.proof.as_deref(), Some("ended_by_reboot"));
    assert_eq!(row.proof_boot_id.as_deref(), Some(real_boot().as_str()));
    assert_eq!(state(&st.store, &session).await, SessionState::Lost);
    let native = starts(&st.store, &session).await[0].1.clone();

    rig.send(
        &st,
        SessionCommand::Resume {
            session_id: session.clone(),
        },
    )
    .await;
    prompt_when_live(&mut rig, &st, &session, "after the reboot").await;
    wait_turns(&st.store, &session, 2).await;
    let after = starts(&st.store, &session).await;
    assert_eq!(
        (after[1].0, after[1].1.as_str()),
        (Some(true), native.as_str())
    );
    let rows = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].state,
        ShimState::Lost,
        "the old row stays as it was"
    );
    assert_eq!(rows[1].state, ShimState::Running);
    st.manager.shutdown(StopMode::Graceful).await;
    drop(st);
}

fn real_boot() -> String {
    fleetdeck_providers::host::shim::identity::boot_id().unwrap()
}

/// Design §7 test 6 (the same boot, an unreadable boot id) and test 8: no
/// proof without a changed boot id; the owner's check finds nothing alive
/// and the reconciliation makes the session resumable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_changed_boot_id_the_session_needs_the_owner_check_and_reconciliation() {
    let mut rig = Rig::new();
    let (session, shim_id) = lost_under_a_dead_shim(&mut rig, false).await;
    // An unreadable boot id: recovery proves nothing, and the launch of a
    // new shim cannot start either.
    rig.set_boot(Boot::Unreadable);
    let st = rig.station().await;
    let row = st.store.shim(shim_id.clone()).await.unwrap().unwrap();
    assert_eq!(row.proof, None);
    st.manager.shutdown(StopMode::Graceful).await;
    rig.close(st).await;
    // The same boot id: no proof either.
    rig.set_boot(Boot::Real);
    let st = rig.station().await;
    let row = st.store.shim(shim_id.clone()).await.unwrap().unwrap();
    assert_eq!((row.state, row.proof.as_deref()), (ShimState::Lost, None));
    let (code, message) = rig
        .rejected(
            &st,
            SessionCommand::Resume {
                session_id: session.clone(),
            },
        )
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(
        message.contains("not_resumable") && message.contains("execution_unknown"),
        "{message}"
    );

    // The check: the same boot, nothing alive of that run, complete.
    let check = st.manager.execution_check(&session).await.unwrap();
    assert_eq!(check.shim_id.as_deref(), Some(shim_id.as_str()));
    assert!(
        check.same_boot && check.complete && !check.still_runs(),
        "{check:?}"
    );
    // The owner marks the run as ended: receipt, event, proof.
    let d = rig
        .send(
            &st,
            SessionCommand::ReconcileExecution {
                session_id: session.clone(),
                shim_id: shim_id.clone(),
            },
        )
        .await;
    assert!(d.events.iter().any(|e| matches!(
        &e.event,
        EventBody::Known(Event::ExecutionReconciled { shim_id: id }) if *id == shim_id
    )));
    let row = st.store.shim(shim_id.clone()).await.unwrap().unwrap();
    assert_eq!(row.proof.as_deref(), Some("reconciled_by_owner"));
    assert_eq!(row.state, ShimState::Lost);
    // The session resumes under a new shim.
    rig.send(
        &st,
        SessionCommand::Resume {
            session_id: session.clone(),
        },
    )
    .await;
    prompt_when_live(&mut rig, &st, &session, "after the reconciliation").await;
    wait_turns(&st.store, &session, 2).await;
    assert_eq!(st.store.shim_rows(session.clone()).await.unwrap().len(), 2);
    st.manager.shutdown(StopMode::Graceful).await;
    drop(st);
    let _ = CommandOrigin::Local;
}

/// Design §7 test 10: the prompt of a resume that a crash interrupted after
/// the claim and before the write settles as `outcome_unknown` and is never
/// sent again (ADR-005 §4.3). The resume effect is claimed, nothing was
/// written, the station starts again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_claimed_before_a_crash_settles_as_outcome_unknown_and_is_never_resent() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let session = rig.ended(&st, "claude").await;
    st.manager.shutdown(StopMode::Graceful).await;
    // The claimed resume: the effect worker is not running in this rig of
    // the store alone, so the claim is made here.
    let store = st.store.clone();
    let summary = store.summary(session.clone()).await.unwrap().unwrap();
    let native = starts(&store, &session).await[0].1.clone();
    let spec = rig.spec("claude");
    let envelope = fleetdeck_proto::CommandEnvelope {
        authority: None,
        command_id: fleetdeck_proto::CommandId::from("cmd-crash"),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap()
            + fleetdeck_proto::command::RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Resume {
            session_id: session.clone(),
        },
    };
    let fence = store.register_worker().await.unwrap();
    let session_id = session.clone();
    store
        .decide(
            envelope,
            fleetdeck_station::store::Caller::local(),
            move |d| {
                d.enqueue(
                    &session_id,
                    EffectPayload::Resume {
                        native_session_id: native,
                        spec,
                        prompt: Some(fleetdeck_station::store::InputRecord {
                            input_id: "01J00000000000000000000009".into(),
                            origin: fleetdeck_proto::event::RunOrigin::Board,
                            input: text("never sent twice"),
                            shell_context: Vec::new(),
                        }),
                        continue_by: None,
                        import: false,
                        transcript: None,
                        authority: None,
                    },
                )?;
                Ok(fleetdeck_proto::command::CommandResult {
                    session_id: Some(session_id.clone()),
                    input_id: None,
                })
            },
        )
        .await
        .unwrap();
    let claimed = store.claim_next(fence).await.unwrap().unwrap();
    assert!(matches!(claimed.payload, EffectPayload::Resume { .. }));
    assert_eq!(summary.state, SessionState::Exited);
    // The test's own clone of the store closes with the station.
    drop(store);
    rig.close(st).await;

    let st = rig.station().await;
    let effects = st.store.session_effects(session.clone()).await.unwrap();
    let resume = effects
        .iter()
        .find(|e| matches!(e.payload, EffectPayload::Resume { .. }))
        .unwrap();
    assert_eq!(resume.state, EffectState::OutcomeUnknown, "{effects:?}");
    // It never ran: no new shim, no prompt at the provider.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(st.store.shim_rows(session.clone()).await.unwrap().len(), 1);
    assert_eq!(rig.log_count("claude", "never sent twice"), 0);
    st.manager.shutdown(StopMode::Graceful).await;
    drop(st);
}
