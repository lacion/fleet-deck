//! The durable Claude driver through a real shim and the fake Claude CLI
//! (ADR-005 §2.1, tests 36, 46, 47 for Claude; DL-112). Each test drives a
//! session with [`DurableClaude`], stops at a crash point ("station
//! crash": the pump stops and the attachment drops; the shim keeps the
//! provider), runs the recovery of a new "station start", restores the
//! driver from the committed checkpoint and continues. The fake CLI logs
//! every stdin frame, so a test proves which frames reached the provider
//! and how often.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    ApprovalDecision, CommandEnvelope, CommandOrigin, CommandResult, RETRY_WINDOW_MS,
    SessionCommand, UserInput,
};
use fleetdeck_proto::event::{InputState, ProviderKind, ResolvedBy, RunOrigin};
use fleetdeck_proto::{CommandId, Event, EventBody, InputId, RequestId};
use fleetdeck_providers::claude::{CoreConfig, capabilities};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimHostConfig};
use fleetdeck_station::manager::shim_claude::DurableClaude;
use fleetdeck_station::manager::shims::{Recovered, launch_shim, recover_shims};
use fleetdeck_station::store::{
    Caller, Dispatch, Effect, EffectPayload, InputRecord, NewSession, OfflineApprovalPolicy,
    ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};
use serde_json::Value;

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(10);
const IN_FLIGHT: Duration = Duration::from_secs(1);

