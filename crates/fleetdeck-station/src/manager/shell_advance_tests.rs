//! The advance task and the shutdown (B8): a task that waits for a held
//! folder fence must stop at the shutdown.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandEnvelope, CommandId};
use fleetdeck_testkit::Det;
use tokio::sync::{Notify, watch};

use super::task;
use crate::manager::turns::Fences;
use crate::store::shell_runs::ShellState;
use crate::store::{Caller, NewSession, OfflineApprovalPolicy, Store, StoreConfig};

const SESSION: &str = "s1";
const ROOT: &str = "/w";
const EFFECT: &str = "effect-1";

/// A store with one session and one settled shell row that waits for its
/// advance.
async fn store_with_a_settled_row(det: &Det) -> Store {
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: SESSION.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: ROOT.to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("plant-1"),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Interrupt {
            session_id: SESSION.into(),
        },
    };
    let decided = store
        .decide(envelope, Caller::local(), |d| {
            d.shell_insert(EFFECT, &SESSION.into(), ProviderKind::Claude, ROOT, "ls")?;
            Ok(CommandResult {
                session_id: Some(SESSION.into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let settled = store
        .shell_settle(EFFECT.to_owned(), ShellState::Done, Some(0), None, None)
        .await
        .unwrap();
    assert!(settled);
    store
}

#[tokio::test]
async fn the_advance_task_stops_at_the_shutdown_while_it_waits_for_the_fence() {
    let det = Det::new();
    let store = store_with_a_settled_row(&det).await;
    assert_eq!(store.shell_pending_advances().await.unwrap().len(), 1);
    let fences = Fences::default();
    let fence = fences.for_worktree(Path::new(ROOT));
    let _held = fence.lock().await;
    let (tx, rx) = watch::channel(false);
    let handle = tokio::spawn(task(
        store.clone(),
        fences.clone(),
        Arc::new(Notify::new()),
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(true).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(ended.is_ok(), "the task stops while it waits for the fence");
    let row = store.shell_run(EFFECT.to_owned()).await.unwrap().unwrap();
    assert!(row.advance_pending, "{row:?}");
}
