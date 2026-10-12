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
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId, TurnId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::shim_station::recover_station;
use fleetdeck_station::manager::target::{SessionTarget, ShimSettings};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager, durable_configs};
use fleetdeck_station::store::{
    Caller, Decided, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_station::workspace::{GitOptions, settle_unsettled};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};
use serde_json::Value;

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(20);
/// The slow turn: long enough that the follow-up is committed before it
/// ends, also on a loaded machine (the full `make check` runs in parallel).
const SLOW_MS: u64 = 5000;

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

    fn shim_settings(&self, store: &Store) -> ShimSettings {
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
        let shims = self.shim_settings(&store);
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

// Buffered output across a station restart (DL-114). The station is down
// while the provider writes; the shim keeps the output. The restored
// driver replays it during the restoration, and the session's observer
// must see every turn and exit event of that replay.

async fn start_session(rig: &mut Rig, station: &Station, prompt: &str) -> SessionId {
    let decided = rig
        .send(
            station,
            SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "codex".into(),
                    cwd: rig.workspace(),
                    mode: None,
                    model: None,
                    title: None,
                },
                prompt: Some(UserInput {
                    text: prompt.to_owned(),
                }),
            },
        )
        .await;
    decided
        .receipt
        .result
        .and_then(|r| r.session_id)
        .expect("a session id")
}

async fn wait_for(store: &Store, session: &SessionId, what: &str, pick: impl Fn(&[Event]) -> bool) {
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

fn started_turns(events: &[Event]) -> Vec<TurnId> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TurnStarted { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .collect()
}

fn finalized_turns(events: &[Event]) -> Vec<TurnId> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TurnFinalized { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        })
        .collect()
}

/// A first station runs a session, a slow second turn and a follow-up that
/// the adapter holds until that turn ends; then the station stops while
/// the slow turn still runs.
async fn slow_turn_and_follow_up(rig: &mut Rig) -> (SessionId, Store) {
    slow_turn_and(rig, "two").await
}