struct Rig {
    tmp: tempfile::TempDir,
    store: Store,
    det: Det,
    fence: u64,
    commands: std::sync::atomic::AtomicU32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        std::fs::create_dir(tmp.path().join("w")).unwrap();
        store
            .create_session(NewSession {
                session_id: "s1".into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: tmp.path().join("w").display().to_string(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        store
            .process_started("s1".into(), "proc-1".to_owned().into(), None)
            .await
            .unwrap();
        let fence = store.register_worker().await.unwrap();
        Self {
            tmp,
            store,
            det,
            fence,
            commands: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn host(&self) -> ShimHost {
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            self.tmp.path().join("station"),
            self.store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = self.tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        ShimHost::new(config)
    }

    fn cwd(&self) -> String {
        self.tmp.path().join("w").display().to_string()
    }

    fn log(&self) -> PathBuf {
        self.tmp.path().join("claude-stdin.log")
    }

    fn config(&self) -> CoreConfig {
        CoreConfig {
            provider_instance: "claude".into(),
            cwd: self.cwd(),
            capabilities: capabilities(false),
            resumed: None,
        }
    }

    fn env(&self) -> ChildEnv {
        let mut env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, self.log()).unwrap();
        env
    }

    async fn open(&self) -> Arc<DurableClaude> {
        let spec = ProcessSpec {
            program: FakeAgent::Claude.locate().unwrap(),
            args: Vec::new(),
            cwd: self.tmp.path().join("w"),
            env: self.env(),
        };
        let attached = launch_shim(
            &self.store,
            &self.host(),
            &"s1".into(),
            &spec,
            AckMode::Manual,
        )
        .await
        .unwrap();
        match DurableClaude::open(&self.store, attached, self.config()).await {
            Ok(d) => d,
            Err(e) => panic!(
                "open: {e}; log {:?}; events {:?}",
                std::fs::read_to_string(self.log()),
                self.events().await
            ),
        }
    }

    /// A station crash, then the recovery and restore of a new start.
    async fn crash_and_restore(&self, driver: Arc<DurableClaude>) -> Arc<DurableClaude> {
        driver.abandon().await;
        drop(driver);
        let attached = self.reattach().await;
        DurableClaude::restore(&self.store, attached, self.config(), IN_FLIGHT)
            .await
            .unwrap()
    }

    async fn reattach(&self) -> Attached {
        let mut recovered = recover_shims(&self.store, &self.host(), AckMode::Manual, IN_FLIGHT)
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        match recovered.remove(0).1 {
            Recovered::Reattached(attached) => attached,
            other => panic!("not reattached: {}", other.name()),
        }
    }

    /// A claimed command effect with `payload`.
    async fn claimed(&self, payload: EffectPayload) -> Effect {
        let n = self
            .commands
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("cmd-{n}").as_str()),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::Interrupt {
                session_id: "s1".into(),
            },
        };
        self.store
            .decide(envelope, Caller::local(), move |d| {
                d.enqueue(&"s1".into(), payload)?;
                Ok(CommandResult {
                    session_id: Some("s1".into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
        self.store.claim_next(self.fence).await.unwrap().unwrap()
    }

    async fn events(&self) -> Vec<Event> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 10_000).await.unwrap()
        else {
            panic!("snapshot required")
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                EventBody::Unknown { .. } => None,
            })
            .collect()
    }

    async fn wait_event(&self, what: &str, pick: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(e) = self.events().await.into_iter().find(|e| pick(e)) {
                return e;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The JSON frames that the fake app-server read from stdin.
    fn frames(&self) -> Vec<Value> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The user messages that the fake CLI read.
    fn user_messages(&self) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|f| f["type"] == "user")
            .collect()
    }

    /// The control requests of `subtype` that the fake CLI read.
    fn control_count(&self, subtype: &str) -> usize {
        self.frames()
            .iter()
            .filter(|f| f["type"] == "control_request" && f["request"]["subtype"] == subtype)
            .count()
    }

    async fn wait_frames(&self, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let frames = self.frames();
            if done(&frames) {
                return frames;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {what}: {frames:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

const IN_1: &str = "01J00000000000000000000001";
const IN_2: &str = "01J00000000000000000000002";

fn input(id: &str, text: &str) -> InputRecord {
    InputRecord {
        input_id: InputId::from(id),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
    }
}

fn id_number(frame: &Value) -> u64 {
    frame["request_id"]
        .as_str()
        .and_then(|s| s.strip_prefix("fd-"))
        .and_then(|n| n.parse().ok())
        .unwrap()
}

fn turn_started(e: &Event) -> bool {
    matches!(e, Event::TurnStarted { .. })
}

fn is_text(frame: &Value, text: &str) -> bool {
    frame["message"]["content"] == text || frame["message"]["content"][0]["text"] == text
}

#[tokio::test]
async fn a_crash_after_an_outbound_dispatch_before_any_response_recovers_the_claude_input_and_never_reuses_a_control_id()
 {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let init = rig
        .frames()
        .into_iter()
        .find(|f| f["request"]["subtype"] == "initialize")
        .unwrap();
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input(IN_1, "wait here"),
        })
        .await;
    // The CLI's answer stays at the shim: the pump stops before it.
    let hold = driver.hold_output().await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    rig.wait_frames("user message", |f| f.iter().any(|f| f["type"] == "user"))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    driver.abandon().await;
    drop(hold);
    assert!(
        !rig.events().await.iter().any(turn_started),
        "the answer was not committed before the crash"
    );
    let driver = rig.crash_and_restore(driver).await;
    // The replayed output finds the restored input.
    rig.wait_event("turn.started", turn_started).await;
    let interrupt = rig.claimed(EffectPayload::Interrupt).await;
    assert_eq!(driver.apply(&interrupt).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("interrupt", |f| {
            f.iter().any(|f| f["request"]["subtype"] == "interrupt")
        })
        .await;
    let sent = frames
        .iter()
        .find(|f| f["request"]["subtype"] == "interrupt")
        .unwrap();
    assert!(id_number(sent) > id_number(&init), "{sent} after {init}");
    rig.wait_event("turn.completed", |e| {
        matches!(e, Event::TurnCompleted { .. })
    })
    .await;
    assert_eq!(rig.control_count("initialize"), 1);
    assert_eq!(rig.user_messages().len(), 1, "nothing was resent");
    assert_eq!(
        rig.events()
            .await
            .iter()
            .filter(|e| turn_started(e))
            .count(),
        1,
        "the replayed output committed one turn.started"
    );
}

#[tokio::test]
async fn a_busy_follow_up_and_a_mode_or_model_change_survive_a_claude_restart() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input(IN_1, "wait here"),
        })
        .await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    rig.wait_event("message delta", |e| matches!(e, Event::MessageDelta { .. }))
        .await;
    // The CLI's answers to these commands stay at the shim until the
    // crash: the restored state must already hold the commands.
    let hold = driver.hold_output().await;
    for payload in [
        EffectPayload::FollowUp {
            input: input(IN_2, "second"),
        },
        EffectPayload::SetMode {
            mode: "acceptEdits".to_owned(),
        },
    ] {
        let effect = rig.claimed(payload).await;
        assert_eq!(driver.apply(&effect).await, Dispatch::Sent);
    }
    rig.wait_frames("set_permission_mode", |f| {
        f.iter()
            .any(|f| f["request"]["subtype"] == "set_permission_mode")
    })
    .await;
    driver.abandon().await;
    drop(hold);
    let driver = rig.crash_and_restore(driver).await;
    // The first turn still waits for the interrupt, so the follow-up
    // cannot have started: the restored core must hold it as pending.
    assert_eq!(
        driver.with_core(|c| c.pending_inputs()).await,
        1,
        "the follow-up is pending in the restored core"
    );
    assert_ne!(
        driver.with_core(|c| c.input_state(&IN_2.into())).await,
        Some(InputState::Started)
    );
    rig.wait_event(
        "mode change",
        |e| matches!(e, Event::ModeChanged { mode, .. } if mode.as_str() == "acceptEdits"),
    )
    .await;
    // The first turn ends; the queued follow-up runs; nothing is resent.
    let interrupt = rig.claimed(EffectPayload::Interrupt).await;
    assert_eq!(driver.apply(&interrupt).await, Dispatch::Sent);
    rig.wait_event(
        "second turn",
        |e| matches!(e, Event::MessageCompleted { text, .. } if text.contains("echo: second")),
    )
    .await;
    let users = rig.user_messages();
    assert_eq!(users.len(), 2, "{users:?}");
    assert!(is_text(&users[1], "second"), "{users:?}");
    assert_eq!(rig.control_count("set_permission_mode"), 1);
    assert_eq!(rig.control_count("initialize"), 1);
}

#[tokio::test]
async fn a_reattached_claude_session_answers_an_approval_opened_before_the_crash_with_the_restored_checkpoint()
 {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input(IN_1, "ask touch x"),
        })
        .await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    let Event::ApprovalRequested { request_id, .. } = rig
        .wait_event("approval", |e| matches!(e, Event::ApprovalRequested { .. }))
        .await
    else {
        unreachable!()
    };
    let driver = rig.crash_and_restore(driver).await;
    let answer = rig
        .claimed(EffectPayload::Answer {
            request_id: RequestId(request_id.0.clone()),
            decision: ApprovalDecision {
                option_id: "allow".to_owned(),
                updated_input: None,
                text: None,
            },
            by: ResolvedBy::Board,
        })
        .await;
    assert_eq!(driver.apply(&answer).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("permission answer", |f| {
            f.iter().any(|f| f["type"] == "control_response")
        })
        .await;
    let reply = frames
        .iter()
        .find(|f| f["type"] == "control_response")
        .unwrap();
    assert!(
        reply["response"]["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("sim-req-")),
        "the native request id: {reply}"
    );
    rig.wait_event("turn.completed", |e| {
        matches!(e, Event::TurnCompleted { .. })
    })
    .await;
    assert_eq!(rig.control_count("initialize"), 1);
    assert_eq!(rig.user_messages().len(), 1);
}
