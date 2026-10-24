//! Output gaps at the station through the durable driver and a real shim
//! (ADR-005 §4.2 and test 38; DL-113). A plain line core turns each
//! provider line into one event, so the tests see exactly which bytes the
//! station parsed.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand,
};
use fleetdeck_proto::event::{OutputGapReason, ProviderKind};
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_providers::adapter::ProviderError;
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, CheckpointError};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_station::manager::durable::{
    DurableCore, DurableDriver, REASON_OUTPUT_FRAME_TOO_LARGE, Steps,
};
use fleetdeck_station::manager::shim_station::{Configs, recover_station};
use fleetdeck_station::manager::shims::launch_shim;
use fleetdeck_station::store::shim_gaps::REASON_ADAPTER_STATE_GAP;
use fleetdeck_station::store::{Caller, EffectState};
use fleetdeck_station::store::{
    Effect, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(15);
const BUFFER: usize = 64 * 1024;

/// One `error { code: "line" }` event per provider line, and
/// `session.ended` at the exit.
#[derive(Debug, Default)]
struct Lines {
    seen: u64,
}

impl DurableCore for Lines {
    type Config = ();

    fn create((): ()) -> Self {
        Self::default()
    }

    fn save(&self) -> Result<AdapterCheckpoint, CheckpointError> {
        Ok(AdapterCheckpoint {
            provider: ProviderKind::Claude,
            version: 1,
            state: serde_json::json!({ "seen": self.seen }),
        })
    }

    fn load((): (), checkpoint: &AdapterCheckpoint) -> Result<Self, CheckpointError> {
        Ok(Self {
            seen: checkpoint.state["seen"].as_u64().unwrap_or_default(),
        })
    }

    /// No state outside the checkpoint.
    fn keep_runtime_of(&mut self, _old: &mut Self) {}

    fn line(&mut self, text: &str) -> Steps {
        self.seen += 1;
        let event = Event::Error {
            code: "line".to_owned(),
            message: text.to_owned(),
            retry_at: None,
        };
        Steps {
            events: vec![event.into()],
            ..Steps::default()
        }
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        let event = Event::SessionEnded {
            reason: "exited".to_owned(),
            exit_code: code,
        };
        Steps {
            events: vec![event.into()],
            ..Steps::default()
        }
    }

    /// Every command is one stdin line `cmd`.
    fn command(&mut self, _effect: &Effect) -> Result<Steps, ProviderError> {
        Ok(Steps {
            frames: vec!["cmd".to_owned()],
            ..Steps::default()
        })
    }

    fn ended(&self) -> bool {
        false
    }
}

struct Rig {
    tmp: tempfile::TempDir,
    store: Store,
    _det: Det,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        store
            .create_session(NewSession {
                session_id: "s1".into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        Self {
            tmp,
            store,
            _det: det,
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
        config.buffer_bytes = BUFFER;
        ShimHost::new(config)
    }

    async fn drive(&self, script: &str) -> (Arc<DurableDriver<Lines>>, String, u32) {
        let spec = ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: self.tmp.path().to_owned(),
            env: ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]),
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
        let shim_id = attached.record.shim_id.clone();
        let pid = attached.record.provider_identity.as_ref().unwrap().pid;
        (
            DurableDriver::<Lines>::attach(&self.store, attached, ()),
            shim_id,
            pid,
        )
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

    async fn wait_ended(&self) -> Vec<Event> {
        let deadline = Instant::now() + WAIT;
        loop {
            let events = self.events().await;
            if events
                .iter()
                .any(|e| matches!(e, Event::SessionEnded { .. }))
            {
                return events;
            }
            assert!(Instant::now() < deadline, "no session.ended: {events:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn cursor(&self, shim_id: &str) -> u64 {
        self.store
            .shim(shim_id.to_owned())
            .await
            .unwrap()
            .unwrap()
            .cursor
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        !s.rsplit(')')
            .next()
            .unwrap_or_default()
            .trim_start()
            .starts_with('Z')
    })
}

fn lines(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Error { code, message, .. } if code == "line" => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn gaps(events: &[Event]) -> Vec<(u64, u64, OutputGapReason)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OutputGap { from, to, reason } => Some((*from, *to, *reason)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_line_of_the_buffer_size_without_a_newline_fails_the_session_with_an_explicit_gap_and_stops_the_provider()
 {
    let rig = Rig::new().await;
    // 200,000 bytes without a newline after the first line, then a newline
    // and a line that must never be parsed as the provider's.
    let (_driver, shim_id, pid) = rig
        .drive("echo first; head -c 200000 /dev/zero | tr '\\0' x; echo; echo after; sleep 30")
        .await;
    let events = rig.wait_ended().await;
    assert_eq!(lines(&events), vec!["first".to_owned()], "{events:?}");
    let found = gaps(&events);
    assert_eq!(found.len(), 1, "{events:?}");
    let (from, to, reason) = found[0];
    assert_eq!(reason, OutputGapReason::OutputFrameTooLarge);
    assert_eq!(from, 6, "the gap starts after `first\\n`");
    assert!(to - from >= BUFFER as u64, "{found:?}");
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Error { code, .. } if code == REASON_OUTPUT_FRAME_TOO_LARGE
        )),
        "{events:?}"
    );
    // The gap and the failure are committed before the exit.
    let gap_at = events
        .iter()
        .position(|e| matches!(e, Event::OutputGap { .. }))
        .unwrap();
    let ended_at = events
        .iter()
        .position(|e| matches!(e, Event::SessionEnded { .. }))
        .unwrap();
    assert!(gap_at < ended_at);
    // The shim stopped the provider; the cursor passed the gap.
    let deadline = Instant::now() + WAIT;
    while alive(pid) {
        assert!(Instant::now() < deadline, "the provider still runs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(rig.cursor(&shim_id).await >= to);
}

#[tokio::test]
async fn an_unterminated_last_line_at_eof_is_committed_as_malformed_before_the_exit_is_acknowledged()
 {
    // ADR-005 test 39 (formerly
    // `an_unterminated_last_line_at_the_exit_is_an_explicit_gap_with_the_final_cursor`).
    let rig = Rig::new().await;
    let (_driver, shim_id, _pid) = rig.drive("echo first; printf partial").await;
    let events = rig.wait_ended().await;
    assert_eq!(lines(&events), vec!["first".to_owned()], "{events:?}");
    assert_eq!(
        gaps(&events),
        vec![(6, 13, OutputGapReason::UnterminatedLine)],
        "{events:?}"
    );
    // The malformed tail is committed before the exit events.
    let gap_at = events
        .iter()
        .position(|e| matches!(e, Event::OutputGap { .. }))
        .unwrap();
    let ended_at = events
        .iter()
        .position(|e| matches!(e, Event::SessionEnded { .. }))
        .unwrap();
    assert!(gap_at < ended_at, "{events:?}");
    assert_eq!(rig.cursor(&shim_id).await, 13);
}

// ADR-005 test 4: the bytes after the last newline of a running provider
// are never committed and never acknowledged.
#[tokio::test]
async fn a_partial_line_is_never_acknowledged() {
    let rig = Rig::new().await;
    let (driver, shim_id, pid) = rig.drive("echo one; printf part; sleep 30").await;
    let deadline = Instant::now() + WAIT;
    while lines(&rig.events().await).is_empty() {
        assert!(Instant::now() < deadline, "no line committed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give the pump time to read `part`.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid));
    assert_eq!(lines(&rig.events().await), vec!["one".to_owned()]);
    assert!(gaps(&rig.events().await).is_empty());
    assert_eq!(rig.cursor(&shim_id).await, 4, "only `one\\n` is committed");
    assert_eq!(driver.process().acked(), 4, "only `one\\n` is acknowledged");
}

#[tokio::test]
async fn complete_lines_have_no_gap() {
    let rig = Rig::new().await;
    let (_driver, shim_id, _pid) = rig.drive("echo one; echo two").await;
    let events = rig.wait_ended().await;
    assert_eq!(lines(&events), vec!["one".to_owned(), "two".to_owned()]);
    assert!(gaps(&events).is_empty(), "{events:?}");
    assert_eq!(rig.cursor(&shim_id).await, 8);
}

// ADR-005 §4.2 (review sa-9c1141b8 MAJOR 2; DL-122): an owner that
// acknowledged output the station never committed leaves the shim without
// the bytes above the committed cursor. The next owner commits the
// `below_floor` gap at once, while the provider still runs: the adapter
// checkpoint is invalidated, claimed effects become `outcome_unknown`,
// input is blocked, no later line is parsed, and a later station start
// restores no driver from that state.
#[tokio::test]
async fn a_below_floor_gap_invalidates_the_adapter_state_at_once_while_the_provider_runs() {
    let rig = Rig::new().await;
    rig.store
        .process_started("s1".into(), "proc-1".to_owned().into(), None)
        .await
        .unwrap();
    let (driver, shim_id, pid) = rig
        .drive("echo one; sleep 1; echo two; echo three; sleep 30")
        .await;
    let deadline = Instant::now() + WAIT;
    while rig.cursor(&shim_id).await < 4 {
        assert!(Instant::now() < deadline, "`one` was never committed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    driver.abandon().await;
    drop(driver);
    // A claimed command whose outcome only the lost state could prove.
    let fence = rig.store.register_worker().await.unwrap();
    let envelope = CommandEnvelope {
        authority: None,
        command_id: "cmd-1".into(),
        target_incarnation: None,
        retry_until: i64::try_from(rig._det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    };
    rig.store
        .decide(envelope, Caller::local(), |d| {
            d.enqueue(
                &"s1".into(),
                fleetdeck_station::store::EffectPayload::Interrupt { authority: None },
            )?;
            Ok(CommandResult {
                session_id: Some("s1".into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    let claimed = rig.store.claim_next(fence).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // A rogue owner acknowledges `two` and `three` and commits nothing.
    let record = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .record();
    let generation = rig
        .store
        .shim_next_generation(shim_id.clone())
        .await
        .unwrap()
        .unwrap();
    let rogue = rig
        .host()
        .attach(&record, generation, AckMode::Manual, false)
        .await
        .unwrap();
    assert!(rogue.process.ack(14).await);
    drop(rogue);

    let record = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .record();
    assert_eq!(record.cursor, 4);
    let generation = rig
        .store
        .shim_next_generation(shim_id.clone())
        .await
        .unwrap()
        .unwrap();
    let attached = rig
        .host()
        .attach(&record, generation, AckMode::Manual, false)
        .await
        .unwrap();
    assert_eq!(attached.floor, 14);
    let started = Instant::now();
    let driver = DurableDriver::<Lines>::restore(&rig.store, attached, (), Duration::from_secs(1))
        .await
        .unwrap();
    assert!(driver.has_adapter_gap());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "committed without waiting for an EOF"
    );
    let events = rig.events().await;
    assert_eq!(
        gaps(&events),
        vec![(4, 14, OutputGapReason::BelowFloor)],
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Error { code, .. } if code == REASON_ADAPTER_STATE_GAP)),
        "{events:?}"
    );
    assert_eq!(
        lines(&events),
        vec!["one".to_owned()],
        "nothing after the gap is parsed"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::SessionEnded { .. }))
    );
    assert!(alive(pid), "the provider still runs");
    assert_eq!(
        rig.store
            .shim_input_blocked("s1".into())
            .await
            .unwrap()
            .as_deref(),
        Some(REASON_ADAPTER_STATE_GAP)
    );
    assert!(
        rig.store
            .shim_checkpoint(shim_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    let effect = rig
        .store
        .effect(claimed.effect_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (effect.state, effect.reason.as_deref()),
        (EffectState::OutcomeUnknown, Some(REASON_ADAPTER_STATE_GAP))
    );
    // A command after the gap goes nowhere.
    assert!(matches!(
        driver.apply(&claimed).await,
        fleetdeck_station::store::Dispatch::NotDispatched(_)
    ));
    driver.abandon().await;
    drop(driver);

    // A later station start restores no driver from the invalidated state.
    let configs = Configs {
        claude: Box::new(|_| None),
        codex: Box::new(|_| None),
    };
    let start = recover_station(&rig.store, &rig.host(), &configs, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    assert_eq!(
        start.ended,
        vec![(shim_id, REASON_ADAPTER_STATE_GAP.to_owned())]
    );
    assert!(alive(pid), "the shim and its provider are left alone");
}

// Review of DL-122: a dispatch that waited for the core lock while a gap
// was committed sends nothing. The gap here is committed by the store (as
// another path or owner would), so only the store fences and the recheck
// under the core lock can stop the command.
#[tokio::test]
async fn a_dispatch_racing_an_adapter_state_gap_sends_nothing() {
    let rig = Rig::new().await;
    rig.store
        .process_started("s1".into(), "proc-1".to_owned().into(), None)
        .await
        .unwrap();
    let out = rig.tmp.path().join("stdin.out");
    let (driver, shim_id, pid) = rig.drive(&format!("cat > '{}'", out.display())).await;
    let fence = rig.store.register_worker().await.unwrap();
    let envelope = CommandEnvelope {
        authority: None,
        command_id: "cmd-1".into(),
        target_incarnation: None,
        retry_until: i64::try_from(rig._det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    };
    rig.store
        .decide(envelope, Caller::local(), |d| {
            d.enqueue(
                &"s1".into(),
                fleetdeck_station::store::EffectPayload::Interrupt { authority: None },
            )?;
            Ok(CommandResult {
                session_id: Some("s1".into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    let claimed = rig.store.claim_next(fence).await.unwrap().unwrap();

    let guard = driver.hold_core().await;
    let racing = {
        let driver = Arc::clone(&driver);
        let effect = claimed.clone();
        tokio::spawn(async move { driver.apply(&effect).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !racing.is_finished(),
        "the dispatch waits for the core lock"
    );
    let generation = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .generation;
    assert!(
        rig.store
            .shim_adapter_gap(shim_id.clone(), generation, 0, 0)
            .await
            .unwrap()
    );
    drop(guard);

    let dispatch = racing.await.unwrap();
    assert!(
        matches!(
            &dispatch,
            fleetdeck_station::store::Dispatch::NotDispatched(reason)
                if reason == REASON_ADAPTER_STATE_GAP
        ),
        "{dispatch:?}"
    );
    assert!(driver.has_adapter_gap());
    assert!(
        rig.store
            .shim_transitions(shim_id.clone())
            .await
            .unwrap()
            .is_empty()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        std::fs::read(&out).unwrap_or_default(),
        Vec::<u8>::new(),
        "no byte reached the provider"
    );
    assert!(alive(pid));
    driver.abandon().await;
    drop(driver);
}

// DL-124: a session whose input was already blocked (a partial stdin
// write) when the gap was committed: every claim fence still holds.
#[tokio::test]
async fn a_gap_after_an_earlier_input_block_still_stops_every_dispatch() {
    let rig = Rig::new().await;
    rig.store
        .process_started("s1".into(), "proc-1".to_owned().into(), None)
        .await
        .unwrap();
    let out = rig.tmp.path().join("stdin.out");
    let (driver, shim_id, _pid) = rig.drive(&format!("cat > '{}'", out.display())).await;
    rig.store
        .shim_block_input(shim_id.clone(), "stdin_partial_write")
        .await
        .unwrap();
    let generation = rig
        .store
        .shim(shim_id.clone())
        .await
        .unwrap()
        .unwrap()
        .generation;
    assert!(
        rig.store
            .shim_adapter_gap(shim_id.clone(), generation, 0, 0)
            .await
            .unwrap()
    );
    let fence = rig.store.register_worker().await.unwrap();
    let envelope = CommandEnvelope {
        authority: None,
        command_id: "cmd-1".into(),
        target_incarnation: None,
        retry_until: i64::try_from(rig._det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    };
    rig.store
        .decide(envelope, Caller::local(), |d| {
            d.enqueue(
                &"s1".into(),
                fleetdeck_station::store::EffectPayload::Interrupt { authority: None },
            )?;
            Ok(CommandResult {
                session_id: Some("s1".into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    let claimed = rig.store.claim_next(fence).await.unwrap().unwrap();
    // The driver's memory has no gap; only the store can stop it.
    assert!(!driver.has_adapter_gap());
    let dispatch = driver.apply(&claimed).await;
    assert!(
        matches!(
            &dispatch,
            fleetdeck_station::store::Dispatch::NotDispatched(reason)
                if reason == REASON_ADAPTER_STATE_GAP
        ),
        "{dispatch:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read(&out).unwrap_or_default(), Vec::<u8>::new());
    driver.abandon().await;
    drop(driver);
    assert_eq!(
        rig.store
            .shim_input_blocked("s1".into())
            .await
            .unwrap()
            .as_deref(),
        Some("stdin_partial_write")
    );
    // Recovery must consult the persistent flag, not the first block reason.
    let configs = Configs {
        claude: Box::new(|_| None),
        codex: Box::new(|_| None),
    };
    let start = recover_station(&rig.store, &rig.host(), &configs, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(start.restored.is_empty());
    assert_eq!(
        start.ended,
        vec![(shim_id, REASON_ADAPTER_STATE_GAP.to_owned())]
    );
}
