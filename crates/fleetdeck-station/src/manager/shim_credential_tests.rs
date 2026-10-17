//! The failed launches of a start with tools (DL-194 T4a, §2.3): the
//! shim host cannot start here (its executable does not exist), so these
//! tests show what the launch does with the binding row and the memory
//! entry when the row insert or the start fails. The real shims are in
//! `crates/fleetdeck-cli/tests/shim_credential.rs`.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_testkit::Det;

use super::super::coordinator_start::{
    BridgeStart, prepare_bridge_fleet, revoke_stored, store_tools, unless_unsaved,
};
use super::{LaunchError, launch_shim_for, reason};
use crate::coordinator::fence::{Fence, FenceState};
use crate::coordinator::{BindingState, Bindings, BridgeEndpoint};
use crate::store::shim_bindings::RowState;
use crate::store::{NewSession, OfflineApprovalPolicy, Store, StoreConfig};

fn verified(_: ProviderKind, _: Option<&str>) -> Result<(), &'static str> {
    Ok(())
}

struct Rig {
    _tmp: tempfile::TempDir,
    store: Store,
    host: ShimHost,
    bindings: Bindings,
    spec: ProcessSpec,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        store
            .create_session(NewSession {
                session_id: "s1".into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let mut config = ShimHostConfig::new(
            PathBuf::from("/nonexistent/fleetdeck"),
            tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = tmp.path().to_owned();
        let bindings = Bindings::default().with_handoff(verified);
        bindings.set_bridge(BridgeEndpoint {
            program: "/station/bin/current".into(),
            socket: "/run/fleetdeck/mcp.s".into(),
        });
        let spec = ProcessSpec {
            program: PathBuf::from("/nonexistent/claude"),
            args: Vec::new(),
            cwd: tmp.path().to_owned(),
            env: ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]),
            launch: None,
        };
        Self {
            _tmp: tmp,
            store,
            host: ShimHost::new(config),
            bindings,
            spec,
        }
    }

    fn tools(&self) -> BridgeStart {
        let spec = SessionSpec {
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
        };
        prepare_bridge_fleet(&self.bindings, ProviderKind::Claude, &"s1".into(), &spec)
            .unwrap_or_else(|why| panic!("no bridge start: {why}"))
    }

    async fn launch(&self, tools: BridgeStart) -> Result<(), LaunchError> {
        launch_shim_for(
            &self.store,
            &self.host,
            &"s1".into(),
            &self.spec,
            AckMode::Manual,
            None,
            &|| Box::pin(std::future::ready(None)),
            &self.bindings,
            Some(tools),
        )
        .await
        .map(|_| ())
    }
}

/// The start fails after the row was stored: the row ends and the memory
/// entry refuses the token before the launch is settled.
#[tokio::test]
async fn a_failed_start_ends_the_stored_row_and_refuses_the_token() {
    let rig = Rig::new().await;
    let tools = rig.tools();
    let digest = tools.digest.clone();
    let binding_id = tools.binding.binding_id.clone();
    // The start takes the secrets path (the hash of the executable is
    // needed first) and fails: the executable does not exist.
    let Err(LaunchError::Settled { error, .. }) = rig.launch(tools).await else {
        panic!("the start must fail");
    };
    assert!(error.contains("cannot be hashed"), "{error}");
    let rows = rig.store.shim_bindings_load().await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].digest, digest);
    assert_eq!(rows[0].binding_id, binding_id);
    assert_eq!(rows[0].state, RowState::Ended);
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

