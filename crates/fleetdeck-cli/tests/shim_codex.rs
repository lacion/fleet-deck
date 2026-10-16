//! The durable Codex driver through a real shim and the fake Codex
//! app-server (ADR-005 §2.1, tests 36, 46, 47; DL-110). Each test drives a
//! session with [`DurableCodex`], stops at a crash point ("station crash":
//! the pump stops and the attachment drops; the shim keeps the provider),
//! runs the recovery of a new "station start", restores the driver from
//! the committed checkpoint and continues. The fake app-server logs every
//! stdin frame, so a test proves which frames reached the provider and
//! how often.

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
use fleetdeck_proto::event::{ProviderKind, ResolvedBy, RunOrigin};
use fleetdeck_proto::{CommandId, Event, EventBody, InputId, RequestId};
use fleetdeck_providers::codex::{ClientInfo, CoreConfig, SessionSpec, capabilities};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimHostConfig};
use fleetdeck_station::manager::shim_codex::DurableCodex;
use fleetdeck_station::manager::shim_codex_poll::spawn_background_poll;
use fleetdeck_station::manager::shims::{Recovered, launch_shim, recover_shims};
use fleetdeck_station::store::shell_runs::ShellState;
use fleetdeck_station::store::{
    Caller, Dispatch, Effect, EffectPayload, EffectState, FrameState, InputRecord, NewSession,
    OfflineApprovalPolicy, OutputCommit, OutputMeta, ReadResult, Store, StoreConfig,
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
                provider: ProviderKind::Codex,
                provider_instance: "codex".into(),
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
        self.tmp.path().join("codex-stdin.log")
    }

    fn config(&self) -> CoreConfig {
        CoreConfig {
            provider_instance: "codex".into(),
            cwd: self.cwd(),
            capabilities: capabilities(false),
            resumed: None,
            client: ClientInfo::fleetdeck(),
            request_models: false,
        }
    }

    fn env(&self) -> ChildEnv {
        let mut env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, self.log()).unwrap();
        env
    }

    async fn open(&self) -> Arc<DurableCodex> {
        let spec = ProcessSpec {
            program: FakeAgent::Codex.locate().unwrap(),
            args: vec!["app-server".into()],
            cwd: self.tmp.path().join("w"),
            env: self.env(),
            launch: None,
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
        let thread = SessionSpec {
            cwd: Some(self.cwd()),
            ..SessionSpec::default()
        };
        match DurableCodex::open(&self.store, attached, self.config(), &thread).await {
            Ok(d) => d,
            Err(e) => panic!(
                "open: {e}; log {:?}; events {:?}",
                std::fs::read_to_string(self.log()),
                self.events().await
            ),
        }
    }

    /// A station crash, then the recovery and restore of a new start.
    async fn crash_and_restore(&self, driver: Arc<DurableCodex>) -> Arc<DurableCodex> {
        driver.abandon().await;
        drop(driver);
        let attached = self.reattach().await;
        DurableCodex::restore(&self.store, attached, self.config(), IN_FLIGHT)
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
            authority: None,
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

    /// A claimed `!` shell effect with its `running` row (B8).
    async fn claimed_shell(&self, command: &str) -> Effect {
        let n = self
            .commands
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{n}").as_str()),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::Interrupt {
                session_id: "s1".into(),
            },
        };
        let (command, root) = (command.to_owned(), self.cwd());
        self.store
            .decide(envelope, Caller::local(), move |d| {
                let payload = EffectPayload::ShellCommand {
                    command: command.clone(),
                    provider: ProviderKind::Codex,
                    worktree_root: root.clone(),
                };
                let id = d
                    .enqueue(&"s1".into(), payload)?
                    .effect_id
                    .as_str()
                    .to_owned();
                d.shell_insert(&id, &"s1".into(), ProviderKind::Codex, &root, &command)?;
                Ok(CommandResult {
                    session_id: Some("s1".into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
        let effect = self.store.claim_next(self.fence).await.unwrap().unwrap();
        assert!(
            self.store
                .shell_set_running(effect.effect_id.as_str().to_owned())
                .await
                .unwrap()
        );
        effect
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

    fn method_count(&self, method: &str) -> usize {
        self.frames()
            .iter()
            .filter(|f| f["method"] == method)
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

fn input(id: &str, text: &str) -> InputRecord {
    InputRecord {
        input_id: InputId::from(id),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
        shell_context: Vec::new(),
    }
}

fn id_number(frame: &Value) -> u64 {
    frame["id"]
        .as_str()
        .and_then(|s| s.strip_prefix("fd-"))
        .and_then(|n| n.parse().ok())
        .unwrap()
}

fn turn_started(e: &Event) -> bool {
    matches!(e, Event::TurnStarted { .. })
}

#[tokio::test]
async fn a_crash_after_an_outbound_dispatch_before_any_response_recovers_the_correlation_and_never_reuses_an_rpc_id()
 {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input("in-1", "wait here"),
            authority: None,
        })
        .await;
    // The response stays at the shim: the pump stops before it.
    let hold = driver.hold_output().await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("turn/start", |f| {
            f.iter().any(|f| f["method"] == "turn/start")
        })
        .await;
    let start = frames
        .iter()
        .find(|f| f["method"] == "turn/start")
        .unwrap()
        .clone();
    tokio::time::sleep(Duration::from_millis(200)).await;
    driver.abandon().await;
    drop(hold);
    assert!(
        !rig.events().await.iter().any(turn_started),
        "the response was not committed before the crash"
    );
    let driver = rig.crash_and_restore(driver).await;
    // The replayed response finds its correlation.
    rig.wait_event("turn.started", turn_started).await;
    let interrupt = rig
        .claimed(EffectPayload::Interrupt { authority: None })
        .await;
    assert_eq!(driver.apply(&interrupt).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("turn/interrupt", |f| {
            f.iter().any(|f| f["method"] == "turn/interrupt")
        })
        .await;
    let sent = frames
        .iter()
        .find(|f| f["method"] == "turn/interrupt")
        .unwrap();
    assert!(id_number(sent) > id_number(&start), "{sent} after {start}");
    assert_eq!(rig.method_count("initialize"), 1);
    assert_eq!(rig.method_count("thread/start"), 1);
    assert_eq!(rig.method_count("turn/start"), 1, "nothing was resent");
    assert_eq!(
        rig.events()
            .await
            .iter()
            .filter(|e| turn_started(e))
            .count(),
        1,
        "the replayed response committed one turn.started"
    );
}

#[tokio::test]
async fn a_busy_follow_up_and_a_mode_or_model_change_without_later_stdout_survive_a_restart() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input("in-1", "wait here"),
            authority: None,
        })
        .await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    rig.wait_event("message delta", |e| matches!(e, Event::MessageDelta { .. }))
        .await;
    // Three transitions without any provider output.
    for payload in [
        EffectPayload::FollowUp {
            input: input("in-2", "second"),
            authority: None,
        },
        EffectPayload::SetMode {
            mode: "on-request/workspace-write".to_owned(),
            authority: None,
        },
        EffectPayload::SetModel {
            model: "gpt-next".to_owned(),
        },
    ] {
        let effect = rig.claimed(payload).await;
        assert_eq!(driver.apply(&effect).await, Dispatch::Sent);
    }
    let driver = rig.crash_and_restore(driver).await;
    assert_eq!(driver.with_core(|c| c.pending_inputs()).await, 1);
    assert_eq!(rig.method_count("turn/start"), 1, "nothing was resent");
    // The queued follow-up goes out with the new mode and model when the
    // running turn ends.
    let interrupt = rig
        .claimed(EffectPayload::Interrupt { authority: None })
        .await;
    assert_eq!(driver.apply(&interrupt).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("second turn/start", |f| {
            f.iter().filter(|f| f["method"] == "turn/start").count() == 2
        })
        .await;
    let second = frames
        .iter()
        .filter(|f| f["method"] == "turn/start")
        .nth(1)
        .unwrap();
    assert_eq!(second["params"]["clientUserMessageId"], "in-2");
    assert_eq!(second["params"]["approvalPolicy"], "on-request");
    assert_eq!(second["params"]["model"], "gpt-next");
    assert_eq!(rig.method_count("initialize"), 1);
    assert_eq!(rig.method_count("thread/start"), 1);
}

#[tokio::test]
async fn a_reattached_codex_session_answers_an_approval_opened_before_the_crash_with_the_restored_checkpoint()
 {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input("in-1", "ask touch x"),
            authority: None,
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
                answered_by: None,
                option_id: "accept".to_owned(),
                updated_input: None,
                text: None,
            },
            by: ResolvedBy::Board,
            authority: None,
        })
        .await;
    assert_eq!(driver.apply(&answer).await, Dispatch::Sent);
    let frames = rig
        .wait_frames("approval reply", |f| {
            f.iter().any(|f| f["result"]["decision"] == "accept")
        })
        .await;
    let reply = frames
        .iter()
        .find(|f| f["result"]["decision"] == "accept")
        .unwrap();
    assert!(reply["id"].is_number(), "the native request id: {reply}");
    rig.wait_event("turn.completed", |e| {
        matches!(e, Event::TurnCompleted { .. })
    })
    .await;
    assert_eq!(rig.method_count("initialize"), 1);
    assert_eq!(rig.method_count("thread/start"), 1);
    assert_eq!(rig.method_count("thread/resume"), 0);
}

/// DL-111: an old owner that claimed a generated frame and was delayed
/// past a takeover cannot move the frame after the new owner settled and
/// wrote it; the frame reaches the provider once.
#[tokio::test]
async fn a_delayed_old_owner_cannot_make_the_new_owner_resend_a_completed_frame() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.store.shims_to_recover().await.unwrap()[0]
        .shim_id
        .clone();
    let old_generation = driver.process().generation();
    // The old owner's pump stops; it commits a generated frame and claims
    // it, then is delayed before the frame leaves.
    let hold = driver.hold_output().await;
    let cursor = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .cursor;
    let meta = OutputMeta {
        frames: vec![r#"{"jsonrpc":"2.0","method":"fd/marker"}"#.to_owned()],
        ..OutputMeta::default()
    };
    assert!(matches!(
        rig.store
            .shim_commit_output_with(shim_id.clone(), old_generation, cursor, Vec::new(), meta)
            .await
            .unwrap(),
        OutputCommit::Committed(_)
    ));
    let frame = rig.store.shim_open_frames(shim_id.clone()).await.unwrap()[0].clone();
    let old_tag = rig
        .store
        .shim_claim_frame(shim_id.clone(), old_generation, frame.frame_seq)
        .await
        .unwrap()
        .unwrap();
    // A takeover: the new owner proves the old claim unwritten and writes
    // the frame with its own claim.
    let driver = {
        driver.abandon().await;
        drop(hold);
        rig.crash_and_restore(driver).await
    };
    assert!(driver.process().generation() > old_generation);
    rig.wait_frames("the marker", |f| {
        f.iter().any(|f| f["method"] == "fd/marker")
    })
    .await;
    assert!(
        rig.store
            .shim_open_frames(shim_id.clone())
            .await
            .unwrap()
            .is_empty()
    );
    // The delayed old owner reports its stale claim: every change is refused.
    for state in [FrameState::Pending, FrameState::Done] {
        assert!(
            !rig.store
                .shim_frame_state(
                    shim_id.clone(),
                    old_generation,
                    frame.frame_seq,
                    old_tag.clone(),
                    state,
                )
                .await
                .unwrap()
        );
    }
    // The new owner flushes again after new output: the frame stays done.
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input("in-1", "hello"),
            authority: None,
        })
        .await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    rig.wait_event("turn.completed", |e| {
        matches!(e, Event::TurnCompleted { .. })
    })
    .await;
    assert!(
        rig.store
            .shim_open_frames(shim_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        rig.method_count("fd/marker"),
        1,
        "the frame was never resent"
    );
    assert_eq!(rig.method_count("initialize"), 1);
}

