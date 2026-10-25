//! The app wiring of the session target (B15 T5a, design
//! `docs/v2/design/mcp-apps.md` §4.3 items 1 and 2): the bridge registry and
//! the candidates of a direct session reaching the capture service.
//!
//! The app sink of a session under a shim (kept candidates, hand-over order,
//! the closed sink of an exited restored session) is tested at its type, in
//! `durable_apps.rs`. A shim process needs the `fleetdeck` executable, which
//! the station crate does not have.
//! The adoption tests and the shim half of the capture test are in `crates/fleetdeck-cli/tests/mcp_apps_adopt.rs` (B15 T5b).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Capabilities, CommandEnvelope, CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::adapter::{NativeSessionId, ProviderProbe};
use fleetdeck_providers::apps::{AppBridge, AppCandidate, AppError, AppTool, ToolAnswer};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{
    DirectChildHost, EndCause, ExitReport, ExitWatch, StopPolicy, Termination,
};
use fleetdeck_providers::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, PermissionMode, ProviderAdapter,
};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{AppHost, HostEnd, direct_end};
use crate::manager::policy::Policy;
use crate::manager::turns::{Fences, SessionTurns};
use crate::manager::{Adapters, SessionManager, SessionTarget};
use crate::store::app_calls::{AppCallBarrier, CallState, NewAppCall, UncertainCause};
use crate::store::{Caller, OfflineApprovalPolicy, ProcessIncarnation, Store, StoreConfig};
use crate::workspace::{GitOptions, Limits, PrivatePaths};

const WAIT: Duration = Duration::from_secs(10);
const URI: &str = "ui://demo/counter.html";
const MIME: &str = "text/html;profile=mcp-app";

/// A bridge whose `read_resource` answers one app document and counts reads.
#[derive(Default)]
struct DocBridge {
    reads: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AppBridge for DocBridge {
    async fn tools(&self) -> Result<Vec<AppTool>, AppError> {
        Ok(Vec::new())
    }

    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, AppError> {
        self.reads
            .lock()
            .unwrap()
            .push((server.to_owned(), uri.to_owned()));
        Ok(json!({"contents": [{
            "uri": uri,
            "mimeType": MIME,
            "text": "<!doctype html><title>counter</title>",
        }]}))
    }

    async fn call_tool(
        &self,
        _server: &str,
        _tool: &str,
        _arguments: Value,
    ) -> Result<ToolAnswer, AppError> {
        Err(AppError::Unsupported)
    }
}

fn candidate() -> AppCandidate {
    AppCandidate {
        node_id: "node-1".into(),
        server: Some("demo".to_owned()),
        server_hint: None,
        tool: "show_counter".to_owned(),
        resource_uri: Some(URI.to_owned()),
        arguments: json!({}),
        result: Some(json!({"content": [{"type": "text", "text": "2"}]})),
    }
}

/// The real Claude adapter (`fake-claude sim`) with an app bridge and a
/// candidate channel that the test feeds.
struct WithApps {
    inner: Arc<dyn ProviderAdapter>,
    bridge: Arc<DocBridge>,
    candidates: Mutex<Option<mpsc::Receiver<AppCandidate>>>,
}

#[async_trait]
impl ProviderAdapter for WithApps {
    fn kind(&self) -> ProviderKind {
        self.inner.kind()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn probe(&self) -> fleetdeck_providers::adapter::Result<ProviderProbe> {
        self.inner.probe().await
    }
    async fn start(
        &self,
        spec: SessionSpec,
        sink: EventSink,
    ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
        let session = self.inner.start(spec, sink).await?;
        Ok(Box::new(AppSession {
            inner: session,
            bridge: Arc::clone(&self.bridge),
            candidates: self.candidates.lock().unwrap().take(),
        }))
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

struct AppSession {
    inner: Box<dyn LiveSession>,
    bridge: Arc<DocBridge>,
    candidates: Option<mpsc::Receiver<AppCandidate>>,
}

#[async_trait]
impl LiveSession for AppSession {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn exit_watch(&self) -> Option<ExitWatch> {
        self.inner.exit_watch()
    }
    fn app_bridge(&self) -> Option<Arc<dyn AppBridge>> {
        Some(Arc::clone(&self.bridge) as Arc<dyn AppBridge>)
    }
    fn take_app_candidates(&mut self) -> Option<mpsc::Receiver<AppCandidate>> {
        self.candidates.take()
    }
    async fn prompt(&mut self, input: HostInput) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.prompt(input).await
    }
    async fn steer(&mut self, input: HostInput) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.steer(input).await
    }
    async fn follow_up(&mut self, input: HostInput) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.follow_up(input).await
    }
    async fn interrupt(
        &mut self,
        id: Option<ControlId>,
    ) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.interrupt(id).await
    }
    async fn answer(&mut self, answer: Answer) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.answer(answer).await
    }
    async fn set_mode(&mut self, mode: PermissionMode) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.set_mode(mode).await
    }
    async fn set_model(&mut self, model: ModelId) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.set_model(model).await
    }
    async fn fork(&mut self) -> fleetdeck_providers::adapter::Result<NativeSessionId> {
        self.inner.fork().await
    }
    async fn stop(&mut self, how: StopMode) -> fleetdeck_providers::adapter::Result<()> {
        self.inner.stop(how).await
    }
}

