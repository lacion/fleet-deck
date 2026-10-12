//! The session manager with the shim host (ADR-005; DL-113): a Codex
//! session launched under a shim gets its turn checkpoints from the events
//! that the durable driver commits, survives a station stop, and is
//! restored by the next station start on the same database without a
//! second handshake. Real shims, the fake Codex app-server, a Git
//! workspace.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{CheckpointSide, ProviderKind, SessionState};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};
use serde_json::Value;

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(20);

fn git(dir: &Path, args: &[&str]) {
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

struct Rig {
    tmp: tempfile::TempDir,
    det: Det,
    next: u32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

/// One station start: a store on the shared database file and a manager
/// with the shim host.
struct Station {
    store: Store,
    manager: SessionManager,
}

impl Rig {
    fn new() -> Self {
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

    fn workspace(&self) -> String {
        std::fs::canonicalize(self.tmp.path().join("projects/app"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn log(&self) -> PathBuf {
        self.tmp.path().join("codex-stdin.log")
    }

    fn adapters(&self) -> Adapters {
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
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

    async fn station(&self) -> Station {
        let store = Store::open(
            &self.tmp.path().join("station.db"),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let policy = Policy::new(
            vec![self.tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("codex".into(), ProviderKind::Codex)]),
        );
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            self.tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = self.tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        let shims = ShimSettings {
            host: Arc::new(ShimHost::new(config)),
            in_flight_wait: Duration::from_secs(1),
        };
        let manager = SessionManager::start_with_shims(
            store.clone(),
            policy,
            self.adapters(),
            self.det.ids(),
            shims,
        )
        .await
        .unwrap();
        Station { store, manager }
    }

    async fn send(&mut self, station: &Station, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
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

    fn method_count(&self, method: &str) -> usize {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|f| f["method"] == method)
            .count()
    }
}

async fn events(store: &Store, session: &SessionId) -> Vec<Event> {
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

/// Waits until `n` turns of `session` have `turn.finalized` with both
/// checkpoints available; returns those events.
async fn finalized(store: &Store, session: &SessionId, n: usize) -> Vec<Event> {
    let deadline = Instant::now() + WAIT;
    loop {
        let found: Vec<Event> = events(store, session)
            .await
            .into_iter()
            .filter(|e| matches!(e, Event::TurnFinalized { .. }))
            .collect();
        if found.len() >= n {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "no {n} turn.finalized: {:?}",
            events(store, session).await
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn both_available(event: &Event) -> bool {
    let Event::TurnFinalized {
        checkpoint: Some(c),
        ..
    } = event
    else {
        return false;
    };
    matches!(c.start, CheckpointSide::Available { .. })
        && matches!(c.end, CheckpointSide::Available { .. })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shim_session_keeps_its_turn_checkpoints_and_survives_a_station_restart() {
    let mut rig = Rig::new();
    let first = rig.station().await;
    let decided = rig
        .send(
            &first,
            SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "codex".into(),
                    cwd: rig.workspace(),
                    mode: None,
                    model: None,
                    title: None,
                },
                prompt: Some(UserInput {
                    text: "hello".to_owned(),
                }),
            },
        )
        .await;
    let session = decided
        .receipt
        .result
        .and_then(|r| r.session_id)
        .expect("a session id");
    // The durable driver committed the turn; the observer finalized it.
    let done = finalized(&first.store, &session, 1).await;
    assert!(both_available(&done[0]), "{done:?}");
    let shims = first.store.shims_of_session(session.clone()).await.unwrap();
    assert_eq!(shims.len(), 1, "the session runs under a shim");

    // A station stop leaves the session under its shim running.
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);

    let second = rig.station().await;
    let summary = second
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, SessionState::Running, "restored, not lost");
    rig.send(
        &second,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput {
                text: "again".to_owned(),
            },
        },
    )
    .await;
    let done = finalized(&second.store, &session, 2).await;
    assert!(both_available(&done[1]), "{done:?}");
    assert_eq!(rig.method_count("initialize"), 1, "no second handshake");
    assert_eq!(rig.method_count("thread/start"), 1);
    assert_eq!(rig.method_count("turn/start"), 2);
    assert!(
        !events(&second.store, &session)
            .await
            .iter()
            .any(|e| matches!(
                e,
                Event::SessionStateChanged {
                    state: SessionState::Lost,
                    ..
                }
            )),
        "the session was never lost"
    );
    second.manager.shutdown(StopMode::Graceful).await;
}
