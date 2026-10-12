//! Live shim exits (DL-120): the evidence of a surviving child, and a
//! proven end without survivors. Split from `shim_manager.rs` to keep
//! each module under 800 lines.

use super::*;

/// Waits until the session's process ended and its shim rows closed, and
/// returns the row of `shim_id` (DL-120).
async fn ended_row(
    store: &Store,
    session: &SessionId,
    shim_id: &str,
) -> fleetdeck_station::store::ShimRow {
    wait_for(store, session, "session.ended", |e| {
        e.iter().any(|e| matches!(e, Event::SessionEnded { .. }))
    })
    .await;
    let deadline = Instant::now() + WAIT;
    loop {
        let live = store.live_process(session.clone()).await.unwrap();
        let open = store.shims_of_session(session.clone()).await.unwrap();
        if live.is_none() && open.is_empty() {
            return store.shim(shim_id.to_owned()).await.unwrap().unwrap();
        }
        assert!(Instant::now() < deadline, "live {live:?}, open {open:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn resume(rig: &mut Rig, station: &Station, session: &SessionId) -> Decided {
    rig.next += 1;
    let envelope = CommandEnvelope {
        command_id: CommandId::from(format!("cmd-{}", rig.next)),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Resume {
            session_id: session.clone(),
        },
    };
    station
        .manager
        .submit(envelope, Caller::local())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let session = start_session(&mut rig, &station, "hello").await;
    finalized(&station.store, &session, 1).await;
    let shim_id = station
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput::new("exit-leaving-child bye".to_owned()),
        },
    )
    .await;
    // The station runs; its observer ends the session from the live exit.
    let row = ended_row(&station.store, &session, &shim_id).await;
    assert_eq!(row.state, fleetdeck_station::store::ShimState::Exited);
    assert_eq!(
        row.reason.as_deref(),
        Some("exited_descendants_remaining"),
        "{row:?}"
    );
    let summary = station
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    assert!(summary.capabilities.as_ref().unwrap().resume);
    let before = station
        .store
        .session_effects(session.clone())
        .await
        .unwrap()
        .len();
    let decided = resume(&mut rig, &station, &session).await;
    assert_eq!(decided.receipt.state, ReceiptState::Rejected, "{decided:?}");
    let reject = decided.receipt.reject.clone().unwrap();
    assert!(
        reject.message.contains("not_resumable")
            && reject.message.contains("exited_descendants_remaining"),
        "{reject:?}"
    );
    assert_eq!(
        station
            .store
            .session_effects(session.clone())
            .await
            .unwrap()
            .len(),
        before,
        "nothing queued"
    );
    assert_eq!(rig.method_count("thread/resume"), 0);
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_shim_exit_without_survivors_is_a_proven_end() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let session = start_session(&mut rig, &station, "hello").await;
    finalized(&station.store, &session, 1).await;
    let shim_id = station
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput::new("exit-after bye".to_owned()),
        },
    )
    .await;
    let row = ended_row(&station.store, &session, &shim_id).await;
    assert_eq!(row.reason.as_deref(), Some("exited"), "{row:?}");
    // The termination check passes; the shim host then refuses the
    // resume launch itself (DL-118), which is not a `not_resumable`
    // rejection.
    let decided = resume(&mut rig, &station, &session).await;
    if let Some(reject) = decided.receipt.reject.as_ref() {
        assert!(!reject.message.contains("not_resumable"), "{reject:?}");
    }
    station.manager.shutdown(StopMode::Graceful).await;
}
