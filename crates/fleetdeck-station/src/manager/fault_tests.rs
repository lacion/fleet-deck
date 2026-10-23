//! Fault tests inside one station process (M2 P2-8, PLAN §6.4, §11): a
//! worker restart and a provider restart, as separate cases, against
//! `fake-claude sim`. The station crash cases (one `fleetdeck station
//! serve` process aborted at each effect boundary) are in
//! `crates/fleetdeck-cli/tests/station_faults.rs`.
//!
//! A `Gate` target wraps the real `SessionTarget` and holds the worker at
//! one point of one effect, so a test can crash the worker exactly there.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    ApprovalDecision, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec,
    UserInput,
};
use fleetdeck_proto::event::{ExpiredReason, ProviderKind, SessionState};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, RequestId, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::fake::{SCENARIO_ENV, STDIN_LOG_ENV};
use fleetdeck_testkit::sim_claude::StdinLog;
use fleetdeck_testkit::{Det, FakeAgent};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use super::target::SessionTarget;
use super::*;
use crate::store::{
    Dispatch, Effect, EffectState, OfflineApprovalPolicy, ReadResult, RequestState, StoreConfig,
};
use crate::worker::{EffectTarget, EffectWorker};

const WAIT: Duration = Duration::from_secs(10);

const OPEN: u8 = 0;
const HOLD_BEFORE_DISPATCH: u8 = 1;
const HOLD_AFTER_DISPATCH: u8 = 2;

