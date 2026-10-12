//! Station stdin settlement through real shims (ADR-005 §4.3, tests
//! 26–35; slice 5, DL-105). Each test claims a durable frame identity in
//! the station store, stops at one crash boundary, then runs the station
//! recovery of a new "station start" and checks the effect state, the
//! bytes that the provider read and the shim's tombstones.
//!
//! A "station crash" drops the attachment (or sends a raw frame and drops
//! the connection); the shim keeps running, as after a real station kill.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::{CommandId, InputId};
use fleetdeck_providers::host::ProcessSpec;
use fleetdeck_providers::host::shim::client::{AttachResult, Attachment, Connection};
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::journal::ShimFiles;
use fleetdeck_providers::host::shim::settle::{
    EffectResult, RETIRE_ROWS_ENABLED, SHIM_NOT_RECEIVED, SHIM_WRITE_FAILED,
};
use fleetdeck_providers::host::shim::stdin::{WriteOutcome, WriteTag};
use fleetdeck_providers::host::shim::wire::{StatusAnswer, ToShim, WriteState};
use fleetdeck_station::manager::shim_stdin::{Sent, send_input, settle_live};
use fleetdeck_station::manager::shims::{Recovered, launch_shim, reason, recover_shims};
use fleetdeck_station::store::{
    Caller, EffectId, EffectPayload, EffectState, InputRecord, NewSession, OfflineApprovalPolicy,
    Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use tokio::io::{AsyncBufReadExt as _, BufReader};

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(10);
/// The recovery wait for a write in flight (30 s in production).
const IN_FLIGHT: Duration = Duration::from_secs(1);

struct Rig {
    tmp: tempfile::TempDir,
    store: Store,
    det: Det,
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
        store
            .process_started("s1".into(), "proc-1".to_owned().into(), None)
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        Self { tmp, store, det }
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

    fn out(&self) -> PathBuf {
        self.tmp.path().join("stdin.out")
    }

    /// Launches `script`; `$OUT` is the file that receives what the
    /// provider reads.
    async fn launch(&self, script: &str) -> Attached {
        let spec = ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: self.tmp.path().to_owned(),
            env: ChildEnv::scrubbed([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("OUT".into(), self.out().into_os_string()),
            ]),
        };
        launch_shim(
            &self.store,
            &self.host(),
            &"s1".into(),
            &spec,
            AckMode::OnDelivery,
        )
        .await
        .unwrap()
    }

    /// A queued prompt effect, claimed by a fresh worker.
    async fn claimed(&self, n: u32) -> EffectId {
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("cmd-{n}").as_str()),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::Prompt {
                session_id: "s1".into(),
                input: UserInput {
                    text: format!("prompt {n}"),
                },
            },
        };
        self.store
            .decide(envelope, Caller::local(), move |d| {
                d.enqueue(
                    &"s1".into(),
                    EffectPayload::Prompt {
                        input: InputRecord {
                            input_id: InputId::from(format!("in-{n}").as_str()),
                            origin: RunOrigin::Board,
                            input: UserInput {
                                text: format!("prompt {n}"),
                            },
                        },
                    },
                )?;
                Ok(CommandResult {
                    session_id: Some("s1".into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
        let fence = self.store.register_worker().await.unwrap();
        let effect = self.store.claim_next(fence).await.unwrap().unwrap();
        assert_eq!(effect.state, EffectState::Claimed);
        effect.effect_id
    }

    async fn tag(&self, attached: &Attached, effect: &EffectId) -> WriteTag {
        self.store
            .shim_claim_write(
                attached.record.shim_id.clone(),
                attached.process.generation(),
                Some(effect.clone()),
                0,
            )
            .await
            .unwrap()
            .unwrap()
    }

    /// A new station start: recovery with a fresh host.
    async fn recover(&self) -> Attached {
        let mut all = recover_shims(&self.store, &self.host(), AckMode::OnDelivery, IN_FLIGHT)
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        match all.remove(0).1 {
            Recovered::Reattached(attached) => attached,
            other => panic!("not reattached: {other:?}"),
        }
    }

    async fn effect(&self, id: &EffectId) -> (EffectState, Option<String>) {
        let e = self.store.effect(id.clone()).await.unwrap().unwrap();
        (e.state, e.reason)
    }

    fn read(&self) -> Vec<u8> {
        std::fs::read(self.out()).unwrap_or_default()
    }

    /// A raw attachment with the next committed generation, as a station
    /// that sends a frame and dies.
    async fn raw_from_record(&self) -> (Attachment, u64) {
        let rows = self.store.shims_to_recover().await.unwrap();
        let row = &rows[0];
        let generation = self
            .store
            .shim_next_generation(row.shim_id.clone())
            .await
            .unwrap()
            .unwrap();
        let key = ShimFiles::new(&self.host().config().shims_dir(), &row.shim_id)
            .read_key()
            .unwrap();
        let connection = Connection::open(Path::new(&row.socket_path), WAIT)
            .await
            .unwrap();
        match connection
            .attach(&key, self.store.incarnation().as_str(), generation, WAIT)
            .await
            .unwrap()
        {
            AttachResult::Attached { attachment, .. } => (attachment, generation),
            AttachResult::Refused(r) => panic!("refused: {r:?}"),
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.tmp.path().join("station").join("shims"));
    }
}

fn write_frame(generation: u64, tag: &WriteTag) -> ToShim {
    ToShim::Write {
        generation,
        write_id: tag.write_id.clone(),
        write_seq: tag.write_seq,
        sent_at: 0,
    }
}

async fn ready(attached: &mut Attached) {
    let mut out = BufReader::new(&mut attached.stdout);
    let mut text = String::new();
    tokio::time::timeout(WAIT, out.read_line(&mut text))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(text.trim(), "ready");
}

/// Waits until the provider wrote `n` bytes to `$OUT`.
async fn read_bytes(rig: &Rig, n: usize) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while rig.read().len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the provider read {} bytes",
            rig.read().len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const SINK: &str = "exec cat > \"$OUT\"";

#[test]
fn the_retirement_rows_are_enabled_in_the_build_that_has_tests_26_to_35() {
    const { assert!(RETIRE_ROWS_ENABLED) };
}

// 26 + 35
#[tokio::test]
async fn a_station_killed_after_the_claim_before_the_frame_retires_the_effect_and_the_tombstone_refuses_a_late_frame()
 {
    let rig = Rig::new().await;
    let first = rig.launch(SINK).await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    // Crash: the identity is committed, no frame left.
    let old = first.process.clone();
    drop(first);
    let second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::RetiredNotDispatched,
            Some(SHIM_NOT_RECEIVED.to_owned())
        )
    );
    // 35: the tombstone refuses the same identity on the new attachment,
    // and the stale attachment's frame is dropped.
    assert_eq!(
        second.process.write_tagged(&tag, b"late\n").await.unwrap(),
        WriteOutcome::Done(WriteState::ClosedUnwritten)
    );
    let _ = old.write_tagged(&tag, b"stale\n").await;
    // A new effect identity is written normally.
    let next = rig.claimed(2).await;
    let sent = send_input(
        &rig.store,
        &second.process,
        &second.record.shim_id,
        &next,
        b"new\n",
    )
    .await
    .unwrap();
    assert_eq!(sent, Sent::Written);
    read_bytes(&rig, 4).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.read(), b"new\n");
}

