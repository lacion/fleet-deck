//! App tool calls across a station restart under a shim (B15 T5b, design
//! `docs/v2/design/mcp-apps.md` §4.6.1): a call whose answer is delayed
//! survives the stop of the station as an `uncertain` row (cause
//! `station_restart`) that keeps the folder barrier; the bridge of the
//! adopted session answers; an item that the provider completes while no
//! station runs is captured; a provider timeout of a non-read-only tool keeps
//! the barrier until the owner releases it; the end of the provider settles
//! the rows of its process. Real shims, the fake Codex app-server (scenario
//! `mcp_app`), a Git workspace.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::{
    AppCallParams, AppCallReply, AppCallState, AppRequest, AppUncertainCause,
};
use fleetdeck_proto::command::StopMode;
use fleetdeck_station::apps::calls::{self, code};
use fleetdeck_station::store::app_calls::{AppCallBarrier, CallState};
use serde_json::json;

mod mcp_apps_rig;

use mcp_apps_rig::{Rig, Station, until, wait_tool_apps};

fn params(session: &SessionId, app: &str, tool: &str, confirmed: bool) -> AppCallParams {
    AppCallParams {
        session_id: session.clone(),
        app_id: app.to_owned(),
        request: AppRequest::ToolsCall {
            name: tool.to_owned(),
            arguments: json!({}),
        },
        confirmed,
    }
}

async fn call(
    station: &Station,
    session: &SessionId,
    app: &str,
    tool: &str,
    confirmed: bool,
) -> Result<AppCallReply, calls::AppFail> {
    let host = station.manager.app_host(session);
    calls::app_call(
        &station.manager.app_env(),
        host,
        params(session, app, tool, confirmed),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_calls_survive_a_station_restart() {
    let mut rig = Rig::new();
    let first = rig.station().await;
    let session = rig.start_session(&first, "hello").await;
    let app = wait_tool_apps(&first.store, &session, 1).await.remove(0);

    // A `bump` call with `confirmed: true`; the sim answers after 3 s. While
    // it waits, a second turn is sent: the sim reads it after the answer, so
    // its `mcpToolCall` item completes while no station runs.
    let (env, host) = (
        first.manager.app_env(),
        first.manager.app_host(&session).expect("an app host"),
    );
    let pending_params = params(&session, &app, "bump", true);
    let pending =
        tokio::spawn(async move { calls::app_call(&env, Some(host), pending_params).await });
    let call_id = until("the bump row", || async {
        let rows = first
            .store
            .app_calls_open(session.clone(), None)
            .await
            .unwrap();
        rows.into_iter().next().map(|r| r.call_id)
    })
    .await;
    rig.prompt(&first, &session, "hello again").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The station stops; the shim and the provider survive.
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);
    let second = rig.station().await;
    pending.abort();

    // (a) The delayed call's row is `uncertain` with the cause
    // `station_restart` (the detach ended no wait with `AppError::Ended`), and
    // the barrier of the folder is `Running`.
    let row = second
        .store
        .app_call(call_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, CallState::Uncertain, "{row:?}");
    assert_eq!(row.cause, Some(AppUncertainCause::StationRestart));
    assert_eq!(
        second
            .store
            .app_call_barrier(row.worktree_root.clone(), "no-turn".to_owned())
            .await
            .unwrap(),
        Some(AppCallBarrier::Running)
    );

    // (b) The adopted session has a bridge: a read-only call answers.
    until("the app host", || async {
        second.manager.app_host(&session).map(|_| ())
    })
    .await;
    let reply = call(&second, &session, &app, "show_counter", false)
        .await
        .expect("show_counter answers, not session_stopped");
    assert!(matches!(reply, AppCallReply::Done { .. }), "{reply:?}");

    // (c) The item that the sim completed while the station was down is
    // captured after the restart.
    let apps = wait_tool_apps(&second.store, &session, 2).await;
    assert_eq!(apps.len(), 2);

    // (e) A `slow_bump` call with `confirmed: true`: the provider answers its
    // own timeout, the row is `uncertain` with the cause `provider_error`, and
    // the barrier still holds after the sim wrote `slow_bump.txt`.
    let err = call(&second, &session, &app, "slow_bump", true)
        .await
        .expect_err("a provider error");
    assert_eq!(err.code, code::PROVIDER_ERROR, "{err}");
    assert!(err.message.contains("may still run"), "{err}");
    let env = second.manager.app_env();
    let listed = calls::app_calls(&env, second.manager.app_host(&session), session.clone())
        .await
        .unwrap();
    let slow = listed
        .calls
        .iter()
        .find(|c| c.tool == "slow_bump")
        .expect("the slow_bump call is listed");
    assert_eq!(slow.state, AppCallState::Uncertain);
    assert_eq!(slow.cause, Some(AppUncertainCause::ProviderError));
    let written = std::path::Path::new(&rig.workspace()).join("slow_bump.txt");
    until("slow_bump.txt", || async { written.exists().then_some(()) }).await;
    assert_eq!(
        second
            .store
            .app_call_barrier(row.worktree_root.clone(), "no-turn".to_owned())
            .await
            .unwrap(),
        Some(AppCallBarrier::Running)
    );
    let released = calls::app_release(&env, session.clone(), slow.call_id.clone())
        .await
        .unwrap();
    assert!(released.released);
    let again = calls::app_calls(&env, second.manager.app_host(&session), session.clone())
        .await
        .unwrap();
    assert!(again.calls.iter().all(|c| c.call_id != slow.call_id));

    // (d) The provider ends (`exit-after`): the rows of its process settle
    // `ended`. The `uncertain` row of (a) belongs to that process.
    rig.prompt(&second, &session, "exit-after bye").await;
    until("the row of (a) ends", || async {
        let row = second
            .store
            .app_call(call_id.clone())
            .await
            .unwrap()
            .unwrap();
        (row.state == CallState::Ended).then_some(())
    })
    .await;
    second.manager.shutdown(StopMode::Graceful).await;
}
