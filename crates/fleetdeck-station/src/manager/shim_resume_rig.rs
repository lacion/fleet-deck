//! The harness of the resume tests on the shim host (DL-182): a station
//! with a shim host whose executable does not exist, the fake agents, rows
//! written the way a shim station leaves them, and a boot id that a test
//! sets.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::identity::{BootIdReader, real_boot_id_reader};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use super::*;
use crate::store::{
    Caller, Decided, EffectState, NewShim, OfflineApprovalPolicy, ReadResult, RunningFacts,
    ShimState, StoreConfig,
};

pub const WAIT: Duration = Duration::from_secs(15);
pub const AFTER: u32 = 30;

pub fn stop_policy() -> StopPolicy {
    StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    }
}

pub fn claude_adapter() -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.stop = stop_policy();
    config.delivery_grace = Duration::from_millis(500);
    Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
}

pub fn codex_adapter(instance: &str) -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        CodexAdapterConfig::new(instance.into(), FakeAgent::Codex.locate().unwrap(), env);
    config.background_poll = Some(Duration::from_millis(50));
    config.stop = stop_policy();
    config.delivery_grace = Duration::from_millis(500);
    Arc::new(CodexAdapter::new(config, Arc::new(DirectChildHost::new())))
}

/// The boot id as a test sets it.
#[derive(Clone)]
pub struct Boot(pub Arc<Mutex<Option<String>>>);

impl Boot {
    pub fn set(&self, id: Option<&str>) {
        *self.0.lock().unwrap() = id.map(str::to_owned);
    }

    pub fn reader(&self) -> BootIdReader {
        let inner = Arc::clone(&self.0);
        Arc::new(move || {
            inner
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| std::io::Error::other("the boot id is unreadable"))
        })
    }
}

pub struct Rig {
    pub det: Det,
    pub tmp: tempfile::TempDir,
    pub manager: Arc<SessionManager>,
    pub boot: Boot,
    pub host: Arc<ShimHost>,
    pub next: u32,
}

pub fn policy(root: &std::path::Path) -> Policy {
    Policy::new(
        vec![root.join("projects")],
        8,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([
            ("claude".into(), ProviderKind::Claude),
            ("codex".into(), ProviderKind::Codex),
            ("codex-b".into(), ProviderKind::Codex),
        ]),
    )
}

pub fn adapters() -> Adapters {
    Adapters::from([
        ("claude".into(), claude_adapter()),
        ("codex".into(), codex_adapter("codex")),
        ("codex-b".into(), codex_adapter("codex-b")),
    ])
}