// 27
#[tokio::test]
async fn a_frame_torn_mid_payload_retires_the_effect_and_the_provider_reads_no_byte() {
    let rig = Rig::new().await;
    let first = rig.launch(SINK).await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    drop(first);
    let (raw, generation) = rig.raw_from_record().await;
    raw.send_torn(&write_frame(generation, &tag), &[b'x'; 4000], 3000)
        .await
        .unwrap();
    let second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::RetiredNotDispatched,
            Some(SHIM_NOT_RECEIVED.to_owned())
        )
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rig.read().is_empty());
    drop(second);
}

// 28
#[tokio::test]
async fn a_full_frame_without_its_answer_is_outcome_unknown_never_resent_and_read_once() {
    let rig = Rig::new().await;
    let first = rig.launch(SINK).await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    drop(first);
    let (mut raw, generation) = rig.raw_from_record().await;
    raw.send(&write_frame(generation, &tag), b"once\n")
        .await
        .unwrap();
    read_bytes(&rig, 5).await;
    drop(raw);
    let second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::OutcomeUnknown,
            Some("shim_write_unknown".to_owned())
        )
    );
    // Nothing resends it; a resubmission of the identity is a duplicate.
    assert_eq!(
        second.process.write_tagged(&tag, b"once\n").await.unwrap(),
        WriteOutcome::Done(WriteState::Written)
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.read(), b"once\n");
}

// 29
#[tokio::test]
async fn a_write_blocked_on_a_full_pipe_is_outcome_unknown_after_the_bounded_wait() {
    let rig = Rig::new().await;
    let first = rig.launch("exec sleep 30").await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    drop(first);
    let (mut raw, generation) = rig.raw_from_record().await;
    raw.send(&write_frame(generation, &tag), &vec![b'y'; 512 * 1024])
        .await
        .unwrap();
    drop(raw);
    let started = std::time::Instant::now();
    let second = rig.recover().await;
    assert!(
        started.elapsed() >= IN_FLIGHT,
        "recovery asked again for the bounded wait"
    );
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::OutcomeUnknown,
            Some("shim_write_unknown".to_owned())
        )
    );
    drop(second);
}

