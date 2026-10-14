//! The rig of the resume tests on the shim host (DL-182): real shims, the
//! fake Claude and Codex agents, a Git workspace, a station on a database
//! file, and a boot id that a test can change (a reboot, a memory restore,
//! an unreadable id).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, SessionState, TurnStatus};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::identity::boot_id;
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, OfflineApprovalPolicy, ReadResult, ShimRow, ShimState, Store, StoreConfig,
};
use fleetdeck_station::workspace::PrivatePaths;
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, LAUNCH_LOG_ENV, SCENARIO_ENV, STDIN_LOG_ENV};
use serde_json::Value;

#[path = "../shim_reaper/mod.rs"]
pub mod shim_reaper;

pub const WAIT: Duration = Duration::from_secs(20);

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

/// What the station reads as the boot id.
#[derive(Debug, Clone)]
pub enum Boot {
    /// The real boot id of this machine.
    Real,
    /// Another boot id (a reboot, or a memory restore that brings back an
    /// old one).
    Id(String),
    /// The boot id cannot be read.
    Unreadable,
}

pub struct Rig {
    pub tmp: tempfile::TempDir,
    pub det: Det,
    pub next: u32,
    pub boot: Arc<Mutex<Boot>>,
    /// The scenario of the fake Claude; `sim` unless a test changes it.
    pub claude_scenario: String,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

/// One station start: a store on the shared database file and a manager
/// with the shim host.
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
            boot: Arc::new(Mutex::new(Boot::Real)),
            claude_scenario: "sim".to_owned(),
        }
    }

    pub fn workspace(&self) -> String {
        std::fs::canonicalize(self.tmp.path().join("projects/app"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub fn shims_dir(&self) -> PathBuf {
        self.tmp.path().join("station").join("shims")
    }

    pub fn stdin_log(&self, instance: &str) -> PathBuf {
        self.tmp.path().join(format!("{instance}-stdin.log"))
    }

    pub fn launch_log(&self) -> PathBuf {
        self.tmp.path().join("claude-launch.log")
    }

    /// Installs the boot id that the station reads from now on.
    pub fn set_boot(&self, boot: Boot) {
        *self.boot.lock().unwrap() = boot;
    }

    pub fn adapters(&self) -> Adapters {
        let stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, &self.claude_scenario).unwrap();
        env.set(STDIN_LOG_ENV, self.stdin_log("claude")).unwrap();
        env.set(LAUNCH_LOG_ENV, self.launch_log()).unwrap();
        let mut claude =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        claude.stop = stop;
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, self.stdin_log("codex")).unwrap();
        let mut codex =
            CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
        codex.stop = stop;
        Adapters::from([
            (
                "claude".into(),
                Arc::new(ClaudeAdapter::new(claude, Arc::new(DirectChildHost::new())))
                    as Arc<dyn fleetdeck_providers::ProviderAdapter>,
            ),
            (
                "codex".into(),
                Arc::new(CodexAdapter::new(codex, Arc::new(DirectChildHost::new())))
                    as Arc<dyn fleetdeck_providers::ProviderAdapter>,
            ),
        ])
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
        let boot = Arc::clone(&self.boot);
        config.boot_id_reader = Arc::new(move || match &*boot.lock().unwrap() {
            Boot::Real => boot_id().map_err(|e| std::io::Error::other(e.to_string())),
            Boot::Id(id) => Ok(id.clone()),
            Boot::Unreadable => Err(std::io::Error::other("the boot id is unreadable")),
        });
        ShimSettings {
            host: Arc::new(ShimHost::new(config)),
            in_flight_wait: Duration::from_secs(1),
            rollback_timeout: Duration::from_millis(500),
        }
    }

    pub fn policy(&self) -> Policy {
        Policy::new(
            vec![self.tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([
                ("claude".into(), ProviderKind::Claude),
                ("codex".into(), ProviderKind::Codex),
            ]),
        )
    }

    /// One station start on the database file of the rig.
    pub async fn station(&self) -> Station {
        self.station_with(self.policy()).await
    }

    pub async fn station_with(&self, policy: Policy) -> Station {
        let store = Store::open(
            &self.tmp.path().join("station.db"),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
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

    pub async fn submit(&mut self, station: &Station, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        station
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    pub async fn send(&mut self, station: &Station, command: SessionCommand) -> Decided {
        let decided = self.submit(station, command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt
        );
        decided
    }

    /// The code and the message of a rejected command.
    pub async fn rejected(
        &mut self,
        station: &Station,
        command: SessionCommand,
    ) -> (fleetdeck_proto::RejectCode, String) {
        let decided = self.submit(station, command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Rejected,
            "{:?}",
            decided.receipt
        );
        let reject = decided.receipt.reject.unwrap();
        (reject.code, reject.message)
    }

    pub fn spec(&self, instance: &str) -> SessionSpec {
        SessionSpec {
            provider_instance: instance.into(),
            cwd: self.workspace(),
            mode: (instance == "claude").then(|| "default".to_owned()),
            model: None,
            title: None,
            coordinator: None,
        }
    }

    pub async fn start(&mut self, station: &Station, instance: &str, prompt: &str) -> SessionId {
        let spec = self.spec(instance);
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
                },
            )
            .await;
        decided
            .receipt
            .result
            .and_then(|r| r.session_id)
            .expect("a session id")
    }

    pub async fn stop(&mut self, station: &Station, session: &SessionId) {
        self.send(
            station,
            SessionCommand::Stop {
                session_id: session.clone(),
                how: StopMode::Graceful,
            },
        )
        .await;
    }

    /// Starts a session, runs one turn and stops it cleanly. Returns it
    /// once its shim row is `exited`.
    pub async fn ended(&mut self, station: &Station, instance: &str) -> SessionId {
        let session = self.start(station, instance, "hello").await;
        wait_turns(&station.store, &session, 1).await;
        self.stop(station, &session).await;
        wait_state(&station.store, &session, SessionState::Exited).await;
        wait_rows(&station.store, &session, "the shim row ends", |rows| {
            rows.iter().all(|r| r.state == ShimState::Exited)
        })
        .await;
        session
    }

    /// The number of stdin lines of `instance` that hold `needle`; for
    /// Codex the number of requests of the method `needle`.
    pub fn log_count(&self, instance: &str, needle: &str) -> usize {
        std::fs::read_to_string(self.stdin_log(instance))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(needle))
            .count()
    }

    pub fn method_count(&self, method: &str) -> usize {
        std::fs::read_to_string(self.stdin_log("codex"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|f| f["method"] == method)
            .count()
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

pub fn turns_done(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::TurnCompleted {
                    status: TurnStatus::Done,
                    ..
                }
            )
        })
        .count()
}

pub async fn wait_turns(store: &Store, session: &SessionId, n: usize) {
    wait_for(store, session, &format!("{n} done turns"), |e| {
        turns_done(e) >= n
    })
    .await;
}

pub async fn state(store: &Store, session: &SessionId) -> SessionState {
    store.summary(session.clone()).await.unwrap().unwrap().state
}

pub async fn wait_state(store: &Store, session: &SessionId, want: SessionState) {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = state(store, session).await;
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "{now:?}, not {want:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_rows(
    store: &Store,
    session: &SessionId,
    what: &str,
    pick: impl Fn(&[ShimRow]) -> bool,
) -> Vec<ShimRow> {
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = store.shim_rows(session.clone()).await.unwrap();
        if pick(&rows) {
            return rows;
        }
        assert!(Instant::now() < deadline, "{what}: {rows:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The `session.started` events of a session: `(resumed, native id)`.
pub async fn starts(store: &Store, session: &SessionId) -> Vec<(Option<bool>, String)> {
    events(store, session)
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::SessionStarted {
                resumed,
                provider_refs,
                ..
            } => {
                let native = provider_refs
                    .get("session_id")
                    .or_else(|| provider_refs.get("thread_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some((resumed, native))
            }
            _ => None,
        })
        .collect()
}

pub fn text(t: &str) -> UserInput {
    UserInput::new(t.to_owned())
}

/// Sends a prompt as soon as the resumed session takes input (a Claude CLI
/// reports `session.started` only after its first input).
pub async fn prompt_when_live(rig: &mut Rig, station: &Station, session: &SessionId, input: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let decided = rig
            .submit(
                station,
                SessionCommand::Prompt {
                    session_id: session.clone(),
                    input: text(input),
                },
            )
            .await;
        if decided.receipt.state == ReceiptState::Accepted {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", decided.receipt);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
