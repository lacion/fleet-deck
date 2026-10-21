//! MCP P4b §11 test 7 (station): a grant with a record returns at every
//! process start with a higher generation, on the direct host against the
//! fake agent and on the shim host through the store of the shim rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CoordinatorSpec, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
    SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_testkit::fake::{LAUNCH_LOG_ENV, RESUME_FAULT_ENV, RESUME_OTHER_ID, SCENARIO_ENV};
use fleetdeck_testkit::{Det, FakeAgent};
use serde_json::Value;

use crate::coordinator::{Binding, BindingState, Bindings, Transport, digest};
use crate::manager::coordinator_spec_tests::coordinator_spec;
use crate::manager::{Adapters, Holders, Policy, SessionManager, hibernate};
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::{Caller, Decided, OfflineApprovalPolicy, ReadResult, Store, StoreConfig};

pub(super) const URL: &str = "http://127.0.0.1:41234/mcp";
const WAIT: Duration = Duration::from_secs(15);
pub(super) const GRANT: &str = "grant-1";

/// A deck link that negotiated `coordinator_live.v1` or not.
pub(super) struct Link {
    pub(super) live: bool,
}

#[async_trait]
impl Transport for Link {
    async fn request(
        &self,
        _method: &str,
        _command_id: Option<&CommandId>,
        _params: Value,
    ) -> Result<Outcome, String> {
        Err("not used".to_owned())
    }

    fn has_feature(&self, name: &str) -> bool {
        self.live && name == feature::COORDINATOR_LIVE
    }
}

pub(super) fn verified(_: ProviderKind, _: Option<&str>) -> Result<(), &'static str> {
    Ok(())
}

pub(super) fn grant(expires_at: i64) -> CoordinatorSpec {
    CoordinatorSpec {
        expires_at,
        ..coordinator_spec()
    }
}

pub(super) fn session_spec(cwd: &str, coordinator: Option<CoordinatorSpec>) -> SessionSpec {
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: cwd.to_owned(),
        mode: Some("default".to_owned()),
        model: None,
        title: None,
        coordinator: coordinator.map(Box::new),
        launch: None,
        extra_env: None,
    }
}

fn start_command(spec: SessionSpec) -> SessionCommand {
    SessionCommand::Start {
        spec,
        prompt: Some(UserInput::new("hello".to_owned())),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

/// The lifecycle events of `session`, as `bound:<binding>@<generation>` and
/// `ended:<binding>@<generation>:<reason>`.
pub(super) fn lifecycle(
    events: &[fleetdeck_proto::EventEnvelope],
    session: &SessionId,
) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.session_id == *session)
        .filter_map(|e| match &e.event {
            EventBody::Known(Event::CoordinatorBound {
                binding_id,
                generation,
                ..
            }) => Some(format!("bound:{binding_id}@{generation}")),
            EventBody::Known(Event::CoordinatorEnded {
                binding_id,
                generation,
                reason,
            }) => Some(format!("ended:{binding_id}@{generation}:{reason}")),
            _ => None,
        })
        .collect()
}

// --- the direct host ------------------------------------------------------

struct Direct {
    det: Det,
    manager: SessionManager,
    bindings: Bindings,
    deck: Caller,
    ws: PathBuf,
    launch_log: PathBuf,
    next: u32,
    _root: tempfile::TempDir,
}

