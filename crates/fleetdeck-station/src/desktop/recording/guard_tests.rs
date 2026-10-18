//! The guards of the recording operation (PRE-PR round 1): the launch guard
//! on the thread of the recorder, the lease expiry of the operation through
//! the import and the commit, and the supervision that starts before any
//! bookkeeping await.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::capture::code;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::recording::{LeasePhase, StopReason};

use super::guard::LeaseGuard;
use super::rig_tests::{ID, Reply, T, err};
use crate::artifacts::capture_import::tests::FP;
use crate::artifacts::capture_import::{DurableFs, SyncFs};
use crate::desktop::Fail;
use crate::desktop::authority::{EXPIRED, START_LIMIT};
use crate::desktop::authority_tests::Harness;
use crate::desktop::code as desktop_code;
use crate::media::clock::Clocks;
use crate::store::capture_ops::CaptureState;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A filesystem whose file syncs (the ones of the import) take `0`.
struct SlowFs(Duration);

impl DurableFs for SlowFs {
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        std::thread::sleep(self.0);
        SyncFs.sync_file(f)
    }

    fn sync_dir(&self, d: &OwnedFd) -> std::io::Result<()> {
        SyncFs.sync_dir(d)
    }
}

type Run = tokio::task::JoinHandle<Result<DesktopActResult, Fail>>;

/// The recording in a task.
fn spawn_run(t: &T, req: super::RecordRequest) -> Run {
    let desktop = std::sync::Arc::clone(t.desktop());
    tokio::spawn(async move { desktop.screen_record(req).await })
}

/// The PID of the fake recorder, once it runs.
async fn recorder_pid(t: &T) -> i32 {
    for _ in 0..500 {
        if let Ok(text) = std::fs::read_to_string(t.dir().join("recorder.pid"))
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        tokio::time::sleep(ms(20)).await;
    }
    panic!("the recorder did not start");
}

// The launch guard.

#[tokio::test]
async fn a_stop_while_the_launch_is_queued_starts_no_recorder() {
    for (reason, save) in [
        (StopReason::OwnerStop, true),
        (StopReason::DeckPolicy, false),
    ] {
        let t = T::new().await;
        let block = t.desktop().recordings.hooks.hold_launch();
        let run = t.start(ID, "DP-1", 5);
        // The thread of the recorder is at its guard; the stop comes now.
        block.reached().await;
        let outcome = t
            .desktop()
            .stop_recording(t.store(), FP, ID, reason)
            .await
            .unwrap();
        assert_eq!(outcome, super::registry::StopOutcome::Live, "{save}");
        block.open();
        let (c, m) = err(run.await.unwrap());
        assert_eq!(
            (c, m.as_str()),
            (
                code::RECORDING_CANCELLED,
                fleetdeck_proto::capture::text::RECORDING_CANCELLED
            )
        );
        t.assert_discarded(ID, code::RECORDING_CANCELLED, reason)
            .await;
        assert_eq!(t.recorder_runs(), 0);
        assert!(!t.dir().join("recorder.pid").exists());
        assert_eq!(t.peer.count(LeasePhase::Record), 0);
    }
}