/// [`slow_turn_and_follow_up`] with the follow-up text `follow_up`.
async fn slow_turn_and(rig: &mut Rig, follow_up: &str) -> (SessionId, Store) {
    let first = rig.station().await;
    let session = start_session(rig, &first, "hello").await;
    finalized(&first.store, &session, 1).await;
    // A slow turn, and a follow-up that the adapter holds until it ends.
    rig.send(
        &first,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput {
                text: format!("slow {SLOW_MS} one"),
            },
        },
    )
    .await;
    wait_for(&first.store, &session, "the slow turn", |e| {
        started_turns(e).len() == 2
    })
    .await;
    rig.send(
        &first,
        SessionCommand::FollowUp {
            session_id: session.clone(),
            input: UserInput {
                text: follow_up.to_owned(),
            },
        },
    )
    .await;
    // The driver committed the follow-up (no frame yet: the adapter holds it
    // while the slow turn runs): as a transition, or, after a later output
    // line committed the state, in the stored adapter checkpoint.
    let shim_id = first
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let transitions = first.store.shim_transitions(shim_id.clone()).await.unwrap();
        let checkpoint = first
            .store
            .shim_checkpoint(shim_id.clone())
            .await
            .unwrap()
            .unwrap_or_default();
        if !transitions.is_empty() || checkpoint.contains(follow_up) {
            break;
        }
        assert!(Instant::now() < deadline, "the follow-up was not committed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        rig.method_count("turn/start"),
        2,
        "the slow turn still runs"
    );
    let old_store = first.store.clone();
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);
    (session, old_store)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turns_that_end_and_start_while_the_station_is_down_are_finalized_after_the_restart() {
    let mut rig = Rig::new();
    let (session, old_store) = slow_turn_and_follow_up(&mut rig).await;
    // The slow turn ends while no station reads: the shim buffers its
    // completion. The follow-up waits in the committed adapter state,
    // because the adapter sends it only after it reads that completion.
    tokio::time::sleep(Duration::from_millis(SLOW_MS + 1500)).await;
    let before = events(&old_store, &session).await;
    assert_eq!(
        started_turns(&before).len(),
        2,
        "nothing was read while down"
    );
    assert!(
        !before
            .iter()
            .any(|e| matches!(e, Event::TurnCompleted { turn_id, .. } if *turn_id == started_turns(&before)[1])),
        "the slow turn's completion was not read while down"
    );
    assert_eq!(
        rig.method_count("turn/start"),
        2,
        "no frame left while down"
    );

    let second = rig.station().await;
    // The restored pump replays the buffered completion; the restored
    // adapter then sends the follow-up, whose turn starts and ends during
    // the restoration of the observer's range.
    wait_for(&second.store, &session, "turn three completed", |e| {
        let started = started_turns(e);
        started.len() == 3
            && e.iter().any(
                |e| matches!(e, Event::TurnCompleted { turn_id, .. } if *turn_id == started[2]),
            )
    })
    .await;
    assert_eq!(
        rig.method_count("turn/start"),
        3,
        "the follow-up was sent once"
    );
    let all = events(&second.store, &session).await;
    let turns = started_turns(&all);
    // Every turn has exactly one turn.finalized, including the turn that
    // started and ended during the replay.
    let deadline = Instant::now() + WAIT;
    loop {
        let all = events(&second.store, &session).await;
        let done = finalized_turns(&all);
        if turns.iter().all(|t| done.contains(t)) {
            for t in &turns {
                assert_eq!(done.iter().filter(|d| *d == t).count(), 1, "{all:?}");
            }
            break;
        }
        assert!(Instant::now() < deadline, "unfinalized turns: {all:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The session is idle and takes input; its next turn has both
    // checkpoints.
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
    let done = finalized(&second.store, &session, 4).await;
    assert!(both_available(&done[3]), "{done:?}");
    assert_eq!(rig.method_count("initialize"), 1, "no second handshake");
    second.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_that_exits_while_the_station_is_down_ends_its_session_after_the_restart() {
    let mut rig = Rig::new();
    let first = rig.station().await;
    let session = start_session(&mut rig, &first, "hello").await;
    finalized(&first.store, &session, 1).await;
    let shim_id = first
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    let pid = first
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .provider_identity
        .unwrap()
        .pid;
    first.manager.shutdown(StopMode::Graceful).await;
    drop(first);
    // The provider dies while no station runs; the shim keeps its exit.
    shim_reaper::kill9(pid);

    let second = rig.station().await;
    wait_for(&second.store, &session, "session.ended", |e| {
        e.iter().any(|e| matches!(e, Event::SessionEnded { .. }))
    })
    .await;
    // The observer saw the replayed exit: the process ended, the shim row
    // is `exited`, and the session takes no more input.
    let deadline = Instant::now() + WAIT;
    loop {
        let live = second.store.live_process(session.clone()).await.unwrap();
        let open = second
            .store
            .shims_of_session(session.clone())
            .await
            .unwrap();
        if live.is_none() && open.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live {live:?}, open shim rows {open:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let row = second.store.shim(shim_id).await.unwrap().unwrap();
    assert_eq!(row.reason.as_deref(), Some("exited"), "{row:?}");
    let summary = second
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    second.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adoption_observes_the_turns_that_restored_drivers_committed_before_it() {
    let mut rig = Rig::new();
    let (session, _old) = slow_turn_and_follow_up(&mut rig).await;
    tokio::time::sleep(Duration::from_millis(SLOW_MS + 1500)).await;
    // The steps of a station start, with a deterministic delay between the
    // restoration and the adoption.
    let store = Store::open(
        &rig.tmp.path().join("station.db"),
        rig.det.clock(),
        rig.det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    settle_unsettled(&store, GitOptions::isolated())
        .await
        .unwrap();
    let from = *store.subscribe().borrow();
    let settings = rig.shim_settings(&store);
    let start = recover_station(
        &store,
        &settings.host,
        &durable_configs(&rig.adapters()),
        settings.in_flight_wait,
    )
    .await
    .unwrap();
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    // The restored driver replays the slow turn's completion, sends the
    // follow-up, and commits its whole turn before the adoption.
    wait_for(&store, &session, "the follow-up turn completed", |e| {
        let started = started_turns(e);
        started.len() == 3
            && e.iter().any(
                |e| matches!(e, Event::TurnCompleted { turn_id, .. } if *turn_id == started[2]),
            )
    })
    .await;
    let target =
        SessionTarget::new(store.clone(), rig.adapters(), rig.det.ids()).with_shims(settings);
    target
        .adopt(start.restored, StopPolicy::default(), from)
        .await;
    // The observer starts at the head before the restoration, so it sees
    // the follow-up turn and finalizes it.
    let all = events(&store, &session).await;
    let third = started_turns(&all)[2].clone();
    wait_for(&store, &session, "the follow-up turn finalized", |e| {
        finalized_turns(e).contains(&third)
    })
    .await;
    target.stop_all(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_buffered_turn_that_completes_before_the_provider_exits_is_finalized_by_the_adoption() {
    let mut rig = Rig::new();
    // The follow-up turn answers and then the app-server exits (`exit-after`).
    let (session, _old) = slow_turn_and(&mut rig, "exit-after bye").await;
    tokio::time::sleep(Duration::from_millis(SLOW_MS + 1500)).await;
    let store = Store::open(
        &rig.tmp.path().join("station.db"),
        rig.det.clock(),
        rig.det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    settle_unsettled(&store, GitOptions::isolated())
        .await
        .unwrap();
    let from = *store.subscribe().borrow();
    let settings = rig.shim_settings(&store);
    let start = recover_station(
        &store,
        &settings.host,
        &durable_configs(&rig.adapters()),
        settings.in_flight_wait,
    )
    .await
    .unwrap();
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    // Before the adoption, the restored driver replays the slow turn's
    // completion, sends the held follow-up, and commits the follow-up
    // turn and the provider's exit.
    wait_for(&store, &session, "the follow-up turn and the exit", |e| {
        let started = started_turns(e);
        started.len() == 3
            && e.iter().any(
                |e| matches!(e, Event::TurnCompleted { turn_id, .. } if *turn_id == started[2]),
            )
            && e.iter().any(|e| matches!(e, Event::SessionEnded { .. }))
    })
    .await;
    assert_eq!(
        store.summary(session.clone()).await.unwrap().unwrap().state,
        SessionState::Exited
    );
    let target =
        SessionTarget::new(store.clone(), rig.adapters(), rig.det.ids()).with_shims(settings);
    target
        .adopt(start.restored, StopPolicy::default(), from)
        .await;
    // The adoption reads the buffered turn before it ends the session: the
    // follow-up turn is finalized, then the process ends and the shim row
    // is `exited`.
    let all = events(&store, &session).await;
    let third = started_turns(&all)[2].clone();
    wait_for(&store, &session, "the follow-up turn finalized", |e| {
        finalized_turns(e).contains(&third)
    })
    .await;
    let deadline = Instant::now() + WAIT;
    loop {
        let live = store.live_process(session.clone()).await.unwrap();
        let open = store.shims_of_session(session.clone()).await.unwrap();
        if live.is_none() && open.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live {live:?}, open shim rows {open:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Every started turn is finalized exactly once.
    let all = events(&store, &session).await;
    let mut done = finalized_turns(&all);
    done.sort();
    let mut started = started_turns(&all);
    started.sort();
    assert_eq!(done, started, "{all:?}");
    assert_eq!(rig.method_count("initialize"), 1);
    target.stop_all(StopMode::Graceful).await;
}

/// Waits until the session's process ended and its shim rows closed, and
/// returns the row of `shim_id` (DL-120).
async fn ended_row(
    store: &Store,
    session: &SessionId,
    shim_id: &str,
) -> fleetdeck_station::store::ShimRow {
    wait_for(store, session, "session.ended", |e| {
        e.iter().any(|e| matches!(e, Event::SessionEnded { .. }))
    })
    .await;
    let deadline = Instant::now() + WAIT;
    loop {
        let live = store.live_process(session.clone()).await.unwrap();
        let open = store.shims_of_session(session.clone()).await.unwrap();
        if live.is_none() && open.is_empty() {
            return store.shim(shim_id.to_owned()).await.unwrap().unwrap();
        }
        assert!(Instant::now() < deadline, "live {live:?}, open {open:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn resume(rig: &mut Rig, station: &Station, session: &SessionId) -> Decided {
    rig.next += 1;
    let envelope = CommandEnvelope {
        command_id: CommandId::from(format!("cmd-{}", rig.next)),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Resume {
            session_id: session.clone(),
        },
    };
    station
        .manager
        .submit(envelope, Caller::local())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_shim_exit_with_a_surviving_child_keeps_the_evidence_and_resume_stays_refused() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let session = start_session(&mut rig, &station, "hello").await;
    finalized(&station.store, &session, 1).await;
    let shim_id = station
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput {
                text: "exit-leaving-child bye".to_owned(),
            },
        },
    )
    .await;
    // The station runs; its observer ends the session from the live exit.
    let row = ended_row(&station.store, &session, &shim_id).await;
    assert_eq!(row.state, fleetdeck_station::store::ShimState::Exited);
    assert_eq!(
        row.reason.as_deref(),
        Some("exited_descendants_remaining"),
        "{row:?}"
    );
    let summary = station
        .store
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    assert!(summary.capabilities.as_ref().unwrap().resume);
    let before = station
        .store
        .session_effects(session.clone())
        .await
        .unwrap()
        .len();
    let decided = resume(&mut rig, &station, &session).await;
    assert_eq!(decided.receipt.state, ReceiptState::Rejected, "{decided:?}");
    let reject = decided.receipt.reject.clone().unwrap();
    assert!(
        reject.message.contains("not_resumable")
            && reject.message.contains("exited_descendants_remaining"),
        "{reject:?}"
    );
    assert_eq!(
        station
            .store
            .session_effects(session.clone())
            .await
            .unwrap()
            .len(),
        before,
        "nothing queued"
    );
    assert_eq!(rig.method_count("thread/resume"), 0);
    station.manager.shutdown(StopMode::Graceful).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_shim_exit_without_survivors_is_a_proven_end() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let session = start_session(&mut rig, &station, "hello").await;
    finalized(&station.store, &session, 1).await;
    let shim_id = station
        .store
        .shims_of_session(session.clone())
        .await
        .unwrap()
        .pop()
        .unwrap();
    rig.send(
        &station,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput {
                text: "exit-after bye".to_owned(),
            },
        },
    )
    .await;
    let row = ended_row(&station.store, &session, &shim_id).await;
    assert_eq!(row.reason.as_deref(), Some("exited"), "{row:?}");
    // The termination check passes; the shim host then refuses the
    // resume launch itself (DL-118), which is not a `not_resumable`
    // rejection.
    let decided = resume(&mut rig, &station, &session).await;
    if let Some(reject) = decided.receipt.reject.as_ref() {
        assert!(!reject.message.contains("not_resumable"), "{reject:?}");
    }
    station.manager.shutdown(StopMode::Graceful).await;
}