impl Direct {
    /// A station with the fake Claude. `live`: the deck link negotiated
    /// `coordinator_live.v1`. `fault`: the resume fault of the fake.
    async fn new(live: bool, fault: Option<&str>) -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("projects/app");
        std::fs::create_dir_all(&ws).unwrap();
        let launch_log = root.path().join("launch.log");
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let bindings = Bindings::default().with_handoff(verified);
        bindings.set_endpoint(URL.to_owned());
        bindings.set_deck(1, Arc::new(Link { live }));
        let policy = Policy::new(
            vec![root.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(LAUNCH_LOG_ENV, &launch_log).unwrap();
        if let Some(fault) = fault {
            env.set(RESUME_FAULT_ENV, fault).unwrap();
        }
        let mut config =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        config.stop = fleetdeck_providers::host::StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        config.delivery_grace = Duration::from_millis(500);
        let adapter: Arc<dyn fleetdeck_providers::ProviderAdapter> =
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
        let manager = SessionManager::start_with_holders(
            store,
            policy,
            Holders {
                coordinator: bindings.clone(),
                ..Holders::default()
            },
            Adapters::from([("claude".into(), adapter)]),
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        let key = Identity::generate().unwrap().public_key();
        manager
            .store()
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive].into_iter().collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Self {
            det,
            manager,
            bindings,
            deck: Caller::peer(key.fingerprint(), Scope::Drive),
            ws,
            launch_log,
            next: 0,
            _root: root,
        }
    }

    async fn send(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command,
        };
        self.manager
            .submit(envelope, self.deck.clone())
            .await
            .unwrap()
    }

    async fn accepted(&mut self, command: SessionCommand) -> Decided {
        let decided = self.send(command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt
        );
        decided
    }

    /// Starts the session with `coordinator`; its id and the `seq` of the
    /// receipt.
    async fn start(&mut self, coordinator: CoordinatorSpec) -> (SessionId, Option<u64>) {
        let spec = session_spec(&self.ws.to_string_lossy(), Some(coordinator));
        let decided = self.accepted(start_command(spec)).await;
        let result = decided.receipt.result.unwrap();
        (result.session_id.unwrap(), result.seq)
    }

