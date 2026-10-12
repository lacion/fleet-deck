//! Revert of a Codex session under a shim (PLAN §6.6, L13; ADR-005;
//! DL-116): the session manager with the shim host, the fake Codex
//! app-server and a Git workspace. The same revert executor as with the
//! direct host runs, with the durable driver's `thread/rollback` as the
//! conversation rollback: success restores the files after the confirmed
//! rollback, a refusal changes nothing, and a station crash between the
//! rollback and the file restore never repeats the rollback and blocks the
//! session's input.

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
use fleetdeck_proto::event::{ProviderKind, RevertState, SessionState};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId, TurnId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, OfflineApprovalPolicy, ReadResult, RevertStep, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};
use serde_json::Value;

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(20);

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

struct Rig {
    tmp: tempfile::TempDir,
    det: Det,
    next: u32,
    rollback_timeout: Duration,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

struct Station {
    store: Store,
    manager: SessionManager,
}

impl Rig {
    fn new(rollback_timeout: Duration) -> Self {
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
            rollback_timeout,
        }
    }

    fn workspace(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path().join("projects/app")).unwrap()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.workspace().join("a.txt")).unwrap()
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
            rollback_timeout: self.rollback_timeout,
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

    async fn submit(&mut self, station: &Station, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
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

    async fn send(&mut self, station: &Station, command: SessionCommand) -> Decided {
        let decided = self.submit(station, command).await;
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

    /// A session with two finalized turns: the first runs `first`, the
    /// second edits `a.txt` while it waits and is interrupted. Returns the
    /// session and the second turn.
    async fn two_turns(&mut self, station: &Station, first: &str) -> (SessionId, TurnId) {
        let decided = self
            .send(
                station,
                SessionCommand::Start {
                    spec: SessionSpec {
                        provider_instance: "codex".into(),
                        cwd: self.workspace().to_string_lossy().into_owned(),
                        mode: None,
                        model: None,
                        title: None,
                    },
                    prompt: Some(text(first)),
                },
            )
            .await;
        let session = decided.receipt.result.and_then(|r| r.session_id).unwrap();
        finalized(&station.store, &session, 1).await;
        self.send(
            station,
            SessionCommand::Prompt {
                session_id: session.clone(),
                input: text("wait edit"),
            },
        )
        .await;
        wait_for(&station.store, &session, "the waiting delta", |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::MessageDelta { text, .. } if text == "waiting"))
        })
        .await;
        std::fs::write(self.workspace().join("a.txt"), "one\nedited\n").unwrap();
        self.send(
            station,
            SessionCommand::Interrupt {
                session_id: session.clone(),
            },
        )
        .await;
        let done = finalized(&station.store, &session, 2).await;
        let Event::TurnFinalized { turn_id, .. } = &done[1] else {
            unreachable!()
        };
        (session, turn_id.clone())
    }

    /// Submits the revert of `turn` until the session is quiet enough to
    /// admit it (the last effects confirm shortly after the turn ends).
    async fn revert(&mut self, station: &Station, session: &SessionId, turn: &TurnId) {
        let deadline = Instant::now() + WAIT;
        loop {
            let decided = self
                .submit(
                    station,
                    SessionCommand::Revert {
                        session_id: session.clone(),
                        turn_id: turn.clone(),
                    },
                )
                .await;
            if decided.receipt.state == ReceiptState::Accepted {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the revert was never admitted: {:?}",
                decided.receipt
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn text(t: &str) -> UserInput {
    UserInput { text: t.to_owned() }
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

async fn wait_for(store: &Store, session: &SessionId, what: &str, pick: impl Fn(&[Event]) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = events(store, session).await;
        if pick(&found) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what}: {found:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn finalized(store: &Store, session: &SessionId, n: usize) -> Vec<Event> {
    wait_for(store, session, "turn.finalized", |events| {
        events
            .iter()
            .filter(|e| matches!(e, Event::TurnFinalized { .. }))
            .count()
            >= n
    })
    .await;
    events(store, session)
        .await
        .into_iter()
        .filter(|e| matches!(e, Event::TurnFinalized { .. }))
        .collect()
}

fn revert_states(events: &[Event]) -> Vec<(RevertState, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::RevertUpdated { state, reason, .. } => Some((*state, reason.clone())),
            _ => None,
        })
        .collect()
}

async fn wait_revert(store: &Store, session: &SessionId, state: RevertState) {
    wait_for(store, session, "the revert state", |events| {
        revert_states(events).iter().any(|(s, _)| *s == state)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revert_under_a_shim_rolls_back_the_conversation_then_restores_the_files() {
    let mut rig = Rig::new(Duration::from_secs(5));
    let station = rig.station().await;
    let (session, turn) = rig.two_turns(&station, "hello").await;
    assert_eq!(rig.file(), "one\nedited\n");
    rig.revert(&station, &session, &turn).await;
    wait_revert(&station.store, &session, RevertState::Succeeded).await;
    let states: Vec<RevertState> = revert_states(&events(&station.store, &session).await)
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert_eq!(
        states,
        vec![
            RevertState::Admitted,
            RevertState::ConversationRolledBack,
            RevertState::Succeeded
        ]
    );
    assert_eq!(rig.file(), "one\n", "the files match the turn's start");
    assert_eq!(rig.method_count("thread/rollback"), 1);
    // The session takes input again, under the same shim.
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: text("after"),
        },
    )
    .await;
    finalized(&station.store, &session, 3).await;
    assert_eq!(rig.method_count("initialize"), 1);
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_rollback_under_a_shim_changes_no_file() {
    let mut rig = Rig::new(Duration::from_secs(5));
    let station = rig.station().await;
    let (session, turn) = rig.two_turns(&station, "set-rollback refuse").await;
    rig.revert(&station, &session, &turn).await;
    wait_revert(&station.store, &session, RevertState::Failed).await;
    let states = revert_states(&events(&station.store, &session).await);
    let (_, reason) = states.last().unwrap();
    assert!(
        reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("rollback_refused"),
        "{states:?}"
    );
    assert_eq!(rig.file(), "one\nedited\n", "no file changed");
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: text("after"),
        },
    )
    .await;
    finalized(&station.store, &session, 3).await;
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_rollback_under_a_shim_requires_reconciliation_and_blocks_input() {
    let mut rig = Rig::new(Duration::from_millis(500));
    let station = rig.station().await;
    let (session, turn) = rig.two_turns(&station, "set-rollback hang").await;
    rig.revert(&station, &session, &turn).await;
    wait_revert(
        &station.store,
        &session,
        RevertState::ReconciliationRequired,
    )
    .await;
    assert_eq!(rig.file(), "one\nedited\n", "no file restored");
    let decided = rig
        .submit(
            &station,
            SessionCommand::Prompt {
                session_id: session.clone(),
                input: text("blocked"),
            },
        )
        .await;
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_station_crash_between_the_rollback_and_the_restore_never_repeats_the_rollback() {
    // A long rollback wait: the station stops while the rollback has no
    // answer (step `rollback_sent`).
    let mut rig = Rig::new(Duration::from_secs(60));
    let first = rig.station().await;
    let (session, turn) = rig.two_turns(&first, "set-rollback hang").await;
    rig.revert(&first, &session, &turn).await;
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = first.store.session_reverts(session.clone()).await.unwrap();
        if rows.iter().any(|r| r.step == RevertStep::RollbackSent) {
            break;
        }
        assert!(Instant::now() < deadline, "no rollback_sent: {rows:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // `rollback_sent` is committed before the frame leaves: wait until the
    // app-server logged it.
    let deadline = Instant::now() + WAIT;
    while rig.method_count("thread/rollback") < 1 {
        assert!(
            Instant::now() < deadline,
            "the rollback frame never arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rig.method_count("thread/rollback"), 1);
    // A station crash: no shutdown, the old station's tasks are left
    // behind (a takeover fences its shim attachment), and a new station
    // opens the same database.
    drop(first);

    let second = rig.station().await;
    let rows = second.store.session_reverts(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].step, RevertStep::ReconciliationRequired);
    let summary = second
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        summary.state,
        SessionState::Running,
        "restored under its shim"
    );
    let decided = rig
        .submit(
            &second,
            SessionCommand::Prompt {
                session_id: session.clone(),
                input: text("blocked"),
            },
        )
        .await;
    assert_eq!(
        decided.receipt.state,
        ReceiptState::Rejected,
        "input blocked"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rig.method_count("thread/rollback"),
        1,
        "the rollback is never sent again"
    );
    assert_eq!(
        rig.file(),
        "one\nedited\n",
        "no file restored without a confirmed rollback"
    );
    assert_eq!(rig.method_count("initialize"), 1);
    second.manager.shutdown(StopMode::Graceful).await;
}
