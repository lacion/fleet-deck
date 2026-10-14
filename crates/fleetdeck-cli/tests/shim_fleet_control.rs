//! The authority gate of a fleet-control effect on the shim host (ADR-008
//! §6.2, RE-REVIEW round 2): the deck is asked after every awaited
//! preparation, right before the first byte reaches the provider. Real
//! shims, the fake Codex app-server, a fake deck that revokes while the
//! station holds the effect at the gate.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::{AuthorityCheck, CommandAuthority};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::coordinator::Transport;
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, Effect, EffectState, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_station::workspace::PrivatePaths;
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

/// The deck: it answers `coordinator.authority_check` with the reply that the
/// test sets, and records every question.
struct FakeDeck {
    valid: Mutex<bool>,
    asked: Mutex<Vec<AuthorityCheck>>,
}

impl FakeDeck {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            valid: Mutex::new(true),
            asked: Mutex::default(),
        })
    }

    fn revoke(&self) {
        *self.valid.lock().unwrap() = false;
    }

    fn asked(&self) -> Vec<AuthorityCheck> {
        self.asked.lock().unwrap().clone()
    }
}

// `Transport` is an `async_trait`; this crate has no `async-trait`, so the
// method has the desugared form.
impl Transport for FakeDeck {
    fn request<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        method: &'life1 str,
        _command_id: Option<&'life2 CommandId>,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Outcome, String>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            assert_eq!(method, "coordinator.authority_check");
            let check: AuthorityCheck = serde_json::from_value(params).unwrap();
            self.asked.lock().unwrap().push(check);
            if *self.valid.lock().unwrap() {
                Ok(Outcome::Ok(serde_json::json!({"valid": true})))
            } else {
                Ok(Outcome::Ok(
                    serde_json::json!({"valid": false, "reason": "revoked"}),
                ))
            }
        })
    }
}

struct Rig {
    tmp: tempfile::TempDir,
    det: Det,
    next: u32,
    store: Store,
    manager: Arc<SessionManager>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

impl Rig {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/app")).unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        let ws = tmp.path().join("projects/app");
        git(&ws, &["init", "-q", "-b", "main"]);
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        git(&ws, &["add", "."]);
        git(&ws, &["commit", "-q", "-m", "init"]);
        let det = Det::new();
        let store = Store::open(
            &tmp.path().join("station.db"),
            det.clock(),
            det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let policy = Policy::new(
            vec![tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("codex".into(), ProviderKind::Codex)]),
        )
        .with_allow_fleet_control(true);
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, tmp.path().join("codex-stdin.log"))
            .unwrap();
        let mut config =
            CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        let adapters = Adapters::from([(
            "codex".into(),
            Arc::new(CodexAdapter::new(config, Arc::new(DirectChildHost::new())))
                as Arc<dyn fleetdeck_providers::ProviderAdapter>,
        )]);
        let mut host = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        host.runtime_dir = None;
        host.tmp_dir = tmp.path().to_owned();
        host.buffer_bytes = 1024 * 1024;
        let shims = ShimSettings {
            host: Arc::new(ShimHost::new(host)),
            in_flight_wait: Duration::from_secs(1),
            rollback_timeout: Duration::from_millis(500),
        };
        let manager = SessionManager::start_with_shims(
            store.clone(),
            policy,
            adapters,
            det.ids(),
            shims,
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        Self {
            tmp,
            det,
            next: 0,
            store,
            manager: Arc::new(manager),
        }
    }