/// Holds the first `prompt` effect before or after the real dispatch.
struct Gate {
    inner: Arc<SessionTarget>,
    mode: AtomicU8,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl EffectTarget for Gate {
    async fn dispatch(&self, effect: &Effect) -> Dispatch {
        let mode = if effect.payload.kind() == "prompt" {
            self.mode.swap(OPEN, Ordering::SeqCst)
        } else {
            OPEN
        };
        if mode == HOLD_BEFORE_DISPATCH {
            self.reached.notify_one();
            self.release.notified().await;
        }
        let dispatch = self.inner.dispatch(effect).await;
        if mode == HOLD_AFTER_DISPATCH {
            self.reached.notify_one();
            self.release.notified().await;
        }
        dispatch
    }
}

struct Rig {
    det: Det,
    store: Store,
    policy: Arc<Policy>,
    target: Arc<SessionTarget>,
    gate: Arc<Gate>,
    root: tempfile::TempDir,
    log: PathBuf,
    worker: Option<JoinHandle<()>>,
    shutdown: watch::Sender<bool>,
    next: u32,
}

impl Rig {
    async fn new(hold: u8) -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
        let log = root.path().join("stdin.log");
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, log.to_str().unwrap()).unwrap();
        let mut config =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        config.delivery_grace = Duration::from_millis(500);
        let adapter: Arc<dyn fleetdeck_providers::ProviderAdapter> =
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
        let policy = Policy::new(
            vec![root.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let hub = Arc::new(PolicyHub::new(policy.clone()));
        let policy = Arc::new(policy);
        let target = Arc::new(
            SessionTarget::new(
                store.clone(),
                Adapters::from([("claude".into(), adapter)]),
                det.ids(),
            )
            .with_policy(hub)
            .with_private(Arc::new(crate::workspace::PrivatePaths::default())),
        );
        let gate = Arc::new(Gate {
            inner: Arc::clone(&target),
            mode: AtomicU8::new(hold),
            reached: Notify::new(),
            release: Notify::new(),
        });
        let (shutdown, _) = watch::channel(false);
        let mut rig = Self {
            det,
            store,
            policy,
            target,
            gate,
            root,
            log,
            worker: None,
            shutdown,
            next: 0,
        };
        rig.spawn_worker().await;
        rig
    }

    /// Registers a new worker (which fences the old one) and runs it.
    async fn spawn_worker(&mut self) {
        let worker = EffectWorker::register(self.store.clone(), Arc::clone(&self.gate) as Arc<_>)
            .await
            .unwrap();
        let rx = self.shutdown.subscribe();
        self.worker = Some(tokio::spawn(async move {
            worker.run(rx).await.unwrap();
        }));
    }

    /// The worker task dies where it is (a crash of the worker only).
    async fn crash_worker(&mut self) {
        let task = self.worker.take().unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    async fn send(&mut self, id: &str, command: SessionCommand) -> Decided {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(id.to_owned()),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        let policy = Arc::clone(&self.policy);
        let origin = envelope.origin;
        let command = envelope.command.clone();
        self.store
            .decide(envelope, Caller::local(), move |d| {
                decide::decide(
                    d,
                    &policy,
                    &crate::coordinator::Bindings::default(),
                    origin,
                    false,
                    command,
                )
            })
            .await
            .unwrap()
    }

    async fn accepted(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let id = format!("cmd-{}", self.next);
        let d = self.send(&id, command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    async fn start(&mut self, prompt: &str) -> SessionId {
        let cwd = self.root.path().join("projects/app");
        let d = self
            .accepted(SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "claude".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    mode: Some("default".to_owned()),
                    model: None,
                    title: None,
                    coordinator: None,
                    launch: None,
                    extra_env: None,
                },
                prompt: Some(text(prompt)),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
                fleet_git: None,
            })
            .await;
        d.receipt.result.unwrap().session_id.unwrap()
    }

    async fn events(&self) -> Vec<(SessionId, Event)> {
        match self.store.read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events
                .into_iter()
                .filter_map(|e| match e.event {
                    EventBody::Known(k) => Some((e.session_id, k)),
                    EventBody::Unknown { .. } => None,
                })
                .collect(),
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    /// Waits until `n` events of `session` match.
    async fn wait_events(&self, what: &str, session: &SessionId, n: usize, f: fn(&Event) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let found = self
                .events()
                .await
                .iter()
                .filter(|(s, e)| s == session && f(e))
                .count();
            if found >= n {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn effect_of(&self, session: &SessionId, command: &str) -> Effect {
        self.store
            .session_effects(session.clone())
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.command_id.as_ref().is_some_and(|c| c.as_str() == command))
            .unwrap()
    }

    async fn wait_effect(
        &self,
        session: &SessionId,
        command: &str,
        want: &[EffectState],
    ) -> Effect {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let effect = self.effect_of(session, command).await;
            if want.contains(&effect.state) {
                return effect;
            }
            assert!(tokio::time::Instant::now() < deadline, "{effect:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn reached(&self) {
        tokio::time::timeout(WAIT, self.gate.reached.notified())
            .await
            .expect("the worker reached the gate");
    }

    fn stdin(&self) -> StdinLog {
        StdinLog::read(&self.log)
    }

    async fn close(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.worker.take() {
            let _ = task.await;
        }
        self.target
            .stop_all(fleetdeck_proto::command::StopMode::Kill)
            .await;
    }
}

fn text(t: &str) -> UserInput {
    UserInput::new(t.to_owned())
}

fn prompt(session: &SessionId, t: &str) -> SessionCommand {
    SessionCommand::Prompt {
        session_id: session.clone(),
        input: text(t),
    }
}

fn turn_done(e: &Event) -> bool {
    matches!(e, Event::TurnCompleted { .. })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_crash_after_the_claim_sends_nothing_and_the_session_keeps_working() {
    let mut rig = Rig::new(HOLD_BEFORE_DISPATCH).await;
    let session = rig.start("hello one").await;
    rig.wait_events("the first turn", &session, 1, turn_done)
        .await;

    rig.send("two", prompt(&session, "hello two")).await;
    rig.reached().await;
    assert_eq!(
        rig.effect_of(&session, "two").await.state,
        EffectState::Claimed
    );
    rig.crash_worker().await;
    rig.spawn_worker().await;
    // The new worker's fence turns the old claim into `outcome_unknown`; it
    // never sends it.
    let effect = rig.effect_of(&session, "two").await;
    assert_eq!(effect.state, EffectState::OutcomeUnknown);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.stdin().count("hello two"), 0, "never sent");
    assert_eq!(
        rig.effect_of(&session, "two").await.state,
        EffectState::OutcomeUnknown
    );

    // The provider process lives on: the owner sends a new command.
    rig.send("three", prompt(&session, "hello three")).await;
    rig.wait_events("the second turn", &session, 2, turn_done)
        .await;
    rig.wait_effect(&session, "three", &[EffectState::Confirmed])
        .await;
    let log = rig.stdin();
    assert_eq!((log.count("hello two"), log.count("hello three")), (0, 1));
    assert_eq!(log.pids.len(), 1, "no provider restart");

    // A duplicate of an accepted command returns its receipt and sends nothing.
    let again = rig.send("three", prompt(&session, "hello three")).await;
    assert!(again.duplicate);
    assert!(again.effects.is_empty());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.stdin().count("hello three"), 1);
    rig.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_crash_after_the_send_never_resends_and_late_evidence_confirms() {
    let mut rig = Rig::new(HOLD_AFTER_DISPATCH).await;
    let session = rig.start("hello one").await;
    rig.wait_events("the first turn", &session, 1, turn_done)
        .await;

    rig.send("two", prompt(&session, "hello two")).await;
    rig.reached().await;
    rig.crash_worker().await;
    rig.spawn_worker().await;
    // The write happened; the provider's evidence (`input.updated
    // started`) confirms the effect even after the claim became unknown.
    rig.wait_events("the second turn", &session, 2, turn_done)
        .await;
    rig.wait_effect(&session, "two", &[EffectState::Confirmed])
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = rig.stdin();
    assert_eq!(log.count("hello two"), 1, "sent once, never again");
    assert_eq!(log.pids.len(), 1);
    rig.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_crash_settles_its_effects_expires_its_request_and_rejects_a_stale_reply() {
    let mut rig = Rig::new(HOLD_BEFORE_DISPATCH).await;
    let session = rig.start("ask ls").await;
    rig.wait_events("the approval", &session, 1, |e| {
        matches!(e, Event::ApprovalRequested { .. })
    })
    .await;
    let request: RequestId = rig
        .events()
        .await
        .into_iter()
        .find_map(|(_, e)| match e {
            Event::ApprovalRequested { request_id, .. } => Some(request_id),
            _ => None,
        })
        .unwrap();

    // One prompt is claimed (held at the gate), one waits in the queue.
    rig.send("held", prompt(&session, "held prompt")).await;
    rig.reached().await;
    rig.send("queued", prompt(&session, "queued prompt")).await;

    // The provider process dies (`kill -9`); the station lives on.
    let pid = rig.stdin().pids[0];
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    rig.wait_events("the session end", &session, 1, |e| {
        matches!(e, Event::SessionEnded { .. })
    })
    .await;
    let queued = rig
        .wait_effect(&session, "queued", &[EffectState::RetiredNotDispatched])
        .await;
    assert_eq!(queued.reason.as_deref(), Some("process_lost"));
    rig.wait_effect(&session, "held", &[EffectState::OutcomeUnknown])
        .await;
    let open = rig
        .store
        .runtime_request(session.clone(), request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.state, RequestState::Expired);
    assert!(rig.events().await.iter().any(|(_, e)| matches!(
        e,
        Event::ApprovalExpired { request_id, reason: ExpiredReason::ProcessLost }
            if *request_id == request
    )));
    assert_eq!(
        rig.store
            .summary(session.clone())
            .await
            .unwrap()
            .unwrap()
            .state,
        SessionState::Exited
    );

    // The held claim goes on to a dead target: nothing is written.
    rig.gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.effect_of(&session, "held").await.state,
        EffectState::OutcomeUnknown
    );

    // A stale approval reply is rejected and queues nothing.
    let stale = rig
        .send(
            "stale",
            SessionCommand::Answer {
                session_id: session.clone(),
                request_id: request.clone(),
                decision: ApprovalDecision {
                    answered_by: None,
                    option_id: "allow".into(),
                    updated_input: None,
                    text: None,
                },
            },
        )
        .await;
    assert_eq!(stale.receipt.state, ReceiptState::Rejected);
    assert_eq!(
        stale.receipt.reject.as_ref().map(|r| r.code),
        Some(fleetdeck_proto::RejectCode::RequestClosed)
    );
    assert!(stale.effects.is_empty());

    // The owner starts a new provider process (a new session): the station
    // replays nothing of the dead one into it.
    let next = rig.start("hello again").await;
    rig.wait_events("the new turn", &next, 1, turn_done).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = rig.stdin();
    assert_eq!(log.pids.len(), 2, "two provider processes");
    assert_eq!(log.count("held prompt"), 0);
    assert_eq!(log.count("queued prompt"), 0);
    assert_eq!(
        log.count("control_response"),
        0,
        "the stale answer never reached a provider"
    );
    assert_eq!(log.count("hello again"), 1);
    rig.close().await;
}