impl Rig {
    /// A direct-host station (the rows of a shim station are written by the
    /// test). `platform`: the shim-host platform of the policy.
    pub async fn direct(platform: Option<bool>) -> Self {
        let det = Det::new();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/app")).unwrap();
        std::fs::create_dir_all(tmp.path().join("station")).unwrap();
        let store = Store::open(
            &tmp.path().join("station.db"),
            det.clock(),
            det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let mut policy = policy(tmp.path());
        if let Some(supported) = platform {
            policy = policy.with_shim_host(supported);
        }
        let manager = SessionManager::start(
            store.clone(),
            policy,
            adapters(),
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        let boot = Boot(Arc::new(Mutex::new(Some("boot-new".to_owned()))));
        let host = Arc::new(Self::host_of(&tmp, &store, &boot));
        Self {
            det,
            tmp,
            manager: Arc::new(manager),
            boot,
            host,
            next: 0,
        }
    }

    /// A shim host over the rig's directory; its executable does not exist,
    /// so a launch that reaches it fails (the tests that use it stop
    /// before).
    pub fn host_of(tmp: &tempfile::TempDir, store: &Store, boot: &Boot) -> ShimHost {
        let mut config = ShimHostConfig::new(
            PathBuf::from("/nonexistent/fleetdeck"),
            tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = tmp.path().to_owned();
        config.boot_id_reader = boot.reader();
        ShimHost::new(config)
    }

    /// Restarts the station as a shim station on the same database: the
    /// same adapters, the host of the rig.
    pub async fn as_shim_station(&mut self) {
        self.manager.shutdown(StopMode::Kill).await;
        let store = Store::open(
            &self.tmp.path().join("station.db"),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let host = Arc::new(Self::host_of(&self.tmp, &store, &self.boot));
        self.host = Arc::clone(&host);
        let settings = crate::manager::target::ShimSettings::new(host);
        self.manager = Arc::new(
            SessionManager::start_with_shims(
                store,
                policy(self.tmp.path()),
                adapters(),
                self.det.ids(),
                settings,
                Arc::new(crate::workspace::PrivatePaths::default()),
            )
            .await
            .unwrap(),
        );
    }

    pub fn store(&self) -> Store {
        self.manager.store().clone()
    }

    pub fn workspace(&self) -> String {
        self.tmp
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned()
    }

    pub fn envelope(&mut self, origin: CommandOrigin, command: SessionCommand) -> CommandEnvelope {
        self.next += 1;
        CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command,
        }
    }

    pub async fn submit_as(&mut self, origin: CommandOrigin, command: SessionCommand) -> Decided {
        let envelope = self.envelope(origin, command);
        self.manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    pub async fn send(&mut self, command: SessionCommand) -> Decided {
        self.submit_as(CommandOrigin::Local, command).await
    }

    pub async fn accepted(&mut self, command: SessionCommand) -> Decided {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    /// The code and message of a rejection.
    pub async fn refused(&mut self, command: SessionCommand) -> (RejectCode, String) {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let reject = d.receipt.reject.unwrap();
        (reject.code, reject.message)
    }

    pub async fn events(&self) -> Vec<Event> {
        self.events_of(None).await
    }

    /// The events of `session`, or of every session.
    pub async fn events_of(&self, session: Option<&SessionId>) -> Vec<Event> {
        let ReadResult::Events { events, .. } = self.store().read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required")
        };
        events
            .into_iter()
            .filter(|e| session.is_none_or(|s| &e.session_id == s))
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                EventBody::Unknown { .. } => None,
            })
            .collect()
    }

    pub async fn wait_until<F: Fn(Vec<Event>) -> bool>(&self, what: &str, f: F) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if f(self.events().await) {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn state(&self, session: &SessionId) -> SessionState {
        self.store()
            .summary(session.clone())
            .await
            .unwrap()
            .unwrap()
            .state
    }

    pub async fn wait_state(&self, session: &SessionId, want: SessionState) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.state(session).await != want {
            assert!(tokio::time::Instant::now() < deadline, "not {want:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn settled(&self, session: &SessionId) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let effects = self.store().session_effects(session.clone()).await.unwrap();
            if effects
                .iter()
                .all(|e| !matches!(e.state, EffectState::Queued | EffectState::Claimed))
            {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{effects:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn spec(&self, instance: &str) -> SessionSpec {
        SessionSpec {
            effort: None,
            provider_instance: instance.into(),
            cwd: self.workspace(),
            mode: (instance == "claude").then(|| "default".to_owned()),
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        }
    }

    pub async fn start(&mut self, instance: &str) -> SessionId {
        let spec = self.spec(instance);
        let d = self
            .accepted(SessionCommand::Start {
                spec,
                prompt: Some(UserInput::new("hello".to_owned())),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
                fleet_git: None,
            })
            .await;
        let session = d.receipt.result.unwrap().session_id.unwrap();
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let events = self.events_of(Some(&session)).await;
            // The run is over and the session idles (a hibernation needs it).
            if events
                .iter()
                .any(|e| matches!(e, Event::RunCompleted { .. }))
            {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "no run completed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        session
    }

    /// A session that ran one turn and was stopped.
    pub async fn ended(&mut self, instance: &str) -> SessionId {
        let session = self.start(instance).await;
        self.accepted(SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Graceful,
        })
        .await;
        self.wait_state(&session, SessionState::Exited).await;
        self.settled(&session).await;
        session
    }

    /// An ended session whose state is `lost`, as a station start leaves it.
    pub async fn lost(&mut self, instance: &str) -> SessionId {
        let session = self.ended(instance).await;
        self.store()
            .append(
                session.clone(),
                vec![
                    Event::SessionStateChanged {
                        state: SessionState::Lost,
                        detail: Some("test".to_owned()),
                    }
                    .into(),
                ],
            )
            .await
            .unwrap();
        session
    }

    pub async fn native(&self, session: &SessionId) -> String {
        self.events()
            .await
            .into_iter()
            .find_map(|e| match e {
                Event::SessionStarted { provider_refs, .. } => provider_refs
                    .get("session_id")
                    .or_else(|| provider_refs.get("thread_id"))
                    .and_then(|v| v.as_str().map(str::to_owned)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no native id for {session}"))
    }

    /// One ended shim row of `session`.
    pub async fn row(
        &self,
        session: &SessionId,
        id: &str,
        state: ShimState,
        reason: &str,
        inspection: u32,
        identity: Option<ProviderIdentity>,
    ) {
        self.row_for(session, id, state, reason, inspection, identity, None, None)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn row_for(
        &self,
        session: &SessionId,
        id: &str,
        state: ShimState,
        reason: &str,
        inspection: u32,
        identity: Option<ProviderIdentity>,
        launch_boot: Option<&str>,
        native_target: Option<&str>,
    ) {
        let store = self.store();
        store
            .shim_starting(NewShim {
                shim_id: id.to_owned(),
                session_id: session.clone(),
                socket_path: PathBuf::from(format!("/nonexistent/{id}.s")),
                launch_boot_id: launch_boot
                    .map(str::to_owned)
                    .or_else(|| identity.as_ref().map(|i| i.boot_id.clone())),
                native_target: native_target.map(str::to_owned),
            })
            .await
            .unwrap();
        if let Some(identity) = identity {
            store
                .shim_running_with(id.to_owned(), identity, RunningFacts::default())
                .await
                .unwrap();
        }
        store
            .shim_end(id.to_owned(), state, reason.to_owned(), inspection)
            .await
            .unwrap();
    }

    pub async fn rows(&self, session: &SessionId) -> Vec<crate::store::ShimRow> {
        self.store().shim_rows(session.clone()).await.unwrap()
    }
}

/// An identity on `boot` of a process that does not exist.
pub fn dead_identity(boot: &str) -> ProviderIdentity {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    ProviderIdentity {
        pid,
        start_time: 1,
        boot_id: boot.to_owned(),
    }
}

pub fn real_boot() -> String {
    real_boot_id_reader()().unwrap()
}

pub fn resume(session: &SessionId) -> SessionCommand {
    SessionCommand::Resume {
        session_id: session.clone(),
    }
}

pub fn import(rig: &Rig, instance: &str, native: &str) -> SessionCommand {
    SessionCommand::Import {
        spec: rig.spec(instance),
        native_session_id: native.to_owned(),
        prompt: None,
        transcript: None,
    }
}

pub fn reconcile(session: &SessionId, shim: &str) -> SessionCommand {
    SessionCommand::ReconcileExecution {
        session_id: session.clone(),
        shim_id: shim.to_owned(),
    }
}