    fn workspace(&self) -> String {
        std::fs::canonicalize(self.tmp.path().join("projects/app"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// A command as the deck routes it for the coordinator `coord-1`, or a
    /// local one.
    async fn send(&mut self, coordinator: bool, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: coordinator.then(|| CommandAuthority {
                grant_id: "g1".to_owned(),
                caller_station: "alpha".into(),
                caller_session: "coord-1".into(),
                generation: 1,
            }),
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: if coordinator {
                CommandOrigin::Coordinator
            } else {
                CommandOrigin::Local
            },
            command,
        };
        let decided = self
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

    fn start_command(&self, prompt: &str) -> SessionCommand {
        SessionCommand::Start {
            spec: SessionSpec {
                provider_instance: "codex".into(),
                cwd: self.workspace(),
                mode: None,
                model: None,
                title: None,
                coordinator: None,
            },
            prompt: Some(UserInput::new(prompt.to_owned())),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        }
    }

    fn turn_starts(&self) -> usize {
        std::fs::read_to_string(self.tmp.path().join("codex-stdin.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|f| f["method"] == "turn/start")
            .count()
    }

    async fn events(&self, session: &SessionId) -> Vec<Event> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 10_000).await.unwrap()
        else {
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

    async fn errors(&self, session: &SessionId) -> Vec<String> {
        self.events(session)
            .await
            .into_iter()
            .filter_map(|e| match e {
                Event::Error { code, .. } => Some(code),
                _ => None,
            })
            .collect()
    }

    async fn wait_events(&self, session: &SessionId, what: &str, pick: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            let all = self.events(session).await;
            if pick(&all) {
                return;
            }
            assert!(Instant::now() < deadline, "no {what}: {all:?}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn effect_of(&self, session: &SessionId, command: &CommandId) -> Option<Effect> {
        self.store
            .session_effects(session.clone())
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.command_id.as_ref() == Some(command))
    }
}

fn link(rig: &Rig, deck: &Arc<FakeDeck>) {
    rig.manager
        .coordinator_bindings()
        .set_deck(1, Arc::clone(deck) as Arc<dyn Transport>);
}

// A revocation that happens after the transition and the claim of the first
// frame (the test holds the driver there) stops the write: no byte reaches
// the provider, the transition is dropped, the effect is `authority_lost`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_after_the_transition_and_the_frame_claim_stops_the_durable_write() {
    let mut rig = Rig::new().await;
    let deck = FakeDeck::new();
    link(&rig, &deck);
    let command = rig.start_command("hello");
    let decided = rig.send(false, command).await;
    let session = decided.receipt.result.unwrap().session_id.unwrap();
    rig.wait_events(&session, "the first turn", |e| {
        e.iter().any(|e| matches!(e, Event::TurnCompleted { .. }))
    })
    .await;
    assert_eq!(rig.turn_starts(), 1);
    let shim = rig.store.shims_of_session(session.clone()).await.unwrap();
    assert_eq!(shim.len(), 1, "the session runs under a shim");

    let manager = Arc::clone(&rig.manager);
    let hook = manager.gate_hook();
    hook.arm();
    let decided = rig
        .send(
            true,
            SessionCommand::FollowUp {
                session_id: session.clone(),
                input: UserInput::new("again".to_owned()),
            },
        )
        .await;
    let command_id = decided.receipt.command_id.clone();
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .expect("the write reaches the gate");
    // Held after the preparation: the transition and the claim of frame 0
    // exist, the deck is not asked yet, nothing was written.
    let effect = rig.effect_of(&session, &command_id).await.unwrap();
    let held = rig.store.shim_transitions(shim[0].clone()).await.unwrap();
    let transition = held
        .iter()
        .find(|t| t.effect.effect_id == effect.effect_id)
        .expect("the transition is recorded");
    assert_eq!(transition.claimed, 1, "the first frame is claimed");
    assert!(deck.asked().is_empty(), "nothing is asked before the gate");
    assert_eq!(rig.turn_starts(), 1);
    deck.revoke();
    hook.release.notify_one();

    let deadline = Instant::now() + WAIT;
    let retired = loop {
        let effect = rig.effect_of(&session, &command_id).await.unwrap();
        if effect.state == EffectState::RetiredNotDispatched {
            break effect;
        }
        assert!(Instant::now() < deadline, "{effect:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(retired.reason.as_deref(), Some("authority_lost"));
    assert_eq!(deck.asked().len(), 1);
    assert_eq!(rig.errors(&session).await, ["authority_lost"]);
    let after = rig.store.shim_transitions(shim[0].clone()).await.unwrap();
    assert!(
        after.iter().all(|t| t.effect.effect_id != effect.effect_id),
        "the transition is dropped: {after:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.turn_starts(), 1, "no byte reached the provider");
    rig.manager.shutdown(StopMode::Graceful).await;
}

// The first input of a durable start is gated inside `first_input`, before
// its commit: a revocation while the start waits there sends no byte and
// commits no turn; the session stays alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_before_the_first_input_of_a_durable_start_stops_the_write() {
    let mut rig = Rig::new().await;
    let deck = FakeDeck::new();
    link(&rig, &deck);
    let manager = Arc::clone(&rig.manager);
    let hook = manager.gate_hook();
    hook.arm();
    let command = rig.start_command("hello");
    let decided = rig.send(true, command).await;
    let session = decided.receipt.result.unwrap().session_id.unwrap();
    // The first hold is the launch gate, after the `starting` row.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .expect("the launch reaches its gate");
    // Hold the first-input gate too, then let the launch go on.
    hook.arm();
    hook.release.notify_one();
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .expect("the first input reaches its gate");
    assert_eq!(deck.asked().len(), 1, "only the launch gate was asked");
    assert_eq!(rig.turn_starts(), 0, "no input was written yet");
    deck.revoke();
    hook.release.notify_one();

    rig.wait_events(&session, "the refusal", |e| {
        e.iter()
            .any(|e| matches!(e, Event::Error { code, .. } if code == "authority_lost"))
    })
    .await;
    assert_eq!(deck.asked().len(), 2);
    for check in deck.asked() {
        let launch = check
            .launch
            .expect("every gate of a start sends the launch");
        assert_eq!(launch.provider, ProviderKind::Codex);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.turn_starts(), 0, "no byte reached the provider");
    let events = rig.events(&session).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::TurnCompleted { .. } | Event::SessionEnded { .. })),
        "{events:?}"
    );
    rig.manager.shutdown(StopMode::Graceful).await;
}