#[tokio::test]
async fn a_shell_command_of_a_shim_hosted_codex_session_settles_done() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let effect = rig.claimed_shell("pwd").await;
    let id = effect.effect_id.as_str().to_owned();
    assert_eq!(driver.apply(&effect).await, Dispatch::Sent);
    rig.wait_event("the shell end", |e| {
        matches!(e, Event::ToolCompleted { node_id, .. } if node_id.as_str() == format!("shell-{id}"))
    })
    .await;
    let row = rig.store.shell_run(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShellState::Done);
    assert_eq!(row.exit_code, Some(0));
    assert_eq!(
        rig.frames()
            .iter()
            .find(|f| f["method"] == "thread/shellCommand")
            .map(|f| f["params"]["command"].clone()),
        Some(Value::from("pwd"))
    );
    let effect = rig.store.effect(effect.effect_id).await.unwrap().unwrap();
    assert_eq!(effect.state, EffectState::Confirmed);
}

const LIST: &str = "thread/backgroundTerminals/list";

fn background_tasks(e: &Event) -> Option<usize> {
    match e {
        Event::BackgroundUpdated { tasks } => Some(tasks.len()),
        _ => None,
    }
}

/// DL-024, DL-199: the shim host polls the background terminals, so a
/// background command leaves the session when it ends.
#[tokio::test]
async fn a_background_command_of_a_shim_hosted_codex_session_is_polled_until_it_ends() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    spawn_background_poll(&driver, Duration::from_millis(200));
    let prompt = rig
        .claimed(EffectPayload::Prompt {
            input: input("in-1", "bg make build"),
            authority: None,
        })
        .await;
    assert_eq!(driver.apply(&prompt).await, Dispatch::Sent);
    // One update with one task, then a later update with no task.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let counts: Vec<usize> = rig
            .events()
            .await
            .iter()
            .filter_map(background_tasks)
            .collect();
        if let Some(first) = counts.iter().position(|n| *n == 1)
            && counts[first..].contains(&0)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no end of the background command: {counts:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(rig.method_count(LIST) >= 2, "{:?}", rig.frames());
}

/// DL-024, DL-199: a restored driver lists the background work at once.
#[tokio::test]
async fn a_restored_shim_hosted_codex_session_lists_its_background_work_at_once() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    // A tick far away: only the list at the start of the poll runs.
    spawn_background_poll(&driver, Duration::from_secs(3600));
    rig.wait_frames("the first list", |f| f.iter().any(|f| f["method"] == LIST))
        .await;
    let before = rig.method_count(LIST);
    let driver = rig.crash_and_restore(driver).await;
    spawn_background_poll(&driver, Duration::from_secs(3600));
    rig.wait_frames("the list after the restore", |f| {
        f.iter().filter(|f| f["method"] == LIST).count() == before + 1
    })
    .await;
    assert_eq!(rig.method_count("initialize"), 1);
    assert_eq!(rig.method_count("thread/start"), 1);
}
