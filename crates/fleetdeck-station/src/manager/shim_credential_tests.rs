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

use super::super::coordinator_start::{BridgeStart, prepare_bridge_fleet};
use super::{LaunchError, launch_shim_for};
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