fn host(process: &str, turns: &Arc<SessionTurns>) -> AppHost {
    AppHost {
        bridge: Arc::new(DocBridge::default()),
        turns: Arc::clone(turns),
        process: ProcessIncarnation::from(process.to_owned()),
    }
}

async fn target_rig() -> (Arc<SessionTarget>, Arc<SessionTurns>, Det) {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let turns = SessionTurns::open(
        store.clone(),
        det.ids(),
        GitOptions::isolated(),
        Limits::default(),
        &Fences::default(),
        "s1".into(),
        None,
        None,
        Arc::new(PrivatePaths::default()),
    )
    .await;
    let target = SessionTarget::new(store, Adapters::new(), det.ids()).shared();
    (target, turns, det)
}

fn proc(text: &str) -> ProcessIncarnation {
    ProcessIncarnation::from(text.to_owned())
}

#[tokio::test]
async fn bridge_registry_drops_on_process_end() {
    let (target, turns, _det) = target_rig().await;
    let session: SessionId = "s1".into();
    assert!(target.app_host(&session).is_none());
    target.register_app_host(&session, host("proc-1", &turns));
    assert_eq!(target.app_host(&session).unwrap().process, proc("proc-1"));
    // The exit of another process changes nothing.
    assert!(!target.drop_app_host(&session, &proc("proc-0"), HostEnd::Exited));
    assert!(target.app_host(&session).is_some());
    // A newer process replaced the entry: the exit of the old one keeps it.
    target.register_app_host(&session, host("proc-2", &turns));
    assert!(!target.drop_app_host(&session, &proc("proc-1"), HostEnd::Exited));
    assert_eq!(target.app_host(&session).unwrap().process, proc("proc-2"));
    // The exit of the entry's own process removes it.
    assert!(target.drop_app_host(&session, &proc("proc-2"), HostEnd::Exited));
    assert!(target.app_host(&session).is_none());
    // A second end finds nothing.
    assert!(!target.drop_app_host(&session, &proc("proc-2"), HostEnd::Exited));
}

#[tokio::test]
async fn shutdown_detach_drops_the_app_host() {
    let (target, turns, _det) = target_rig().await;
    let (s1, s2): (SessionId, SessionId) = ("s1".into(), "s2".into());
    target.register_app_host(&s1, host("proc-1", &turns));
    target.register_app_host(&s2, host("proc-9", &turns));
    // The detach removes the entry of its own process and no other.
    assert!(target.drop_app_host(&s1, &proc("proc-1"), HostEnd::Detached));
    assert!(target.app_host(&s1).is_none());
    assert!(target.app_host(&s2).is_some());
    // A detach of a process that is not the entry's keeps the entry.
    assert!(!target.drop_app_host(&s2, &proc("proc-1"), HostEnd::Detached));
    assert!(target.app_host(&s2).is_some());
}

struct DirectRig {
    det: Det,
    manager: SessionManager,
    bridge: Arc<DocBridge>,
    feed: mpsc::Sender<AppCandidate>,
    _root: tempfile::TempDir,
    cwd: String,
}

