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
        authority: None,
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

/// The live `sleep` processes whose working directory is under `root`: the
/// children that the simulated app-server leaves behind.
fn sleepers_under(root: &Path) -> Vec<u32> {
    let root = std::fs::canonicalize(root).unwrap();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let zombie = stat
            .rfind(')')
            .is_some_and(|i| stat[i + 1..].trim_start().starts_with('Z'));
        let is_sleep =
            std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|c| c.trim() == "sleep");
        let under =
            std::fs::read_link(entry.path().join("cwd")).is_ok_and(|cwd| cwd.starts_with(&root));
        if is_sleep && under && !zombie {
            found.push(pid);
        }
    }
    found
}

/// Runs one turn that ends the app-server with a child left behind
/// (`mode`), waits for the child, then for the closed row, and checks that
/// the shim stopped the child and the end is proven (DL-431).
async fn left_child_is_stopped_and_the_resume_is_admitted(mode: &str) {
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
            input: UserInput::new(format!("{mode} bye")),
        },
    )
    .await;
    // The child exists while the shim sweeps; the session ends after it.
    let deadline = Instant::now() + WAIT;
    while sleepers_under(rig.tmp.path()).is_empty() {
        assert!(Instant::now() < deadline, "the child never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = ended_row(&station.store, &session, &shim_id).await;
    assert_eq!(row.state, fleetdeck_station::store::ShimState::Exited);
    assert_eq!(row.reason.as_deref(), Some("exited"), "{row:?}");
    assert!(
        sleepers_under(rig.tmp.path()).is_empty(),
        "the shim stopped the child"
    );
    let decided = resume(&mut rig, &station, &session).await;
    // The shim host may refuse the resume launch itself (DL-118); that is
    // not a `not_resumable` rejection.
    if let Some(reject) = decided.receipt.reject.as_ref() {
        assert!(!reject.message.contains("not_resumable"), "{reject:?}");
    }
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_shim_exit_with_a_surviving_child_stops_it_and_the_resume_is_admitted() {
    left_child_is_stopped_and_the_resume_is_admitted("exit-leaving-child").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_exit_with_a_child_in_another_group_is_proven() {
    left_child_is_stopped_and_the_resume_is_admitted("exit-leaving-group-child").await;
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
