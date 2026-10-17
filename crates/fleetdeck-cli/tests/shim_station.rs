//! Station start with shim sessions (ADR-005 §3.2, §4.3, test 25; DL-112):
//! `recover_station` reattaches each running shim, settles its claimed
//! stdin writes from the shim's records, restores its durable driver, and
//! only then runs the generic store recovery with the restored processes
//! as the live ones. A real shim drives the fake Codex app-server; each
//! "station crash" stops the driver's pump and drops the attachment while
//! the shim keeps the provider.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin, SessionState};
use fleetdeck_proto::{CommandId, Event, EventBody, InputId};
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, REASON_UNSUPPORTED};
use fleetdeck_providers::codex::{ClientInfo, CoreConfig, SessionSpec, capabilities};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::journal::{Record, ShimFiles};
use fleetdeck_providers::host::shim::start::Prepared;
use fleetdeck_station::manager::decide::decide;
use fleetdeck_station::manager::policy::Policy;
use fleetdeck_station::manager::shim_codex::DurableCodex;
use fleetdeck_station::manager::shim_station::{Configs, Durable, recover_station};
use fleetdeck_station::manager::shims::launch_shim;
use fleetdeck_station::store::{
    Caller, Dispatch, Effect, EffectPayload, EffectState, InputRecord, NewSession, NewShim,
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

fn cwd_of(tmp: &tempfile::TempDir) -> String {
    tmp.path().join("w").display().to_string()
}

fn codex_config(cwd: String) -> CoreConfig {
    CoreConfig {
        provider_instance: "codex".into(),
        cwd,
        capabilities: capabilities(false),
        resumed: None,
        client: ClientInfo::fleetdeck(),
        request_models: false,
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
                cwd: cwd_of(&tmp),
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

    fn log(&self) -> PathBuf {
        self.tmp.path().join("codex-stdin.log")
    }

    fn configs(&self) -> Configs {
        Configs {
            claude: Box::new(|_| panic!("no Claude session in this rig")),
            codex: Box::new(|summary| Some(codex_config(summary.cwd.clone().unwrap_or_default()))),
        }
    }

    async fn open(&self) -> Arc<DurableCodex> {
        let mut env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, self.log()).unwrap();
        let spec = ProcessSpec {
            program: FakeAgent::Codex.locate().unwrap(),
            args: vec!["app-server".into()],
            cwd: self.tmp.path().join("w"),
            env,
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
            cwd: Some(cwd_of(&self.tmp)),
            ..SessionSpec::default()
        };
        DurableCodex::open(
            &self.store,
            attached,
            codex_config(cwd_of(&self.tmp)),
            &thread,
        )
        .await
        .unwrap()
    }

    async fn shim_id(&self) -> String {
        let rows = self.store.shims_to_recover().await.unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].shim_id.clone()
    }

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

    async fn completed_turns(&self) -> usize {
        self.events()
            .await
            .iter()
            .filter(|e| matches!(e, Event::TurnCompleted { .. }))
            .count()
    }

    async fn wait_turns(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.completed_turns().await < n {
            assert!(tokio::time::Instant::now() < deadline, "no turn {n}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn effect_state(&self, effect: &Effect) -> (EffectState, Option<String>) {
        let e = self
            .store
            .effect(effect.effect_id.clone())
            .await
            .unwrap()
            .unwrap();
        (e.state, e.reason)
    }

    async fn session_state(&self) -> SessionState {
        self.store
            .summary("s1".into())
            .await
            .unwrap()
            .unwrap()
            .state
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

fn prompt(id: &str, text: &str) -> EffectPayload {
    EffectPayload::Prompt {
        input: InputRecord {
            input_id: InputId::from(id),
            origin: RunOrigin::Board,
            input: UserInput::new(text.to_owned()),
            shell_context: Vec::new(),
        },
        authority: None,
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

#[tokio::test]
async fn uncertain_inputs_are_settled_before_commands_are_admitted_after_a_station_start() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let first = rig.claimed(prompt("in-1", "hello")).await;
    assert_eq!(driver.apply(&first).await, Dispatch::Sent);
    rig.wait_turns(1).await;
    // The station claims the identity of a stdin frame and crashes before
    // the frame leaves (ADR-005 test 26 at the station).
    let lost = rig.claimed(prompt("in-2", "never sent")).await;
    let generation = driver.process().generation();
    rig.store
        .shim_claim_write(
            rig.shim_id().await,
            generation,
            Some(lost.effect_id.clone()),
            0,
            1,
        )
        .await
        .unwrap()
        .unwrap();
    driver.abandon().await;
    drop(driver);

    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    assert!(start.ended.is_empty(), "{:?}", start.ended);
    assert_eq!(start.restored[0].process.as_str(), "proc-1");
    assert_eq!(start.report.lost_sessions, 0);
    assert_eq!(start.report.dead_processes, 0);
    // The shim proved the frame unwritten before the generic recovery,
    // which would have made the claim `outcome_unknown`.
    assert_eq!(
        rig.effect_state(&lost).await,
        (
            EffectState::RetiredNotDispatched,
            Some("shim_not_received".to_owned())
        )
    );
    assert_ne!(rig.session_state().await, SessionState::Lost);
    // The restored session takes the next command without a handshake.
    let Durable::Codex(driver) = start.restored[0].driver.clone() else {
        panic!("not a Codex driver")
    };
    let next = rig.claimed(prompt("in-3", "again")).await;
    assert_eq!(driver.apply(&next).await, Dispatch::Sent);
    rig.wait_turns(2).await;
    assert_eq!(rig.method_count("initialize"), 1);
    assert_eq!(rig.method_count("thread/start"), 1);
    assert_eq!(
        rig.method_count("turn/start"),
        2,
        "the lost input never went out"
    );
    start.restored[0].driver.abandon().await;
}

#[tokio::test]
async fn a_dead_shim_leaves_its_session_lost_and_its_claims_outcome_unknown_after_a_station_start()
{
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.shim_id().await;
    // A command without any frame claim: no shim record can prove it.
    let claimed = rig.claimed(prompt("in-1", "hello")).await;
    driver.abandon().await;
    drop(driver);
    let pid = shim_reaper::shim_pid(&shim_id).unwrap();
    shim_reaper::kill9(pid);
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline, "the shim lives");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    assert_eq!(start.ended.len(), 1, "{:?}", start.ended);
    assert_eq!(start.report.dead_processes, 1);
    assert_eq!(start.report.lost_sessions, 1);
    assert_eq!(rig.session_state().await, SessionState::Lost);
    assert_eq!(
        rig.effect_state(&claimed).await.0,
        EffectState::OutcomeUnknown
    );
    assert!(rig.store.shims_to_recover().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unreadable_checkpoint_at_a_station_start_loses_the_session_and_leaves_the_shim_alone() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.shim_id().await;
    let generation = driver.process().generation();
    driver.abandon().await;
    drop(driver);
    // A later build committed a checkpoint that this build cannot read.
    let row = rig.store.shim(shim_id.clone()).await.unwrap().unwrap();
    let newer = AdapterCheckpoint {
        provider: ProviderKind::Codex,
        version: 99,
        state: serde_json::json!({}),
    };
    let commit = rig
        .store
        .shim_commit_output_with(
            shim_id.clone(),
            generation,
            row.cursor,
            Vec::new(),
            OutputMeta {
                checkpoint: Some(newer),
                ..OutputMeta::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(commit, OutputCommit::Committed(_)));

    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    assert_eq!(start.ended.len(), 1);
    assert!(
        start.ended[0].1.contains(REASON_UNSUPPORTED),
        "{:?}",
        start.ended
    );
    assert_eq!(rig.session_state().await, SessionState::Lost);
    // The station never kills a shim that it cannot drive, and it never
    // starts a fresh adapter for it.
    let pid = shim_reaper::shim_pid(&shim_id).unwrap();
    assert!(alive(pid));
    assert_eq!(rig.method_count("initialize"), 1);

    // DL-119: the original provider still runs under its shim, so a resume
    // is refused with a stored receipt that spawns nothing, although the
    // provider reported `resume` and its native thread id.
    let provider = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .provider_identity
        .unwrap()
        .pid;
    assert!(alive(provider), "the original provider survives");
    let summary = rig.store.summary("s1".into()).await.unwrap().unwrap();
    assert!(summary.capabilities.as_ref().unwrap().resume);
    let before = rig.store.session_effects("s1".into()).await.unwrap().len();
    let policy = Policy::new(
        vec![rig.tmp.path().to_owned()],
        4,
        OfflineApprovalPolicy::Wait,
        std::collections::BTreeMap::from([("codex".into(), ProviderKind::Codex)]),
    );
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("resume-1"),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Resume {
            session_id: "s1".into(),
        },
    };
    let decided = rig
        .store
        .decide(envelope.clone(), Caller::local(), {
            let policy = policy.clone();
            move |d| {
                decide(
                    d,
                    &policy,
                    &fleetdeck_station::coordinator::Bindings::default(),
                    CommandOrigin::Board,
                    false,
                    envelope.command.clone(),
                )
            }
        })
        .await
        .unwrap();
    let reject = decided.receipt.reject.clone().expect("a stored rejection");
    assert!(
        reject.message.contains("not_resumable") && reject.message.contains(REASON_UNSUPPORTED),
        "{reject:?}"
    );
    assert_eq!(
        rig.store.session_effects("s1".into()).await.unwrap().len(),
        before,
        "nothing queued"
    );
    assert_eq!(rig.session_state().await, SessionState::Lost);
    assert_eq!(rig.method_count("thread/resume"), 0);
    assert!(alive(provider));
}

// ADR-005 tests 43 and 49 at the station (DL-113): a station crash during a
// launch. The rig's session runs a launch whose claimed start effect is in
// the store; the shim row is `starting`.

fn start_effect() -> EffectPayload {
    EffectPayload::Start {
        spec: fleetdeck_proto::command::SessionSpec {
            provider_instance: "codex".into(),
            cwd: "/w".to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: None,
        workspace: None,
        setup_cmd: None,
        authority: None,
        fleet_git: None,
    }
}

fn codex_spec(rig: &Rig) -> ProcessSpec {
    let mut env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
    env.set(SCENARIO_ENV, "sim").unwrap();
    env.set(STDIN_LOG_ENV, rig.log()).unwrap();
    ProcessSpec {
        program: FakeAgent::Codex.locate().unwrap(),
        args: vec!["app-server".into()],
        cwd: rig.tmp.path().join("w"),
        env,
        launch: None,
    }
}

/// A `starting` row whose shim was prepared (journal `created`, key file)
/// and never started.
async fn starting_row(rig: &Rig) -> (String, ShimFiles, Prepared) {
    let host = rig.host();
    let prepared = host.prepare(&codex_spec(rig)).unwrap();
    let id = prepared.record.shim_id.clone();
    rig.store
        .shim_starting(NewShim {
            shim_id: id.clone(),
            session_id: "s1".into(),
            socket_path: prepared.record.socket_path.clone(),
            launch_boot_id: None,
            native_target: None,
        })
        .await
        .unwrap();
    (
        id.clone(),
        ShimFiles::new(&host.config().shims_dir(), &id),
        prepared,
    )
}

async fn lost_detail(rig: &Rig) -> String {
    rig.events()
        .await
        .into_iter()
        .rev()
        .find_map(|e| match e {
            Event::SessionStateChanged {
                state: SessionState::Lost,
                detail,
            } => detail,
            _ => None,
        })
        .expect("no lost state")
}

#[tokio::test]
async fn a_launch_interrupted_after_the_provider_spawn_and_before_the_identity_commit_is_never_retired_as_not_dispatched()
 {
    for damage in ["spawn_attempted", "missing", "truncated"] {
        let rig = Rig::new().await;
        let start = rig.claimed(start_effect()).await;
        let (id, files, _prepared) = starting_row(&rig).await;
        match damage {
            "spawn_attempted" => files.append(&Record::SpawnAttempted).unwrap(),
            "missing" => std::fs::remove_file(files.journal()).unwrap(),
            _ => {
                let text = std::fs::read(files.journal()).unwrap();
                std::fs::write(files.journal(), &text[..text.len() / 2]).unwrap();
            }
        }
        // A reboot model: the volatile runtime files are gone.
        let socket = rig.tmp.path().join("fd-shims");
        if socket.exists() {
            std::fs::remove_dir_all(&socket).unwrap();
        }

        let result = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
            .await
            .unwrap();
        assert!(result.restored.is_empty(), "{damage}");
        assert_eq!(
            result.ended,
            vec![(id.clone(), "execution_unknown".to_owned())],
            "{damage}"
        );
        assert_eq!(
            rig.effect_state(&start).await.0,
            EffectState::OutcomeUnknown,
            "{damage}: never retired as not dispatched"
        );
        assert_eq!(rig.session_state().await, SessionState::Lost, "{damage}");
        assert!(
            lost_detail(&rig).await.contains("execution_unknown"),
            "{damage}"
        );
    }
}

#[tokio::test]
async fn an_intact_journal_without_spawn_attempted_retires_the_start_effect_as_shim_never_started()
{
    let rig = Rig::new().await;
    let start = rig.claimed(start_effect()).await;
    let (id, files, _prepared) = starting_row(&rig).await;
    let result = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert_eq!(result.ended, vec![(id, "shim_never_started".to_owned())]);
    assert!(files.is_fenced(), "a late shim start exits");
    assert_eq!(
        rig.effect_state(&start).await,
        (
            EffectState::RetiredNotDispatched,
            Some("shim_never_started".to_owned())
        )
    );
    assert_eq!(rig.method_count("initialize"), 0, "no provider ran");
}

#[tokio::test]
async fn a_station_crash_during_a_launch_is_fenced_or_resolved_by_the_launch_lock_before_classification()
 {
    let rig = Rig::new().await;
    let start = rig.claimed(start_effect()).await;
    let (id, _files, prepared) = starting_row(&rig).await;
    // The shim runs and holds its launch lock; the station crashed before it
    // committed the provider identity and before any adapter state.
    let record = rig.host().start(prepared).await.unwrap();
    let pid = record.provider_identity.as_ref().unwrap().pid;

    let result = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    // Resolved through the held lock: the shim was attached and its
    // identity committed; without adapter state it cannot be restored, so
    // the row ends `lost` and the shim is left alone.
    let row = rig.store.shim(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.provider_identity.as_ref().map(|i| i.pid), Some(pid));
    assert!(result.restored.is_empty());
    assert_eq!(result.ended.len(), 1, "{:?}", result.ended);
    assert!(alive(pid), "recovery never kills a shim's provider");
    assert_eq!(
        rig.effect_state(&start).await.0,
        EffectState::OutcomeUnknown,
        "a launch with a live shim is never retired"
    );
    assert_eq!(rig.session_state().await, SessionState::Lost);
}

// ADR-005 §3.2 step 3, §4.5 (review sa-9c1141b8 MAJOR 3; DL-122): a valid
// exit report of a shim that ended while the station was down ends the
// session `exited`, with the unacknowledged output as an explicit gap.
#[tokio::test]
async fn a_persisted_exit_report_ends_the_session_exited_with_a_gap_for_unacknowledged_output() {
    use fleetdeck_proto::event::OutputGapReason;
    use fleetdeck_providers::host::shim::wire::ExitState;

    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.shim_id().await;
    let claimed = rig.claimed(prompt("in-1", "hello")).await;
    driver.abandon().await;
    drop(driver);
    let committed = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .cursor;
    let pid = shim_reaper::shim_pid(&shim_id).unwrap();
    shim_reaper::kill9(pid);
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline, "the shim lives");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The exit file that the shim published before it ended: 100 bytes
    // were never acknowledged by a station.
    let exit = ExitState {
        termination: "signal:9".to_owned(),
        cause: "killed".to_owned(),
        stderr_tail: String::new(),
        descendants_remaining: Some(0),
        inspection: 1,
        final_offset: committed + 100,
    };
    ShimFiles::new(&rig.host().config().shims_dir(), &shim_id)
        .write_exit(&exit)
        .unwrap();

    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    assert_eq!(start.ended, vec![(shim_id.clone(), "exited".to_owned())]);
    assert_eq!(start.report.lost_sessions, 0, "a known exit is not a loss");
    assert_eq!(rig.session_state().await, SessionState::Exited);
    let events = rig.events().await;
    let gap = events.iter().position(|e| {
        matches!(e, Event::OutputGap { from, to, reason: OutputGapReason::BelowFloor }
            if *from == committed && *to == committed + 100)
    });
    let ended = events.iter().position(|e| {
        matches!(
            e,
            Event::SessionEnded {
                exit_code: None,
                ..
            }
        )
    });
    assert!(gap.is_some() && ended.is_some(), "{events:?}");
    assert!(gap < ended, "the gap is committed before the end");
    assert!(
        !events.iter().any(|e| matches!(
            e,
            Event::SessionStateChanged {
                state: SessionState::Lost,
                ..
            }
        )),
        "{events:?}"
    );
    let row = rig.store.shim(shim_id).await.unwrap().unwrap();
    assert_eq!(row.cursor, committed + 100, "the final offset");
    assert_eq!(row.reason.as_deref(), Some("exited"));
    assert_eq!(
        rig.effect_state(&claimed).await.0,
        EffectState::OutcomeUnknown,
        "an unproven claim stays unknown"
    );
    assert!(rig.store.live_process("s1".into()).await.unwrap().is_none());
}