impl DirectRig {
    async fn new() -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("projects/app");
        std::fs::create_dir_all(&cwd).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        let mut config =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        config.delivery_grace = Duration::from_millis(500);
        let real: Arc<dyn ProviderAdapter> =
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
        let bridge = Arc::new(DocBridge::default());
        let (feed, rx) = mpsc::channel(8);
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(WithApps {
            inner: real,
            bridge: Arc::clone(&bridge),
            candidates: Mutex::new(Some(rx)),
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
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        Self {
            det,
            manager,
            bridge,
            feed,
            cwd: cwd.to_string_lossy().into_owned(),
            _root: root,
        }
    }

    async fn submit(&self, n: u32, command: SessionCommand) -> SessionId {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{n}")),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        let d = self
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap();
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d.receipt.result.unwrap().session_id.unwrap()
    }

    async fn start(&self) -> SessionId {
        let spec = SessionSpec {
            provider_instance: "claude".into(),
            cwd: self.cwd.clone(),
            mode: Some("default".to_owned()),
            model: None,
            effort: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        };
        self.submit(
            1,
            SessionCommand::Start {
                spec,
                prompt: Some(UserInput::new("hello".to_owned())),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
                fleet_git: None,
            },
        )
        .await
    }

    async fn wait_until<F, Fut>(&self, what: &str, ready: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !ready().await {
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn has_tool_app_event(&self, session: &SessionId) -> bool {
        let crate::store::ReadResult::Events { events, .. } =
            self.manager.store().read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required")
        };
        events.iter().any(|e| {
            e.session_id == *session && matches!(e.event, EventBody::Known(Event::ToolApp { .. }))
        })
    }
}

#[tokio::test]
async fn direct_candidates_reach_capture() {
    let rig = DirectRig::new().await;
    let session = rig.start().await;
    // The host is in the registry once the session is live.
    rig.wait_until("app host", || async {
        rig.manager.target.app_host(&session).is_some()
    })
    .await;
    rig.feed.send(candidate()).await.unwrap();
    // The capture service reads the resource through the session's bridge,
    // stores the row and appends `tool.app`.
    rig.wait_until("stored app", || async {
        rig.manager.store().app_count().await.unwrap() == 1
            && rig.has_tool_app_event(&session).await
    })
    .await;
    assert_eq!(
        rig.bridge.reads.lock().unwrap().clone(),
        vec![("demo".to_owned(), URI.to_owned())]
    );
    // The end of the process removes the host (the pump of the session ends).
    rig.submit(
        2,
        SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Kill,
        },
    )
    .await;
    rig.wait_until("dropped app host", || async {
        rig.manager.target.app_host(&session).is_none()
    })
    .await;
    rig.manager.shutdown(StopMode::Kill).await;
}

/// A non-read-only row of `s1` and `proc-1` at `/w` that a provider error
/// made `uncertain`.
async fn uncertain_row(store: &Store, id: &str) {
    store
        .app_call_begin(NewAppCall {
            call_id: id.to_owned(),
            session_id: "s1".into(),
            app_id: "app-1".to_owned(),
            worktree_root: "/w".to_owned(),
            server: "demo".to_owned(),
            tool: "bump".to_owned(),
            read_only: false,
            process: "proc-1".to_owned(),
            station_run: store.run_id().to_owned(),
        })
        .await
        .unwrap();
    assert!(
        store
            .app_call_mark_uncertain(id.to_owned(), UncertainCause::ProviderError)
            .await
            .unwrap()
    );
}

async fn call_state(store: &Store, id: &str) -> CallState {
    store.app_call(id.to_owned()).await.unwrap().unwrap().state
}

async fn barrier(store: &Store) -> Option<AppCallBarrier> {
    store
        .app_call_barrier("/w".to_owned(), "no-turn".to_owned())
        .await
        .unwrap()
}

/// Review blocker 3: the end of a direct session's event pump (here a panic;
/// a store failure ends it the same way) is no exit proof. While the
/// provider process lives, the `uncertain` call stays, the barrier holds and
/// the host stays; the host's observed exit then settles the call.
#[tokio::test]
async fn a_failed_pump_keeps_the_call_uncertain_until_the_observed_exit() {
    let (target, turns, _det) = target_rig().await;
    let (session, store) = (SessionId::from("s1"), target.app_env().store);
    uncertain_row(&store, "c1").await;
    target.register_app_host(&session, host("proc-1", &turns));
    let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);
    let pump = tokio::spawn(async {
        panic!("the event pump failed");
    });
    let watcher = {
        let (target, session) = (Arc::clone(&target), session.clone());
        tokio::spawn(async move {
            let end = direct_end(Some(exit_rx), pump).await;
            target.drop_app_host(&session, &proc("proc-1"), end);
            end
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!watcher.is_finished(), "the process still runs");
    assert_eq!(call_state(&store, "c1").await, CallState::Uncertain);
    assert_eq!(barrier(&store).await, Some(AppCallBarrier::Running));
    assert!(target.app_host(&session).is_some());
    assert!(!target.app_env().exits.has(&session, "proc-1"));
    // The real exit: the host reaped the child.
    exit_tx.send_replace(Some(ExitReport {
        termination: Termination::Code(0),
        cause: EndCause::Exited,
        stderr_tail: String::new(),
    }));
    assert_eq!(watcher.await.unwrap(), HostEnd::Exited);
    assert!(target.app_host(&session).is_none());
    assert!(target.app_env().exits.has(&session, "proc-1"));
    let deadline = tokio::time::Instant::now() + WAIT;
    while call_state(&store, "c1").await != CallState::Ended {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the call did not end"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Without an exit watch, or when the watch closes without a report, the end
/// is a lost observation: the host leaves, no exit is recorded, and the
/// `uncertain` call keeps the barrier.
#[tokio::test]
async fn a_lost_observation_settles_no_call() {
    let (target, turns, _det) = target_rig().await;
    let (session, store) = (SessionId::from("s1"), target.app_env().store);
    uncertain_row(&store, "c1").await;
    assert_eq!(
        direct_end(None, tokio::spawn(async {})).await,
        HostEnd::Lost
    );
    let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);
    drop(exit_tx);
    let end = direct_end(Some(exit_rx), tokio::spawn(async {})).await;
    assert_eq!(end, HostEnd::Lost);
    target.register_app_host(&session, host("proc-1", &turns));
    assert!(target.drop_app_host(&session, &proc("proc-1"), end));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(call_state(&store, "c1").await, CallState::Uncertain);
    assert_eq!(barrier(&store).await, Some(AppCallBarrier::Running));
    assert!(!target.app_env().exits.has(&session, "proc-1"));
}
