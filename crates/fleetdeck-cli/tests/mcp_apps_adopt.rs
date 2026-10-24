//! The adoption of a session under a shim with MCP Apps (B15 T5b, design
//! `docs/v2/design/mcp-apps.md` §4.3 items 1 and 2): a new shim session
//! reaches the capture service, `adopt` registers the bridge and hands over
//! the candidates that the restored driver replayed, and a restored session
//! whose exit is committed gets no bridge and no app. Real shims, the fake
//! Codex app-server (scenario `mcp_app`), a Git workspace.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::command::StopMode;
use fleetdeck_proto::event::{Event, SessionState};
use fleetdeck_providers::host::StopPolicy;
use fleetdeck_station::manager::durable_configs;
use fleetdeck_station::manager::shim_station::recover_station;
use fleetdeck_station::manager::target::SessionTarget;
use fleetdeck_station::workspace::{GitOptions, PrivatePaths, settle_unsettled};

mod mcp_apps_rig;

use mcp_apps_rig::{
    Rig, SLOW_MS, WAIT, events, started_turns, tool_apps, wait_for, wait_tool_apps,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shim_candidates_reach_capture() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let session = rig.start_session(&station, "hello").await;
    // One tool.app event and one app row for the one `mcpToolCall` item.
    let apps = wait_tool_apps(&station.store, &session, 1).await;
    assert_eq!(apps.len(), 1);
    assert_eq!(station.store.app_count().await.unwrap(), 1);
    // The session has a bridge, and it is the one of the session's process.
    let host = station.manager.app_host(&session).expect("an app host");
    let process = station.store.live_process(session.clone()).await.unwrap();
    assert_eq!(Some(host.process), process);
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adopt_registers_the_bridge_and_drains_replayed_candidates() {
    let mut rig = Rig::new();
    let first = rig.station().await;
    // A slow turn: its `mcpToolCall` item completes after the delay, while no
    // station runs.
    let session = rig
        .start_session(&first, &format!("slow {SLOW_MS} one"))
        .await;
    wait_for(&first.store, &session, "the slow turn", |e| {
        started_turns(e).len() == 1
    })
    .await;
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(SLOW_MS + 1500)).await;
    // The steps of a station start, with a deterministic delay between the
    // restoration and the adoption.
    let store = rig.open_store().await;
    settle_unsettled(&store, GitOptions::isolated())
        .await
        .unwrap();
    let from = *store.subscribe().borrow();
    let settings = rig.shim_settings(&store);
    let start = recover_station(
        &store,
        &settings.host,
        &durable_configs(&rig.adapters()),
        settings.in_flight_wait,
    )
    .await
    .unwrap();
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    let kept = start.restored[0].process.clone();
    // The restored driver replays the turn and its item before the adoption;
    // it keeps the candidate until the sink attaches.
    wait_for(&store, &session, "the turn completed", |e| {
        let started = started_turns(e);
        !started.is_empty()
            && e.iter().any(
                |e| matches!(e, Event::TurnCompleted { turn_id, .. } if *turn_id == started[0]),
            )
    })
    .await;
    assert!(tool_apps(&events(&store, &session).await).is_empty());
    let target = SessionTarget::new(store.clone(), rig.adapters(), rig.det.ids())
        .with_shims(settings)
        .with_private(Arc::new(PrivatePaths::default()));
    target
        .adopt(start.restored, StopPolicy::default(), from)
        .await;
    let host = target.app_host(&session).expect("an app host after adopt");
    assert_eq!(host.process, kept);
    // The kept candidate is handed over and captured.
    let apps = wait_tool_apps(&store, &session, 1).await;
    assert_eq!(apps.len(), 1);
    assert_eq!(store.app_count().await.unwrap(), 1);
    target.stop_all(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adopt_of_an_exited_session_registers_no_bridge() {
    let mut rig = Rig::new();
    let first = rig.station().await;
    // A slow first turn, and a follow-up that the adapter holds until it
    // ends; the follow-up turn answers and the app-server exits
    // (`exit-after`). Both items complete while no station runs.
    let session = rig
        .start_session(&first, &format!("slow {SLOW_MS} one"))
        .await;
    wait_for(&first.store, &session, "the slow turn", |e| {
        started_turns(e).len() == 1
    })
    .await;
    let follow_up = fleetdeck_proto::command::SessionCommand::FollowUp {
        session_id: session.clone(),
        input: fleetdeck_proto::command::UserInput::new("exit-after bye".to_owned()),
    };
    rig.send(&first, follow_up).await;
    let shim_id = first
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let transitions = first.store.shim_transitions(shim_id.clone()).await.unwrap();
        let checkpoint = first
            .store
            .shim_checkpoint(shim_id.clone())
            .await
            .unwrap()
            .unwrap_or_default();
        if !transitions.is_empty() || checkpoint.contains("exit-after bye") {
            break;
        }
        assert!(Instant::now() < deadline, "the follow-up was not committed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(tool_apps(&events(&first.store, &session).await).is_empty());
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(SLOW_MS + 1500)).await;
    let store = rig.open_store().await;
    settle_unsettled(&store, GitOptions::isolated())
        .await
        .unwrap();
    let from = *store.subscribe().borrow();
    let settings = rig.shim_settings(&store);
    let start = recover_station(
        &store,
        &settings.host,
        &durable_configs(&rig.adapters()),
        settings.in_flight_wait,
    )
    .await
    .unwrap();
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    // The restored driver commits the follow-up turn and the provider's exit
    // before the adoption.
    wait_for(&store, &session, "the exit", |e| {
        e.iter().any(|e| matches!(e, Event::SessionEnded { .. }))
    })
    .await;
    assert_eq!(
        store.summary(session.clone()).await.unwrap().unwrap().state,
        SessionState::Exited
    );
    let target = SessionTarget::new(store.clone(), rig.adapters(), rig.det.ids())
        .with_shims(settings)
        .with_private(Arc::new(PrivatePaths::default()));
    target
        .adopt(start.restored, StopPolicy::default(), from)
        .await;
    assert!(target.app_host(&session).is_none(), "no bridge");
    // Give the capture a chance to run; it must find nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(tool_apps(&events(&store, &session).await).is_empty());
    assert_eq!(store.app_count().await.unwrap(), 0);
    target.stop_all(StopMode::Graceful).await;
}
