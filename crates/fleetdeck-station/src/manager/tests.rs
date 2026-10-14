//! The session manager against `fake-claude sim` (M2 P2-6): every command
//! goes decide → commit → effect → adapter, and the station policy decides
//! in the command's transaction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    ApprovalDecision, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec,
    StopMode, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, SessionState, TurnStatus};
use fleetdeck_proto::{
    CommandEnvelope, CommandId, Event, EventBody, EventEnvelope, RejectCode, RequestId, SessionId,
};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

pub(super) use super::coordinator_spec_tests::coordinator_spec;
use super::*;
use crate::store::{EffectState, OfflineApprovalPolicy, ReadResult, StoreConfig};

const WAIT: Duration = Duration::from_secs(10);

struct Station {
    det: Det,
    manager: Arc<SessionManager>,
    root: tempfile::TempDir,
    next: u32,
}

fn adapter(program: PathBuf) -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config = ClaudeAdapterConfig::new("claude".into(), program, env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
}

fn codex_adapter() -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
    config.background_poll = Some(Duration::from_millis(50));
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    config.rollback_timeout = Duration::from_millis(500);
    Arc::new(CodexAdapter::new(config, Arc::new(DirectChildHost::new())))
}

impl Station {
    async fn new(max_sessions: u32) -> Self {
        Self::with_program(max_sessions, FakeAgent::Claude.locate().unwrap()).await
    }

    async fn with_program(max_sessions: u32, program: PathBuf) -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
        std::fs::create_dir_all(root.path().join("elsewhere")).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![root.path().join("projects")],
            max_sessions,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([
                ("claude".into(), ProviderKind::Claude),
                ("codex".into(), ProviderKind::Codex),
            ]),
        );
        let adapters = Adapters::from([
            ("claude".into(), adapter(program)),
            ("codex".into(), codex_adapter()),
        ]);
        let manager = SessionManager::start(store, policy, adapters, det.ids())
            .await
            .unwrap();
        Self {
            det,
            manager: Arc::new(manager),
            root,
            next: 0,
        }
    }

    fn workspace(&self) -> String {
        self.root
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned()
    }

    async fn send(&mut self, command: SessionCommand) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        };
        self.manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    async fn accepted(&mut self, command: SessionCommand) -> Decided {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    async fn rejected(&mut self, command: SessionCommand) -> RejectCode {
        let d = self.send(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        d.receipt.reject.unwrap().code
    }

    async fn start(&mut self, cwd: &str, prompt: Option<&str>) -> SessionId {
        self.start_with(spec(cwd, "claude"), prompt).await
    }

    async fn start_with(&mut self, spec: SessionSpec, prompt: Option<&str>) -> SessionId {
        let d = self
            .accepted(SessionCommand::Start {
                spec,
                prompt: prompt.map(text),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
            })
            .await;
        d.receipt.result.unwrap().session_id.unwrap()
    }

    async fn events(&self) -> Vec<EventEnvelope> {
        match self.manager.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events,
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    /// Waits for the first event after `after` that `pick` accepts.
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
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {what}: {:#?}",
                kinds(&self.events().await)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn effect_states(&self, session: &SessionId) -> Vec<(&'static str, EffectState)> {
        self.manager
            .store()
            .session_effects(session.clone())
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.payload.kind(), e.state))
            .collect()
    }

    /// Waits until every effect of `session` is settled.
    async fn settled(&self, session: &SessionId) -> Vec<(&'static str, EffectState)> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let states = self.effect_states(session).await;
            if states
                .iter()
                .all(|(_, s)| !matches!(s, EffectState::Queued | EffectState::Claimed))
            {
                return states;
            }
            assert!(tokio::time::Instant::now() < deadline, "{states:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn state(&self, session: &SessionId) -> SessionState {
        self.manager
            .store()
            .summary(session.clone())
            .await
            .unwrap()
            .unwrap()
            .state
    }
}

fn spec(cwd: &str, instance: &str) -> SessionSpec {
    SessionSpec {
        provider_instance: instance.into(),
        cwd: cwd.to_owned(),
        mode: Some("default".to_owned()),
        model: None,
        title: None,
        coordinator: None,
    }
}

pub(super) fn text(t: &str) -> UserInput {
    UserInput::new(t.to_owned())
}

fn kinds(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.event {
            EventBody::Known(k) => k.kind_name().to_owned(),
            EventBody::Unknown { kind, .. } => kind.clone(),
        })
        .collect()
}

fn approval(event: &Event) -> Option<RequestId> {
    match event {
        Event::ApprovalRequested { request_id, .. } => Some(request_id.clone()),
        _ => None,
    }
}

fn answer(session: &SessionId, request: &RequestId, option: &str) -> SessionCommand {
    SessionCommand::Answer {
        session_id: session.clone(),
        request_id: request.clone(),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: option.to_owned(),
            updated_input: None,
            text: None,
        },
    }
}

