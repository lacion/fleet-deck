//! R8.2 (DL-162): `station.info` reports how the probed version compares
//! with the tested minimum, and a start on an instance that is marked broken
//! ends with the refusal text and spawns no process.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::VersionStatus;
use fleetdeck_proto::{Capabilities, CommandEnvelope, CommandId, Event, EventBody};
use fleetdeck_providers::adapter::{AuthState, NativeSessionId, ProviderProbe};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{EventSink, LiveSession, ProviderAdapter};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use super::*;
use crate::store::{OfflineApprovalPolicy, ReadResult, StoreConfig};

const WAIT: Duration = Duration::from_secs(10);

/// The real Claude adapter with a chosen probe version.
struct Probed {
    inner: Arc<dyn ProviderAdapter>,
    version: &'static str,
}

#[async_trait]
impl ProviderAdapter for Probed {
    fn kind(&self) -> ProviderKind {
        self.inner.kind()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn probe(&self) -> fleetdeck_providers::adapter::Result<ProviderProbe> {
        Ok(ProviderProbe {
            kind: ProviderKind::Claude,
            version: Some(self.version.to_owned()),
            auth: AuthState::Unknown,
        })
    }
    async fn start(
        &self,
        spec: SessionSpec,
        sink: EventSink,
    ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
        self.inner.start(spec, sink).await
    }
    async fn resume(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
    ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
        self.inner.resume(native_id, spec, sink).await
    }
}

async fn manager(version: &'static str) -> (SessionManager, Det, tempfile::TempDir, String) {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("projects/app");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    let real: Arc<dyn ProviderAdapter> =
        Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(Probed {
        inner: real,
        version,
    });
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::from([("claude".into(), adapter)]),
        det.ids(),
    )
    .await
    .unwrap();
    (manager, det, root, cwd.to_string_lossy().into_owned())
}

#[tokio::test]
async fn station_info_marks_an_older_cli_and_accepts_a_tested_one() {
    let (old, _det, _root, _cwd) = manager("2.1.100").await;
    let providers = old.providers().await;
    assert_eq!(providers[0].version_status, VersionStatus::OlderThanTested);
    let (current, _det, _root, _cwd) =
        manager(fleetdeck_providers::claude::TESTED_MIN_VERSION).await;
    assert_eq!(
        current.providers().await[0].version_status,
        VersionStatus::Ok
    );
}

#[tokio::test]
async fn a_start_on_a_broken_version_is_refused_with_the_reason() {
    let (manager, det, _root, cwd) = manager("2.1.289").await;
    manager
        .target
        .mark_broken(&"claude".into(), ProviderKind::Claude);
    let envelope = CommandEnvelope {
        command_id: CommandId::from("cmd-1".to_owned()),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Start {
            spec: SessionSpec {
                provider_instance: "claude".into(),
                cwd,
                mode: Some("default".to_owned()),
                model: None,
                title: None,
                coordinator: None,
            },
            prompt: Some(UserInput::new("hello".to_owned())),
            workspace: None,
            setup_cmd: None,
        },
    };
    let decided = manager.submit(envelope, Caller::local()).await.unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ReadResult::Events { events, .. } =
            manager.store().read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        let refusal = events.iter().find_map(|e| match &e.event {
            EventBody::Known(Event::Error { code, message, .. }) if code == "start_failed" => {
                Some(message.clone())
            }
            _ => None,
        });
        if let Some(message) = refusal {
            assert_eq!(
                message,
                "this claude version is known to break the protocol"
            );
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "no start_failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
