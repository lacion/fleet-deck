//! GATE-M2 P2-a: a start effect whose process spawns but whose first input
//! fails records durable evidence for that input. Before the fix the
//! failure was only logged and the input vanished without a trace.
//!
//! `FailFirst` wraps the real Claude adapter (`fake-claude sim`) and fails
//! the first `prompt` of each session with a chosen error; every other
//! operation goes to the real session.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{InputState, ProviderKind, SessionState, TurnStatus};
use fleetdeck_proto::{
    Capabilities, CommandEnvelope, CommandId, Event, EventBody, EventEnvelope, InputId, SessionId,
};
use fleetdeck_providers::adapter::{NativeSessionId, ProviderProbe};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, HostError, StopPolicy};
use fleetdeck_providers::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, PermissionMode, ProviderAdapter,
    ProviderError,
};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use super::*;
use crate::store::{
    EffectState, FIRST_INPUT_NOT_SENT, FIRST_INPUT_UNKNOWN, OfflineApprovalPolicy, ReadResult,
    StoreConfig,
};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum Failure {
    /// Refused before any I/O.
    Refused,
    /// A write that can be partial.
    BrokenPipe,
}

impl Failure {
    fn error(self) -> ProviderError {
        match self {
            Self::Refused => ProviderError::InvalidInput("first".to_owned()),
            Self::BrokenPipe => ProviderError::Host(HostError::Write(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the stdin writer stopped",
            ))),
        }
    }
}

struct FailFirst {
    inner: Arc<dyn ProviderAdapter>,
    failure: Failure,
}

#[async_trait]
impl ProviderAdapter for FailFirst {
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
        Ok(Box::new(FailingSession {
            inner: session,
            failure: Some(self.failure),
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

struct FailingSession {
    inner: Box<dyn LiveSession>,
    failure: Option<Failure>,
}

#[async_trait]
impl LiveSession for FailingSession {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    async fn prompt(&mut self, input: HostInput) -> fleetdeck_providers::adapter::Result<()> {
        match self.failure.take() {
            Some(failure) => Err(failure.error()),
            None => self.inner.prompt(input).await,
        }
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

struct Rig {
    det: Det,
    manager: SessionManager,
    _root: tempfile::TempDir,
    cwd: String,
    next: u32,
}

impl Rig {
    async fn new(failure: Failure) -> Self {
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
        let adapter: Arc<dyn ProviderAdapter> = Arc::new(FailFirst {
            inner: real,
            failure,
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
        Self {
            det,
            manager,
            cwd: cwd.to_string_lossy().into_owned(),
            _root: root,
            next: 0,
        }
    }

    async fn accepted(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("cmd-{}", self.next)),
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
        d
    }

    async fn start(&mut self) -> (SessionId, InputId) {
        let spec = SessionSpec {
            provider_instance: "claude".into(),
            cwd: self.cwd.clone(),
            mode: Some("default".to_owned()),
            model: None,
            title: None,
        };
        let d = self
            .accepted(SessionCommand::Start {
                spec,
                prompt: Some(UserInput::new("hello".to_owned())),
            })
            .await;
        let result = d.receipt.result.unwrap();
        (result.session_id.unwrap(), result.input_id.unwrap())
    }

    async fn events(&self) -> Vec<EventEnvelope> {
        match self.manager.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events,
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    async fn wait_for<T>(&self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            for e in self.events().await {
                if let EventBody::Known(event) = &e.event
                    && let Some(found) = pick(event)
                {
                    return found;
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The start effect once its reason is stored.
    async fn start_effect(&self, session: &SessionId) -> crate::store::Effect {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let effects = self
                .manager
                .store()
                .session_effects(session.clone())
                .await
                .unwrap();
            let start = effects.into_iter().next().unwrap();
            if start.state == EffectState::Confirmed && start.reason.is_some() {
                return start;
            }
            assert!(tokio::time::Instant::now() < deadline, "{start:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A later prompt still runs a turn: the session stays usable.
    async fn later_prompt_runs(&mut self, session: &SessionId) {
        self.accepted(SessionCommand::Prompt {
            session_id: session.clone(),
            input: UserInput::new("again".to_owned()),
        })
        .await;
        self.wait_for("turn done", |e| {
            matches!(
                e,
                Event::TurnCompleted {
                    status: TurnStatus::Done,
                    ..
                }
            )
            .then_some(())
        })
        .await;
        let state = self
            .manager
            .store()
            .summary(session.clone())
            .await
            .unwrap()
            .unwrap()
            .state;
        assert_ne!(state, SessionState::Exited);
    }
}

#[tokio::test]
async fn a_refused_first_input_is_discarded_with_an_error_and_the_session_stays_usable() {
    let mut rig = Rig::new(Failure::Refused).await;
    let (session, input) = rig.start().await;
    let message = rig
        .wait_for("first input error", |e| match e {
            Event::Error { code, message, .. } if code == FIRST_INPUT_NOT_SENT => {
                Some(message.clone())
            }
            _ => None,
        })
        .await;
    assert!(message.contains(input.as_str()), "{message}");
    let discarded = rig.events().await.iter().any(|e| match &e.event {
        EventBody::Known(Event::InputUpdated {
            input_id,
            state: InputState::Discarded,
            ..
        }) => *input_id == input,
        _ => false,
    });
    assert!(discarded, "the first input is not marked discarded");
    let start = rig.start_effect(&session).await;
    let reason = start.reason.unwrap();
    assert!(reason.starts_with(FIRST_INPUT_NOT_SENT), "{reason}");
    rig.later_prompt_runs(&session).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_first_input_that_may_have_reached_the_provider_is_unknown_and_never_resent() {
    let mut rig = Rig::new(Failure::BrokenPipe).await;
    let (session, input) = rig.start().await;
    let message = rig
        .wait_for("delivery unknown", |e| match e {
            Event::Error { code, message, .. } if code == "delivery_unknown" => {
                Some(message.clone())
            }
            _ => None,
        })
        .await;
    assert!(message.contains(input.as_str()), "{message}");
    let start = rig.start_effect(&session).await;
    let reason = start.reason.unwrap();
    assert!(reason.starts_with(FIRST_INPUT_UNKNOWN), "{reason}");
    // An unknown outcome is not a refusal: the input is not discarded.
    let discarded = rig.events().await.iter().any(|e| {
        matches!(
            &e.event,
            EventBody::Known(Event::InputUpdated {
                state: InputState::Discarded,
                ..
            })
        )
    });
    assert!(!discarded);
    rig.later_prompt_runs(&session).await;
    rig.manager.shutdown(StopMode::Kill).await;
}
