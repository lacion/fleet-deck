//! The start order of the shim bindings with a real shim (DL-194 T4b, §9.7,
//! §9.8): the bindings of a reattached shim answer "starting" until its
//! driver committed the buffered output, and the bindings of a shim that is
//! gone end at the start. A real shim drives the fake Codex app-server; the
//! "station crash" stops the driver's pump and drops the attachment while the
//! shim keeps the provider.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, GrantScope, RETRY_WINDOW_MS, SessionCommand,
    UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::{CommandId, Event, EventBody, InputId};
use fleetdeck_providers::codex::{ClientInfo, CoreConfig, SessionSpec, capabilities};
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{ProcessControl as _, ProcessSpec};
use fleetdeck_station::coordinator::fence::Fence;
use fleetdeck_station::coordinator::{Binding, BindingState, Bindings, ToolClass, mint};
use fleetdeck_station::manager::binding_start::{finish, prepare};
use fleetdeck_station::manager::shim_codex::DurableCodex;
use fleetdeck_station::manager::shim_station::{Configs, recover_station};
use fleetdeck_station::manager::shims::launch_shim;
use fleetdeck_station::store::shim_bindings::BindingRow;
use fleetdeck_station::store::{
    Caller, Dispatch, Effect, EffectPayload, InputRecord, NewSession, OfflineApprovalPolicy,
    ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV};

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(10);
const IN_FLIGHT: Duration = Duration::from_secs(1);

struct Rig {
    tmp: tempfile::TempDir,
    store: Store,
    det: Det,
    fence: u64,
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

fn binding() -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "s1".into(),
        provider_instance: "codex".to_owned(),
        native_id: Some("n1".to_owned()),
        generation: 1,
        classes: [ToolClass::Read, ToolClass::Own].into_iter().collect(),
        scope: GrantScope::Lineage,
        state: BindingState::Active,
        control_skill: String::new(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
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

    fn run_dir(&self) -> PathBuf {
        self.host().config().station_dir.join("run")
    }

    fn configs(&self) -> Configs {
        Configs {
            claude: Box::new(|_| panic!("no Claude session in this rig")),
            codex: Box::new(|summary| Some(codex_config(summary.cwd.clone().unwrap_or_default()))),
        }
    }

    async fn open(&self) -> std::sync::Arc<DurableCodex> {
        let mut env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
        env.set(SCENARIO_ENV, "sim").unwrap();
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
            None,
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
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from("cmd-1"),
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
                    seq: None,
                })
            })
            .await
            .unwrap();
        self.store.claim_next(self.fence).await.unwrap().unwrap()
    }

    /// Stores an active binding row of `shim_id`; returns the token and the
    /// digest. The fence file is created with the header only.
    async fn bind(&self, shim_id: &str) -> (String, String) {
        let (token, digest) = mint().unwrap();
        self.store
            .shim_binding_insert(BindingRow::from_binding(
                digest.clone(),
                shim_id.to_owned(),
                &binding(),
                Some(i64::MAX),
                1,
            ))
            .await
            .unwrap();
        Fence::new(self.run_dir()).ensure().unwrap();
        (token.into_exposed(), digest)
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
async fn the_bindings_of_a_reattached_shim_answer_starting_until_its_buffered_output_is_committed()
{
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.shim_id().await;
    let (_token, digest) = rig.bind(&shim_id).await;
    // The pump stops between two lines: the provider's answer to the prompt
    // stays buffered and unacknowledged at the shim.
    let guard = driver.hold_output().await;
    let effect = rig.claimed(prompt("in-1", "hello")).await;
    assert_eq!(driver.apply(&effect).await, Dispatch::Sent);
    let deadline = tokio::time::Instant::now() + WAIT;
    while driver.process().delivered() <= driver.process().acked() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no output was buffered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    driver.abandon().await;
    drop(guard);
    drop(driver);

    let bindings = Bindings::default();
    let held = prepare(&rig.store, &bindings, rig.run_dir()).await.unwrap();
    assert_eq!(held.len(), 1);
    assert!(bindings.is_held(&digest));
    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert!(bindings.is_held(&digest), "nothing releases before finish");
    assert_eq!(start.restored.len(), 1, "{:?}", start.ended);
    finish(&rig.store, &bindings, &start.restored, held)
        .await
        .unwrap();

    // The hold ends only when the driver committed up to the status end.
    let deadline = tokio::time::Instant::now() + WAIT;
    while bindings.is_held(&digest) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hold was not released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        bindings.get_digest(&digest).unwrap().state,
        BindingState::Active
    );
    let end = start.restored[0]
        .driver
        .process()
        .status_end()
        .expect("the release waited for a status frame");
    let cursor = rig.store.shim(shim_id).await.unwrap().unwrap().cursor;
    assert!(cursor >= end, "released at {cursor}, buffered up to {end}");
    start.restored[0].driver.abandon().await;
}

#[tokio::test]
async fn the_bindings_of_a_shim_that_ended_while_the_station_was_down_end_at_the_start() {
    let rig = Rig::new().await;
    let driver = rig.open().await;
    let shim_id = rig.shim_id().await;
    let (_token, digest) = rig.bind(&shim_id).await;
    driver.abandon().await;
    drop(driver);
    let pid = shim_reaper::shim_pid(&shim_id).unwrap();
    shim_reaper::kill9(pid);
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline, "the shim lives");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let bindings = Bindings::default();
    let held = prepare(&rig.store, &bindings, rig.run_dir()).await.unwrap();
    assert!(bindings.is_held(&digest));
    let start = recover_station(&rig.store, &rig.host(), &rig.configs(), IN_FLIGHT)
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    finish(&rig.store, &bindings, &start.restored, held)
        .await
        .unwrap();
    assert!(!bindings.is_held(&digest));
    assert_eq!(
        bindings.get_digest(&digest).unwrap().state,
        BindingState::Revoked
    );
    assert!(rig.store.shim_bindings_load().await.unwrap().is_empty());
    let ended: Vec<_> = rig
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::CoordinatorEnded { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(ended, ["process_exit"]);
}

/// The replay boundary of a real shim (DL-194 §9.7): the buffer ends with a
/// partial line, `status` reports the raw end, and the last complete line
/// ends before it. The release logic compares the committed offset with the
/// line boundary (unit-tested in `durable_release.rs` with fake values: a
/// fake Codex app-server cannot write a partial line).
#[tokio::test]
async fn the_line_boundary_of_a_real_shim_stops_before_a_partial_line_at_the_buffer_end() {
    let rig = Rig::new().await;
    let spec = ProcessSpec {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), "printf 'line1\\nline2\\npar'; sleep 30".into()],
        cwd: rig.tmp.path().join("w"),
        env: ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]),
        launch: None,
    };
    let attached = launch_shim(
        &rig.store,
        &rig.host(),
        &"s1".into(),
        &spec,
        AckMode::Manual,
    )
    .await
    .unwrap();
    let process = attached.process.clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    while process.delivered() < 15 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the output was not delivered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(process.request_status().await);
    while process.status_end().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no status frame arrived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(process.status_end(), Some(15), "the raw end of the buffer");
    assert_eq!(process.line_end(), 12, "after `line2\\n`, before `par`");
    process.kill().await;
}