// 30
#[tokio::test]
async fn a_partial_write_is_outcome_unknown_blocks_input_and_is_never_resent() {
    let rig = Rig::new().await;
    let mut first = rig
        .launch("head -c 10 > \"$OUT\"; exec 0<&-; echo ready; exec sleep 30")
        .await;
    let effect = rig.claimed(1).await;
    // Live path: the frame is larger than the pipe; the reader stops after
    // 10 bytes and closes its end.
    let sent = send_input(
        &rig.store,
        &first.process,
        &first.record.shim_id,
        &effect,
        &vec![b'z'; 512 * 1024],
    )
    .await
    .unwrap();
    ready(&mut first).await;
    let Sent::Ended(state @ WriteState::Partial { .. }) = &sent else {
        panic!("not partial: {sent:?}");
    };
    assert_eq!(
        settle_live(&sent),
        EffectResult::Unknown { block_input: true }
    );
    // Station crash before it settled the live result: recovery settles it.
    drop(first);
    let _second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::OutcomeUnknown,
            Some("stdin_partial_write".to_owned())
        )
    );
    assert_eq!(
        rig.store
            .shim_input_blocked("s1".into())
            .await
            .unwrap()
            .as_deref(),
        Some("stdin_partial_write")
    );
    assert!(matches!(state, WriteState::Partial { bytes_done, .. } if *bytes_done > 0));
    assert_eq!(rig.read().len(), 10);
}

// 31
#[tokio::test]
async fn an_epipe_on_the_first_write_call_retires_the_effect_as_write_failed() {
    let rig = Rig::new().await;
    let mut first = rig.launch("exec 0<&-; echo ready; exec sleep 30").await;
    ready(&mut first).await;
    // Live result.
    let live = rig.claimed(1).await;
    let sent = send_input(
        &rig.store,
        &first.process,
        &first.record.shim_id,
        &live,
        b"x\n",
    )
    .await
    .unwrap();
    assert!(
        matches!(sent, Sent::Ended(WriteState::FailedUnwritten { .. })),
        "{sent:?}"
    );
    assert_eq!(settle_live(&sent), EffectResult::Retired(SHIM_WRITE_FAILED));
    // Recovery result of the same kind of write.
    let effect = rig.claimed(2).await;
    let tag = rig.tag(&first, &effect).await;
    assert!(matches!(
        first.process.write_tagged(&tag, b"y\n").await.unwrap(),
        WriteOutcome::Done(WriteState::FailedUnwritten { .. })
    ));
    drop(first);
    let _second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::RetiredNotDispatched,
            Some(SHIM_WRITE_FAILED.to_owned())
        )
    );
}

// 32
#[tokio::test]
async fn an_evicted_record_is_forgotten_outcome_unknown_and_a_resubmission_is_refused_without_io() {
    let rig = Rig::new().await;
    let first = rig.launch("exec cat > /dev/null").await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    assert_eq!(
        first.process.write_tagged(&tag, b"a\n").await.unwrap(),
        WriteOutcome::Done(WriteState::Written)
    );
    // More terminal records than the shim keeps.
    for ordinal in 0..4200u32 {
        let internal = rig
            .store
            .shim_claim_write(
                first.record.shim_id.clone(),
                first.process.generation(),
                None,
                ordinal,
            )
            .await
            .unwrap()
            .unwrap();
        first.process.write_tagged(&internal, b"b\n").await.unwrap();
    }
    assert_eq!(
        first.process.write_tagged(&tag, b"a\n").await.unwrap(),
        WriteOutcome::Refused(StatusAnswer::Forgotten)
    );
    drop(first);
    let _second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::OutcomeUnknown,
            Some("shim_write_unknown".to_owned())
        )
    );
}

// 33
#[tokio::test]
async fn a_frame_of_the_old_generation_after_a_takeover_is_dropped_and_the_effect_retired() {
    let rig = Rig::new().await;
    let first = rig.launch(SINK).await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    let old = first.process.clone();
    // A second station takes over before the old one sends the frame.
    let _second = rig.recover().await;
    assert_eq!(
        rig.effect(&effect).await,
        (
            EffectState::RetiredNotDispatched,
            Some(SHIM_NOT_RECEIVED.to_owned())
        )
    );
    let _ = old.write_tagged(&tag, b"stale\n").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rig.read().is_empty());
    // A stale generation cannot claim a new identity either.
    let next = rig.claimed(2).await;
    assert_eq!(
        send_input(&rig.store, &old, &first.record.shim_id, &next, b"x\n")
            .await
            .unwrap(),
        Sent::NotSent
    );
}

// 34
#[tokio::test]
async fn a_shim_killed_during_a_write_leaves_the_session_lost_and_the_effect_outcome_unknown() {
    let rig = Rig::new().await;
    let first = rig.launch("exec sleep 30").await;
    let effect = rig.claimed(1).await;
    let tag = rig.tag(&first, &effect).await;
    let shim_id = first.record.shim_id.clone();
    let process = first.process.clone();
    let write =
        tokio::spawn(async move { process.write_tagged(&tag, &vec![b'w'; 512 * 1024]).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    shim_reaper::kill9(shim_reaper::shim_pid(&shim_id).unwrap());
    assert!(write.await.unwrap().is_err());
    drop(first);
    let all = recover_shims(&rig.store, &rig.host(), AckMode::OnDelivery, IN_FLIGHT)
        .await
        .unwrap();
    assert!(
        matches!(all[0].1, Recovered::Lost(reason::EXECUTION_UNKNOWN)),
        "{:?}",
        all[0].1
    );
    // The generic recovery of the lost process settles the claim.
    rig.store.recover(Vec::new()).await.unwrap();
    assert_eq!(rig.effect(&effect).await.0, EffectState::OutcomeUnknown);
}