#[tokio::test]
async fn a_stop_that_missed_the_registry_and_writes_its_latch_late_is_seen_by_the_launch_guard() {
    // The stop looks the recording up before it registers itself, so it
    // finds nothing; its store write comes after the operation did its last
    // store read and reached the launch. The memory latch must still be set.
    let t = T::new().await;
    let hooks = &t.desktop().recordings.hooks;
    let before_insert = hooks.hold_before_insert();
    let after_miss = hooks.hold_after_miss();
    let block = hooks.hold_launch();
    let run = t.start(ID, "DP-1", 5);
    for _ in 0..400 {
        if t.row(ID).await.is_some() {
            break;
        }
        tokio::time::sleep(ms(10)).await;
    }
    let desktop = std::sync::Arc::clone(t.desktop());
    let store = t.store().clone();
    let stop = tokio::spawn(async move {
        desktop
            .stop_recording(&store, FP, ID, StopReason::OwnerStop)
            .await
    });
    // The stop is past its lookup and waits before the store write.
    tokio::time::sleep(ms(200)).await;
    before_insert.add_permits(1);
    // The operation registered, read the store (no latch yet) and queued.
    block.reached().await;
    after_miss.add_permits(1);
    assert_eq!(
        stop.await.unwrap().unwrap(),
        super::registry::StopOutcome::Live
    );
    block.open();
    assert_eq!(err(run.await.unwrap()).0, code::RECORDING_CANCELLED);
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::OwnerStop)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_start_permit_that_expires_while_the_launch_is_queued_starts_no_recorder() {
    let t = T::new().await;
    let h = Harness::new();
    let action = DesktopAction::ScreenRecord {
        monitor: "DP-1".to_owned(),
        seconds: 5,
    };
    let mut req = t.request(ID, "DP-1", 5);
    req.admitted = h.admitted(&action, 1);
    let block = t.desktop().recordings.hooks.hold_launch();
    let run = spawn_run(&t, req);
    block.reached().await;
    // The monotonic limit of the start passes while the launch waits.
    h.advance_mono(u64::try_from(START_LIMIT.as_millis()).unwrap() + 1);
    block.open();
    let (c, m) = err(run.await.unwrap());
    assert_eq!((c, m.as_str()), (desktop_code::FAILED, EXPIRED));
    t.assert_discarded(ID, desktop_code::FAILED, StopReason::LeaseDenied)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_connection_that_is_superseded_while_the_launch_is_queued_starts_no_recorder() {
    let t = T::new().await;
    let h = Harness::new();
    let action = DesktopAction::ScreenRecord {
        monitor: "DP-1".to_owned(),
        seconds: 5,
    };
    let mut req = t.request(ID, "DP-1", 5);
    req.admitted = h.admitted(&action, 1);
    let block = t.desktop().recordings.hooks.hold_launch();
    let run = spawn_run(&t, req);
    block.reached().await;
    h.current.store(false, std::sync::atomic::Ordering::SeqCst);
    block.open();
    assert_eq!(err(run.await.unwrap()).0, desktop_code::FORBIDDEN);
    t.assert_discarded(ID, desktop_code::FORBIDDEN, StopReason::LeaseDenied)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_gate_that_turns_off_while_the_launch_is_queued_starts_no_recorder() {
    let t = T::new().await;
    let block = t.desktop().recordings.hooks.hold_launch();
    let run = t.start(ID, "DP-1", 5);
    block.reached().await;
    t.allowed.lock().unwrap().capture = false;
    block.open();
    assert_eq!(err(run.await.unwrap()).0, desktop_code::FORBIDDEN);
    t.assert_discarded(ID, desktop_code::FORBIDDEN, StopReason::PolicyOff)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_lease_that_runs_out_while_the_launch_is_queued_starts_no_recorder() {
    let t = T::new().await;
    let block = t.desktop().recordings.hooks.hold_launch();
    let run = t.start(ID, "DP-1", 5);
    block.reached().await;
    // The boot clock jumps past the lease (a suspend).
    t.clocks.jump(10_000);
    block.open();
    // The guard of the launch or the lease guard sees it first; either way
    // no process starts and the reason is the lease.
    assert!(run.await.unwrap().is_err());
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.stop_reason, Some(StopReason::LeaseExpired));
    assert_eq!(t.recorder_runs(), 0);
    assert_eq!(t.artifact_count().await, 0);
}

// The lease expiry of the operation.

#[test]
fn a_lease_guard_never_moves_back_and_stays_out_after_a_suspend() {
    let clocks = super::rig_tests::Jumpy::new();
    let g = LeaseGuard::new(clocks.boot().plus(ms(1500)), clocks.clone());
    assert!(g.valid());
    g.renew(clocks.boot().plus(ms(60_000)));
    // An earlier expiry does not move it back.
    g.renew(clocks.boot().plus(ms(10)));
    std::thread::sleep(ms(30));
    assert!(g.valid());
    // The boot clock runs ahead of the monotonic clock: a suspend.
    clocks.jump(5000);
    assert!(!g.valid());
    // It stays out, also with a new expiry.
    g.renew(clocks.boot().plus(ms(60_000)));
    assert!(!g.valid());
}

#[tokio::test]
async fn a_lease_that_runs_out_during_a_slow_import_is_not_committed() {
    // The import lease answers late and leaves little validity; the next
    // renewal does not answer; the import takes longer than what is left.
    let t = T::new().await;
    t.set_timing(|timing| {
        timing.request = Duration::from_secs(3);
        timing.validity = ms(1500);
    });
    t.peer.set_phase(
        LeasePhase::Import,
        Reply::Delay(1200, Box::new(Reply::Continue)),
    );
    let req = t.request_with(ID, "DP-1", 1, std::sync::Arc::new(SlowFs(ms(1500))));
    let run = spawn_run(&t, req);
    for _ in 0..600 {
        if t.peer.count(LeasePhase::Import) > 0 {
            break;
        }
        tokio::time::sleep(ms(20)).await;
    }
    // From now no renewal gets an answer before its 3 s deadline.
    t.peer.set_phase(LeasePhase::Record, Reply::Hang);
    let result = tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("the recording did not end")
        .unwrap();
    assert_eq!(err(result).0, code::RECORDING_INTERRUPTED);
    t.assert_discarded(ID, code::RECORDING_INTERRUPTED, StopReason::LeaseExpired)
        .await;
}

// The supervision before the bookkeeping.

/// The recording ends, long before its 60 s, with the code of `reason`.
async fn ends_with(t: &T, run: Run, reason: StopReason) {
    let result = tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .expect("the recording did not stop")
        .unwrap();
    assert_eq!(err(result).0, code::RECORDING_INTERRUPTED);
    t.assert_discarded(ID, code::RECORDING_INTERRUPTED, reason)
        .await;
}

/// The recorder is killed and the discard is latched within the bound of
/// design §7.4 (2 s) while the phase write does not return.
async fn discards_with_a_blocked_phase_write(trigger: impl FnOnce(&T), reason: StopReason) {
    let t = T::new().await;
    let gate = t.desktop().recordings.hooks.hold_phase_write();
    let run = t.start(ID, "DP-1", 60);
    let pid = recorder_pid(&t).await;
    let began = std::time::Instant::now();
    trigger(&t);
    let latch = t.desktop().recordings.get(FP, ID).unwrap();
    loop {
        let gone = !Path::new(&format!("/proc/{pid}")).exists();
        if latch.first_discard() == Some(reason) && gone {
            break;
        }
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "no discard in 2 s: {:?}, gone: {gone}",
            latch.first_discard()
        );
        tokio::time::sleep(ms(20)).await;
    }
    // The write returns; the operation ends with the reason.
    gate.add_permits(1);
    ends_with(&t, run, reason).await;
}

#[tokio::test]
async fn the_policy_going_off_is_seen_while_the_phase_write_does_not_return() {
    discards_with_a_blocked_phase_write(
        |t| t.allowed.lock().unwrap().desktop = false,
        StopReason::PolicyOff,
    )
    .await;
}

#[tokio::test]
async fn a_link_that_closes_is_seen_while_the_phase_write_does_not_return() {
    discards_with_a_blocked_phase_write(|t| t.peer.close(), StopReason::LinkClosed).await;
}