    async fn events(&self) -> Vec<fleetdeck_proto::EventEnvelope> {
        match self.manager.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events,
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    async fn lifecycle(&self, session: &SessionId) -> Vec<String> {
        lifecycle(&self.events().await, session)
    }

    async fn record(&self, session: &SessionId) -> Option<GrantRecord> {
        self.manager
            .store()
            .session_grant(session.clone())
            .await
            .unwrap()
    }

    /// Waits until the binding of `binding_id` is `state` at `generation`.
    async fn wait_binding(
        &self,
        binding_id: &str,
        state: BindingState,
        generation: u32,
    ) -> Binding {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(b) = self.bindings.get(binding_id)
                && b.state == state
                && b.generation == generation
            {
                return b;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{binding_id}: {:?}",
                self.bindings.get(binding_id)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_events(
        &self,
        what: &str,
        done: impl Fn(&[fleetdeck_proto::EventEnvelope]) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let events = self.events().await;
            if done(&events) {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_state(&self, session: &SessionId, want: SessionState) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let state = self
                .manager
                .store()
                .summary(session.clone())
                .await
                .unwrap()
                .unwrap()
                .state;
            if state == want {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{state:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the first turn is done, then the run.
    async fn wait_idle(&self, session: &SessionId, turns: usize) {
        self.wait_events("completed runs", |events| {
            events
                .iter()
                .filter(|e| e.session_id == *session)
                .filter(|e| matches!(e.event, EventBody::Known(Event::RunCompleted { .. })))
                .count()
                >= turns
        })
        .await;
    }

    /// The process of `session` ends and the end is stored (the session is
    /// `Exited`, its effects settled and its binding refused).
    async fn settle_end(&self, session: &SessionId, binding_id: &str) {
        self.wait_state(session, SessionState::Exited).await;
        let deadline = tokio::time::Instant::now() + WAIT;
        while self
            .bindings
            .get(binding_id)
            .is_some_and(|b| b.state != BindingState::Revoked)
        {
            assert!(tokio::time::Instant::now() < deadline, "{binding_id}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The pump stores the end events right after it revokes the binding.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    /// A crash of the process (a kill), then the end.
    async fn kill(&mut self, session: &SessionId, binding_id: &str) {
        self.accepted(SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Kill,
        })
        .await;
        self.settle_end(session, binding_id).await;
    }

    /// A resume with a first input; the credential of the new process, which
    /// differs from `previous`.
    async fn resume(&mut self, session: &SessionId, previous: &str) -> String {
        self.accepted(SessionCommand::Resume {
            session_id: session.clone(),
        })
        .await;
        // The resumed process takes input once it is live (a Claude CLI
        // reports `session.started` only after its first input).
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let decided = self
                .send(SessionCommand::Prompt {
                    session_id: session.clone(),
                    input: UserInput::new("again".to_owned()),
                })
                .await;
            if decided.receipt.state == ReceiptState::Accepted {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{:?}",
                decided.receipt
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.token() == previous {
            assert!(tokio::time::Instant::now() < deadline, "no new launch");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.token()
    }

    /// The credential of the latest launch of the fake.
    fn token(&self) -> String {
        let log = std::fs::read_to_string(&self.launch_log).unwrap_or_default();
        log.split_once("env:")
            .map(|(_, token)| token.trim().to_owned())
            .unwrap_or_default()
    }

    async fn first_token(&self) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if std::fs::read_to_string(&self.launch_log).is_ok_and(|l| l.contains("env:fdc1_")) {
                return self.token();
            }
            assert!(tokio::time::Instant::now() < deadline, "no launch");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn ended(events: &[String]) -> Vec<&String> {
    events.iter().filter(|e| e.starts_with("ended:")).collect()
}

/// A grant with a record: generation 1 at the start, 2 at a hibernation
/// resume, 3 after a crash; the process end writes no `coordinator.ended`.
async fn direct_with_a_record() {
    let mut st = Direct::new(true, None).await;
    let far = i64::MAX / 4;
    let (s, seq) = st.start(grant(far)).await;
    assert_eq!(seq, Some(1), "the start receipt names the record's seq");
    let id = format!("{s}:{GRANT}");
    let first = st.first_token().await;
    st.wait_binding(&id, BindingState::Active, 1).await;
    let record = st.record(&s).await.unwrap();
    assert_eq!(
        (record.state, record.generation, record.seq),
        (RecordState::Attached, 1, 1)
    );
    assert_eq!(st.lifecycle(&s).await, [format!("bound:{id}@1")]);

    // Hibernation, then the first input of the owner resumes the session.
    st.wait_idle(&s, 1).await;
    // Events of the first turn can still move `updated_at`: advance and
    // sweep again until the session is idle for the interval.
    let (store, policy) = (st.manager.store().clone(), st.manager.policy().clone());
    let mut swept = 0;
    for _ in 0..50 {
        st.det
            .advance(Duration::from_millis(u64::from(30u32) * 60_000 + 1));
        swept = hibernate::sweep(&store, &policy, &st.bindings, 30)
            .await
            .unwrap();
        if swept > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(swept, 1);
    st.settle_end(&s, &id).await;
    assert!(
        ended(&st.lifecycle(&s).await).is_empty(),
        "no ended at the process end"
    );
    assert_eq!(st.record(&s).await.unwrap().state, RecordState::Attached);
    let second = st.resume(&s, &first).await;
    assert_ne!(first, second, "a new credential");
    let b = st.wait_binding(&id, BindingState::Active, 2).await;
    assert!(st.bindings.has_digest(&digest(&second)));
    assert!(!st.bindings.has_digest(&digest(&first)));
    assert_eq!(b.grant_id, GRANT);
    assert_eq!(st.record(&s).await.unwrap().generation, 2);
    assert_eq!(
        st.lifecycle(&s).await,
        [format!("bound:{id}@1"), format!("bound:{id}@2")]
    );

    // A crash, then a resume (what an auto-continue does).
    st.wait_idle(&s, 2).await;
    st.kill(&s, &id).await;
    assert!(ended(&st.lifecycle(&s).await).is_empty());
    let third = st.resume(&s, &second).await;
    assert_ne!(second, third);
    st.wait_binding(&id, BindingState::Active, 3).await;
    assert_eq!(st.record(&s).await.unwrap().generation, 3);
    assert_eq!(
        st.lifecycle(&s).await,
        [
            format!("bound:{id}@1"),
            format!("bound:{id}@2"),
            format!("bound:{id}@3")
        ]
    );
    st.manager.shutdown(StopMode::Kill).await;
}

/// A resume whose provider reports another native id: the binding is
/// revoked, the record ends `identity_changed`, and the event says so.
async fn direct_identity_changed() {
    let mut st = Direct::new(true, Some(RESUME_OTHER_ID)).await;
    let (s, _) = st.start(grant(i64::MAX / 4)).await;
    let id = format!("{s}:{GRANT}");
    st.wait_binding(&id, BindingState::Active, 1).await;
    st.wait_idle(&s, 1).await;
    st.kill(&s, &id).await;
    let first = st.token();
    st.resume(&s, &first).await;
    st.wait_binding(&id, BindingState::Revoked, 2).await;
    st.wait_events("identity_changed", |events| {
        lifecycle(events, &s)
            .iter()
            .any(|e| e == &format!("ended:{id}@2:identity_changed"))
    })
    .await;
    let record = st.record(&s).await.unwrap();
    assert_eq!(record.state, RecordState::Ended);
    assert_eq!(record.ended_reason.as_deref(), Some("identity_changed"));
    assert_eq!(
        st.lifecycle(&s).await,
        [
            format!("bound:{id}@1"),
            format!("ended:{id}@2:identity_changed")
        ]
    );
    st.manager.shutdown(StopMode::Kill).await;
}

/// A record past its expiry ends `expired`; the resume gets the fleet tools.
async fn direct_expired() {
    let mut st = Direct::new(true, None).await;
    let soon = i64::try_from(st.det.now_ms()).unwrap() + 60_000;
    let (s, _) = st.start(grant(soon)).await;
    let id = format!("{s}:{GRANT}");
    st.wait_binding(&id, BindingState::Active, 1).await;
    st.wait_idle(&s, 1).await;
    st.kill(&s, &id).await;
    st.det.advance(Duration::from_secs(120));
    let first = st.token();
    st.resume(&s, &first).await;
    st.wait_binding(&format!("{s}:fleet"), BindingState::Active, 1)
        .await;
    let record = st.record(&s).await.unwrap();
    assert_eq!(record.state, RecordState::Ended);
    assert_eq!(record.ended_reason.as_deref(), Some("expired"));
    assert_eq!(
        st.lifecycle(&s).await,
        [format!("bound:{id}@1"), format!("ended:{id}@1:expired")]
    );
    assert_eq!(st.bindings.get(&id).unwrap().state, BindingState::Revoked);
    st.manager.shutdown(StopMode::Kill).await;
}

/// A start without `coordinator_live.v1` writes no record and behaves as
/// today: the process end gives `coordinator.ended process_exit`, and the
/// resume gets the fleet tools.
async fn direct_without_the_feature() {
    let mut st = Direct::new(false, None).await;
    let (s, seq) = st.start(grant(i64::MAX / 4)).await;
    assert_eq!(seq, None);
    let id = format!("{s}:{GRANT}");
    st.wait_binding(&id, BindingState::Active, 1).await;
    assert!(st.record(&s).await.is_none());
    st.wait_idle(&s, 1).await;
    st.kill(&s, &id).await;
    assert_eq!(
        st.lifecycle(&s).await,
        [
            format!("bound:{id}@1"),
            format!("ended:{id}@1:process_exit")
        ]
    );
    let first = st.token();
    st.resume(&s, &first).await;
    st.wait_binding(&format!("{s}:fleet"), BindingState::Active, 1)
        .await;
    assert!(st.record(&s).await.is_none());
    st.manager.shutdown(StopMode::Kill).await;
}

#[path = "coordinator_epochs_shim_tests.rs"]
mod shim;

#[tokio::test]
async fn a_grant_with_a_record_returns_at_every_process_start_with_a_higher_generation() {
    shim::plan_rules().await;
    shim::shim_with_a_record().await;
    shim::shim_expired_and_without_the_feature().await;
    direct_with_a_record().await;
    direct_identity_changed().await;
    direct_expired().await;
    direct_without_the_feature().await;
}