#[tokio::test]
async fn a_session_runs_asks_for_approval_is_answered_interrupted_and_stopped() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("ask ls")).await;
    let request = st.wait_for("approval", approval).await;

    st.accepted(answer(&s, &request, "allow")).await;
    st.wait_for("tool completed", |e| {
        matches!(e, Event::ToolCompleted { .. }).then_some(())
    })
    .await;
    st.wait_for("turn done", |e| {
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

    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait for the host"),
    })
    .await;
    st.wait_for("streaming", |e| {
        matches!(e, Event::MessageDelta { .. }).then_some(())
    })
    .await;
    let interrupt = st
        .accepted(SessionCommand::Interrupt {
            session_id: s.clone(),
        })
        .await;
    let effect_id = interrupt.effects[0].effect_id.as_str().to_owned();
    st.wait_for("interrupt result", |e| match e {
        Event::InterruptResult {
            control_id: Some(id),
            accepted: true,
            ..
        } if *id == effect_id => Some(()),
        _ => None,
    })
    .await;
    st.wait_for("interrupted turn", |e| {
        matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        )
        .then_some(())
    })
    .await;

    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let states = st.settled(&s).await;
    assert_eq!(
        states,
        vec![
            ("start", EffectState::Confirmed),
            ("answer", EffectState::Confirmed),
            ("prompt", EffectState::Confirmed),
            ("interrupt", EffectState::Confirmed),
            ("stop", EffectState::Confirmed),
        ]
    );
    assert_eq!(st.state(&s).await, SessionState::Exited);
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.manager.live_sessions() > 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The process is gone: a command now is refused, not queued.
    assert_eq!(
        st.rejected(SessionCommand::Prompt {
            session_id: s,
            input: text("late"),
        })
        .await,
        RejectCode::InvalidState
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_second_answer_gets_the_first_outcome_and_sends_nothing() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("ask pwd")).await;
    let request = st.wait_for("approval", approval).await;

    assert_eq!(
        st.rejected(answer(&s, &request, "maybe")).await,
        RejectCode::InvalidCommand
    );
    let first = st.accepted(answer(&s, &request, "deny")).await;
    assert_eq!(first.effects.len(), 1);
    let second = st.accepted(answer(&s, &request, "allow")).await;
    assert!(second.effects.is_empty(), "L3: {:?}", second.effects);
    st.wait_for("denied turn", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    let resolved: Vec<_> = st
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::ApprovalResolved { option_id, .. }) => Some(option_id),
            _ => None,
        })
        .collect();
    assert_eq!(resolved, vec!["deny".to_owned()]);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_station_policy_decides_in_the_command_transaction() {
    let mut st = Station::new(1).await;
    let outside = st.root.path().join("elsewhere");
    let missing = st.root.path().join("projects/missing");
    let ws = st.workspace();
    let cases = [
        (spec(&ws, "nope"), RejectCode::UnknownProvider),
        (
            spec(&outside.to_string_lossy(), "claude"),
            RejectCode::Forbidden,
        ),
        (
            spec(&missing.to_string_lossy(), "claude"),
            RejectCode::InvalidCommand,
        ),
    ];
    for (spec, code) in cases {
        assert_eq!(
            st.rejected(SessionCommand::Start {
                spec,
                prompt: None,
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
            })
            .await,
            code
        );
    }
    // A symlink inside a root that points outside it is outside (trap 8).
    let link = st.root.path().join("projects/link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert_eq!(
        st.rejected(SessionCommand::Start {
            spec: spec(&link.to_string_lossy(), "claude"),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        })
        .await,
        RejectCode::Forbidden
    );

    let s = st.start(&ws, Some("hello")).await;
    assert_eq!(
        st.rejected(SessionCommand::Start {
            spec: spec(&ws, "claude"),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        })
        .await,
        RejectCode::Forbidden,
        "max_sessions = 1"
    );
    st.wait_for("echo", |e| match e {
        Event::MessageCompleted { text, .. } if text.contains("echo: hello") => Some(()),
        _ => None,
    })
    .await;
    // The session's own projection decides too.
    assert_eq!(
        st.rejected(SessionCommand::Interrupt {
            session_id: s.clone(),
        })
        .await,
        RejectCode::InvalidState,
        "no turn runs"
    );
    assert_eq!(
        st.rejected(SessionCommand::SetMode {
            session_id: s.clone(),
            mode: "yolo".into(),
        })
        .await,
        RejectCode::InvalidCommand
    );
    // A running session is not resumed (DL-118); fork is not built.
    assert_eq!(
        st.rejected(SessionCommand::Resume {
            session_id: s.clone()
        })
        .await,
        RejectCode::InvalidState
    );
    assert_eq!(
        st.rejected(SessionCommand::Fork {
            session_id: s.clone()
        })
        .await,
        RejectCode::Unsupported
    );
    assert_eq!(
        st.rejected(SessionCommand::Prompt {
            session_id: SessionId::from("no-such-session"),
            input: text("x"),
        })
        .await,
        RejectCode::UnknownSession
    );
    st.accepted(SessionCommand::SetMode {
        session_id: s.clone(),
        mode: "plan".into(),
    })
    .await;
    st.wait_for("mode changed", |e| match e {
        Event::ModeChanged { mode } if mode == "plan" => Some(()),
        _ => None,
    })
    .await;
    let states = st.settled(&s).await;
    assert!(
        states.contains(&("set_mode", EffectState::Confirmed)),
        "{states:?}"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_mode_change_before_the_first_prompt_is_confirmed_at_once() {
    // The Claude CLI (and the sim) sends `system/init` only after the first
    // user message; the answer to `set_permission_mode` must not wait for it
    // (DL-141).
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, None).await;
    // The command is rejected until the provider process runs.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let d = st
            .send(SessionCommand::SetMode {
                session_id: s.clone(),
                mode: "plan".into(),
            })
            .await;
        if d.receipt.state == ReceiptState::Accepted {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "set_mode never accepted: {:?}",
            d.receipt
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.wait_for("mode changed", |e| match e {
        Event::ModeChanged { mode } if mode == "plan" => Some(()),
        _ => None,
    })
    .await;
    let states = st.settled(&s).await;
    assert!(
        states.contains(&("set_mode", EffectState::Confirmed)),
        "{states:?}"
    );
    let summary = st
        .manager
        .store()
        .summary(s.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.mode.as_deref(), Some("plan"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_that_gets_no_process_ends_the_session() {
    let missing = Path::new("/nonexistent/fleetdeck/claude").to_path_buf();
    let mut st = Station::with_program(4, missing).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("start failure", |e| match e {
        Event::Error { code, .. } if code == "start_failed" => Some(()),
        _ => None,
    })
    .await;
    let states = st.settled(&s).await;
    assert_eq!(states, vec![("start", EffectState::RetiredNotDispatched)]);
    assert_eq!(st.state(&s).await, SessionState::Exited);
    // An ended session does not count against `max_sessions`.
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn shutdown_stops_every_live_process() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let a = st.start(&ws, Some("one")).await;
    let b = st.start(&ws, Some("two")).await;
    for s in [&a, &b] {
        let deadline = tokio::time::Instant::now() + WAIT;
        while st.state(s).await == SessionState::Starting {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(st.manager.live_sessions(), 2);
    let store = st.manager.store().clone();
    st.manager.shutdown(StopMode::Graceful).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    for s in [a, b] {
        loop {
            let state = store.summary(s.clone()).await.unwrap().unwrap().state;
            if state == SessionState::Exited {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "{state:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[test]
fn the_policy_and_the_adapters_come_from_station_toml() {
    let node = fleetdeck_testkit::TempNode::new("alpha").unwrap();
    let bins = FakeAgent::bin_dir().unwrap();
    let text = node.station_toml(&bins, &[FakeAgent::Claude, FakeAgent::Codex]);
    node.write_station_toml(&text).unwrap();
    let config = node.station_config().unwrap();
    let policy = Policy::from_config(&config);
    assert_eq!(policy.max_sessions(), 4);
    assert_eq!(policy.offline(), OfflineApprovalPolicy::Wait);
    let claude = FakeAgent::Claude.instance_id();
    let codex = FakeAgent::Codex.instance_id();
    assert_eq!(policy.instance(&claude.into()), Some(ProviderKind::Claude));
    assert_eq!(policy.instance(&codex.into()), Some(ProviderKind::Codex));
    let ws = node.workspace("app").unwrap();
    assert_eq!(
        policy.workspace(&ws.to_string_lossy()).unwrap(),
        ws.canonicalize().unwrap()
    );
    assert!(matches!(
        policy.workspace(&node.root().to_string_lossy()),
        Err(WorkspaceError::Outside(_))
    ));
    let host: Arc<dyn fleetdeck_providers::host::SessionHost> = Arc::new(DirectChildHost::new());
    let adapters = adapters_from_config(&config, &host, None).unwrap();
    let kinds: Vec<_> = adapters
        .iter()
        .map(|(id, a)| (id.as_str().to_owned(), a.kind()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (claude.to_owned(), ProviderKind::Claude),
            (codex.to_owned(), ProviderKind::Codex),
        ]
    );
}

#[tokio::test]
async fn station_info_providers_are_probed_once_with_versions() {
    let st = Station::new(3).await;
    let providers = st.manager.providers().await;
    let report: Vec<_> = providers
        .iter()
        .map(|p| {
            (
                p.instance.as_str().to_owned(),
                p.provider,
                p.version.clone(),
                p.auth.clone(),
            )
        })
        .collect();
    assert_eq!(
        report,
        vec![
            (
                "claude".to_owned(),
                ProviderKind::Claude,
                Some("2.1.999".to_owned()),
                // Scenario `sim` fails `auth status`: the state is unknown.
                "unknown".to_owned()
            ),
            // `account/read` of the Codex sim answers signed in.
            (
                "codex".to_owned(),
                ProviderKind::Codex,
                Some("0.160.999".to_owned()),
                "signed_in".to_owned()
            ),
        ]
    );
    // Each instance reports the capabilities of a new session under the
    // station's settings (P5-3): the spawn form offers their modes.
    let caps: Vec<_> = providers.iter().map(|p| p.capabilities.clone()).collect();
    assert_eq!(
        caps,
        vec![
            Some(fleetdeck_providers::claude::capabilities(false)),
            Some(fleetdeck_providers::codex::capabilities(false)),
        ]
    );
    assert_eq!(st.manager.providers().await, providers, "cached");
    assert_eq!(st.manager.policy().max_sessions(), 3);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_started_session_records_the_branch_of_its_workspace() {
    let missing = Path::new("/nonexistent/fleetdeck/claude").to_path_buf();
    let mut st = Station::with_program(4, missing).await;
    let ws = st.workspace();
    std::fs::create_dir_all(Path::new(&ws).join(".git")).unwrap();
    std::fs::write(
        Path::new(&ws).join(".git/HEAD"),
        "ref: refs/heads/fd/feature-1\n",
    )
    .unwrap();
    let s = st.start(&ws, None).await;
    let summary = st.manager.store().summary(s).await.unwrap().unwrap();
    assert_eq!(summary.branch.as_deref(), Some("fd/feature-1"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[path = "assist_wired_tests.rs"]
mod assist_wired;
#[path = "batch_tests.rs"]
mod batch;
#[path = "checkpoint_wired_tests.rs"]
mod checkpoint_wired;
#[path = "codex_tests.rs"]
mod codex;
#[path = "coordinator_command_tests.rs"]
mod coordinator_command;
#[path = "drain_tests.rs"]
mod drain;
#[path = "hibernate_tests.rs"]
mod hibernate_tests;
#[path = "hibernated_input_tests.rs"]
mod hibernated_input_tests;
#[path = "import_tests.rs"]
mod import_tests;
#[path = "origin_text_tests.rs"]
mod origin_text_tests;
#[path = "policy_hub_tests.rs"]
mod policy_hub_tests;
#[path = "provision_task_tests.rs"]
mod provision_task_tests;
#[path = "rename_tests.rs"]
mod rename;
#[path = "restart_gate_tests.rs"]
mod restart_gate;
#[path = "resume_tests.rs"]
mod resume;
#[path = "revert_wired_tests.rs"]
mod revert_wired;
#[path = "setup_tests.rs"]
mod setup;
#[path = "workspace_tests.rs"]
mod workspace_provision;

/// ADR-006 §8.2 test 13: the station-local socket (DL-038) never starts a
/// session with a coordinator grant, and the refusal starts nothing.
#[tokio::test]
async fn a_station_local_start_with_a_coordinator_spec_is_rejected() {
    let mut st = Station::new(1).await;
    let ws = st.workspace();
    let mut granted = spec(&ws, "claude");
    granted.coordinator = Some(Box::new(coordinator_spec()));
    assert_eq!(
        st.rejected(SessionCommand::Start {
            spec: granted,
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        })
        .await,
        RejectCode::InvalidCommand
    );
    // The refusal used no session slot: a plain start still fits.
    st.start(&ws, None).await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[path = "trash_wired_tests.rs"]
mod trash_wired;
