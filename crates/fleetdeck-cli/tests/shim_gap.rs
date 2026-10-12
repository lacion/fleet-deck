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
use fleetdeck_proto::event::{OutputGapReason, ProviderKind};
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_providers::adapter::ProviderError;
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, CheckpointError};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimHostConfig};
use fleetdeck_station::manager::durable::{
    DurableCore, DurableDriver, REASON_OUTPUT_FRAME_TOO_LARGE, Steps,
};
use fleetdeck_station::manager::shims::launch_shim;
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

    fn line(&mut self, text: &str) -> Steps {
        self.seen += 1;
        let event = Event::Error {
            code: "line".to_owned(),
            message: text.to_owned(),
            retry_at: None,
        };
        (vec![event.into()], Vec::new())
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        let event = Event::SessionEnded {
            reason: "exited".to_owned(),
            exit_code: code,
        };
        (vec![event.into()], Vec::new())
    }

    fn command(&mut self, _effect: &Effect) -> Result<Steps, ProviderError> {
        Err(ProviderError::NotReady("no commands"))
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
async fn an_unterminated_last_line_at_the_exit_is_an_explicit_gap_with_the_final_cursor() {
    let rig = Rig::new().await;
    let (_driver, shim_id, _pid) = rig.drive("echo first; printf partial").await;
    let events = rig.wait_ended().await;
    assert_eq!(lines(&events), vec!["first".to_owned()], "{events:?}");
    assert_eq!(
        gaps(&events),
        vec![(6, 13, OutputGapReason::UnterminatedLine)],
        "{events:?}"
    );
    assert_eq!(rig.cursor(&shim_id).await, 13);
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