/// The row insert fails: the start goes on without tools, and neither a row
/// nor a memory entry exists.
#[tokio::test]
async fn a_failed_row_insert_leaves_no_row_and_no_memory_entry() {
    let rig = Rig::new().await;
    rig.store
        .db()
        .call(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_binding_insert BEFORE INSERT ON shim_bindings
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let tools = rig.tools();
    let digest = tools.digest.clone();
    let binding_id = tools.binding.binding_id.clone();
    // The start takes the plain path (no hash of the executable) and fails.
    let Err(LaunchError::Settled { error, .. }) = rig.launch(tools).await else {
        panic!("the start must fail");
    };
    assert!(!error.contains("cannot be hashed"), "{error}");
    assert!(rig.store.shim_bindings_load().await.unwrap().is_empty());
    assert!(!rig.bindings.has_digest(&digest));
    assert!(rig.bindings.get(&binding_id).is_none());
}

/// A stored row of `shim-1` and a fence in the rig; returns the digest, the
/// stored pair and the fence.
async fn stored_with_fence(
    rig: &Rig,
) -> (
    String,
    Option<(String, ProviderKind)>,
    std::sync::Arc<Fence>,
) {
    let fence = std::sync::Arc::new(Fence::new(rig._tmp.path().join("run")));
    fence.ensure().unwrap();
    rig.bindings.set_fence(std::sync::Arc::clone(&fence));
    let tools = rig.tools();
    let digest = tools.digest.clone();
    let (_, stored) = store_tools(&rig.store, &rig.bindings, "shim-1", Some(tools)).await;
    assert!(stored.is_some());
    (digest, stored, fence)
}

async fn fail_row_updates(rig: &Rig) {
    rig.store
        .db()
        .call(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_binding_update BEFORE UPDATE ON shim_bindings
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// An unreadable fence takes no write.
fn break_fence(rig: &Rig) {
    std::fs::write(rig._tmp.path().join("run/bindings.fence"), "junk\n").unwrap();
}

fn entries(fence: &Fence) -> usize {
    match fence.read() {
        FenceState::Entries(e) => e.len(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_cleanup_with_both_writes_ok_is_durable_and_leaves_the_fence_empty() {
    let rig = Rig::new().await;
    let (digest, stored, fence) = stored_with_fence(&rig).await;
    assert!(revoke_stored(&rig.store, &rig.bindings, "shim-1", stored.as_ref()).await);
    assert_eq!(entries(&fence), 0);
    assert_eq!(
        rig.store.shim_bindings_load().await.unwrap()[0].state,
        RowState::Ended
    );
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

#[tokio::test]
async fn a_cleanup_with_a_failed_fence_and_an_ended_row_is_durable() {
    let rig = Rig::new().await;
    let (digest, stored, _fence) = stored_with_fence(&rig).await;
    break_fence(&rig);
    assert!(revoke_stored(&rig.store, &rig.bindings, "shim-1", stored.as_ref()).await);
    assert_eq!(
        rig.store.shim_bindings_load().await.unwrap()[0].state,
        RowState::Ended
    );
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

#[tokio::test]
async fn a_cleanup_with_a_fence_entry_and_a_failed_row_is_durable_and_keeps_the_entry() {
    let rig = Rig::new().await;
    let (digest, stored, fence) = stored_with_fence(&rig).await;
    fail_row_updates(&rig).await;
    assert!(revoke_stored(&rig.store, &rig.bindings, "shim-1", stored.as_ref()).await);
    assert_eq!(
        fence.read(),
        FenceState::Entries(std::collections::BTreeSet::from([(
            "shim-1".to_owned(),
            digest.clone()
        )]))
    );
    assert_eq!(
        rig.store.shim_bindings_load().await.unwrap()[0].state,
        RowState::Pending
    );
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

#[tokio::test]
async fn a_cleanup_with_both_writes_failed_is_not_durable() {
    let rig = Rig::new().await;
    let (digest, stored, _fence) = stored_with_fence(&rig).await;
    break_fence(&rig);
    fail_row_updates(&rig).await;
    assert!(!revoke_stored(&rig.store, &rig.bindings, "shim-1", stored.as_ref()).await);
    // The memory gate holds in every case.
    assert_eq!(
        rig.bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
}

#[tokio::test]
async fn a_cleanup_with_nothing_stored_is_a_durable_no_op() {
    let rig = Rig::new().await;
    assert!(revoke_stored(&rig.store, &rig.bindings, "shim-1", None).await);
}

/// `launch_shim_for` cannot reattach in a unit test (the shim executable
/// does not exist here), so these tests run the decision that it applies to
/// a settlement, with a fake attachment and a fake kill.
#[tokio::test]
async fn a_reattaching_settlement_after_a_cleanup_that_is_not_durable_kills_and_fails() {
    let killed = std::cell::Cell::new(false);
    let settled: Result<u32, LaunchError> = Ok(7);
    let out = unless_unsaved(false, settled, |_| async { killed.set(true) }).await;
    assert!(killed.get());
    let Err(LaunchError::Settled { error, settled }) = out else {
        panic!("the launch must fail");
    };
    assert_eq!(error, "the session credentials could not be saved");
    assert_eq!(settled, reason::EXECUTION_UNKNOWN);
}

#[tokio::test]
async fn a_durable_cleanup_or_a_failed_settlement_passes_through_without_a_kill() {
    let killed = std::cell::Cell::new(false);
    let kill = |_: u32| async { killed.set(true) };
    assert!(matches!(unless_unsaved(true, Ok(7), kill).await, Ok(7)));
    let failed: Result<u32, LaunchError> = Err(LaunchError::Settled {
        error: "x".to_owned(),
        settled: reason::EXECUTION_UNKNOWN,
    });
    let out = unless_unsaved(false, failed, kill).await;
    assert!(matches!(out, Err(LaunchError::Settled { ref error, .. }) if error == "x"));
    assert!(!killed.get());
}
