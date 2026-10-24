//! The station rig of the MCP Apps shim tests (B15 T5b): real shims, the fake
//! Codex app-server under the scenario `mcp_app` (the demo MCP server with its
//! app, `fleetdeck-testkit` `sim_codex_mcp.rs`), a Git workspace and a station
//! that starts on the shared database file. It copies the rig of
//! `shim_manager.rs`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId, TurnId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_station::workspace::PrivatePaths;
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};

#[path = "../shim_reaper/mod.rs"]
pub mod shim_reaper;

pub const WAIT: Duration = Duration::from_secs(30);
/// The slow turn: long enough that the station stops while it runs, also on a
/// loaded machine.
pub const SLOW_MS: u64 = 4000;

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

pub struct Rig {
    pub tmp: tempfile::TempDir,
    pub det: Det,
    pub next: u32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

/// One station start: a store on the shared database file and a manager with
/// the shim host.
pub struct Station {
    pub store: Store,
    pub manager: SessionManager,
}

impl Rig {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/app")).unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        let ws = tmp.path().join("projects/app");
        git(&ws, &["init", "-q", "-b", "main"]);
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        git(&ws, &["add", "."]);
        git(&ws, &["commit", "-q", "-m", "init"]);
        Self {
            tmp,
            det: Det::new(),
            next: 0,
        }
    }

    pub fn workspace(&self) -> String {
        std::fs::canonicalize(self.tmp.path().join("projects/app"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub fn log(&self) -> PathBuf {
        self.tmp.path().join("codex-stdin.log")
    }

    pub fn adapters(&self) -> Adapters {
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "mcp_app").unwrap();
        env.set(STDIN_LOG_ENV, self.log()).unwrap();
        let mut config =
            CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        Adapters::from([(
            "codex".into(),
            Arc::new(CodexAdapter::new(config, Arc::new(DirectChildHost::new())))
                as Arc<dyn fleetdeck_providers::ProviderAdapter>,
        )])
    }

    pub fn shim_settings(&self, store: &Store) -> ShimSettings {
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            self.tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = self.tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        ShimSettings {
            host: Arc::new(ShimHost::new(config)),
            in_flight_wait: Duration::from_secs(1),
            rollback_timeout: Duration::from_millis(500),
        }
    }

    pub async fn open_store(&self) -> Store {
        Store::open(
            &self.tmp.path().join("station.db"),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap()
    }

    pub async fn station(&self) -> Station {
        let store = self.open_store().await;
        let policy = Policy::new(
            vec![self.tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("codex".into(), ProviderKind::Codex)]),
        );
        let shims = self.shim_settings(&store);
        let manager = SessionManager::start_with_shims(
            store.clone(),
            policy,
            self.adapters(),
            self.det.ids(),
            shims,
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        Station { store, manager }
    }

    pub async fn send(&mut self, station: &Station, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        let decided = station
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap();
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt
        );
        decided
    }

    pub async fn start_session(&mut self, station: &Station, prompt: &str) -> SessionId {
        let spec = SessionSpec {
            provider_instance: "codex".into(),
            cwd: self.workspace(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        };
        let decided = self
            .send(
                station,
                SessionCommand::Start {
                    spec,
                    prompt: Some(UserInput::new(prompt.to_owned())),
                    workspace: None,
                    setup_cmd: None,
                    batch: None,
                    issue: None,
                    origin_text: false,
                    fleet_git: None,
                },
            )
            .await;
        decided
            .receipt
            .result
            .and_then(|r| r.session_id)
            .expect("a session id")
    }

    pub async fn prompt(&mut self, station: &Station, session: &SessionId, text: &str) {
        let command = SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput::new(text.to_owned()),
        };
        self.send(station, command).await;
    }
}

pub async fn events(store: &Store, session: &SessionId) -> Vec<Event> {
    let ReadResult::Events { events, .. } = store.read_after(0, 10_000).await.unwrap() else {
        panic!("snapshot required")
    };
    events
        .into_iter()
        .filter(|e| &e.session_id == session)
        .filter_map(|e| match e.event {
            EventBody::Known(event) => Some(event),
            EventBody::Unknown { .. } => None,
        })
        .collect()
}

pub async fn wait_for(
    store: &Store,
    session: &SessionId,
    what: &str,
    pick: impl Fn(&[Event]) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        let all = events(store, session).await;
        if pick(&all) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what}: {all:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub fn started_turns(events: &[Event]) -> Vec<TurnId> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .collect()
}

pub fn finalized_turns(events: &[Event]) -> Vec<TurnId> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TurnFinalized { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .collect()
}

/// The app ids of the `tool.app` events of `session`.
pub fn tool_apps(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::ToolApp { app_id, .. } => Some(app_id.clone()),
            _ => None,
        })
        .collect()
}

/// Waits for `n` `tool.app` events of `session` and returns their app ids.
pub async fn wait_tool_apps(store: &Store, session: &SessionId, n: usize) -> Vec<String> {
    wait_for(store, session, "the tool.app events", |e| {
        tool_apps(e).len() >= n
    })
    .await;
    tool_apps(&events(store, session).await)
}

/// Polls `f` until it gives a value (at most [`WAIT`]).
pub async fn until<T, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    mut f: impl FnMut() -> Fut,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
