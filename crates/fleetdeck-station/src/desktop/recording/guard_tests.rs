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
use super::latch::Latch;
use super::rig_tests::{ID, Reply, T, err};
use crate::artifacts::capture_import::tests::FP;
use crate::artifacts::capture_import::{DurableFs, SyncFs};
use crate::desktop::Fail;
use crate::desktop::authority::{EXPIRED, START_LIMIT};
use crate::desktop::authority_tests::Harness;
use crate::desktop::code as desktop_code;
use crate::media::clock::Clocks;
use crate::store::capture_ops::{CaptureState, StopIntent};

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

/// The stop task runs; it leaves a pending stop in the registry (the
/// operation did not register yet).
async fn stop_that_misses_the_registry(
    t: &T,
) -> tokio::task::JoinHandle<Result<super::registry::StopOutcome, crate::store::StoreError>> {
    for _ in 0..400 {
        if t.row(ID).await.is_some() {
            break;
        }
        tokio::time::sleep(ms(10)).await;
    }
    let desktop = std::sync::Arc::clone(t.desktop());
    let store = t.store().clone();
    tokio::spawn(async move {
        desktop
            .stop_recording(&store, FP, ID, StopReason::OwnerStop)
            .await
    })
}

/// Waits until the registry holds the pending stop of the recording.
async fn until_pending(t: &T) {
    for _ in 0..400 {
        if t.desktop().recordings.pending_stop(FP, ID).is_some() {
            return;
        }
        tokio::time::sleep(ms(10)).await;
    }
    panic!("no pending stop");
}

#[tokio::test]
async fn a_pending_stop_is_seen_by_the_launch_guard_before_the_stop_writes_its_latch() {
    // (a) The stop is past its registry step and held before the store
    // write. The operation registers (it takes the pending stop) and reaches
    // the launch: no process starts.
    let t = T::new().await;
    let hooks = &t.desktop().recordings.hooks;
    let before_insert = hooks.hold_before_insert();
    let after_miss = hooks.hold_after_miss();
    let block = hooks.hold_launch();
    let run = t.start(ID, "DP-1", 5);
    let stop = stop_that_misses_the_registry(&t).await;
    until_pending(&t).await;
    before_insert.add_permits(1);
    block.reached().await;
    block.open();
    let (c, m) = err(run.await.unwrap());
    assert_eq!(
        (c, m.as_str()),
        (
            code::RECORDING_CANCELLED,
            fleetdeck_proto::capture::text::RECORDING_CANCELLED
        )
    );
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::OwnerStop)
        .await;
    assert_eq!(t.recorder_runs(), 0);
    assert!(!t.dir().join("recorder.pid").exists());
    // The stop task goes on: the row is terminal now, and no pending stop
    // is left.
    after_miss.add_permits(1);
    assert_eq!(
        stop.await.unwrap().unwrap(),
        super::registry::StopOutcome::NotRunning
    );
    assert_eq!(t.desktop().recordings.pending_count(), 0);
}

#[tokio::test]
async fn a_stop_whose_write_committed_is_seen_by_the_launch_guard_before_its_next_step() {
    // (b) The store write of the stop committed; the stop task is held
    // before any later step. The operation registers and launches: no
    // process starts.
    let t = T::new().await;
    let hooks = &t.desktop().recordings.hooks;
    let before_insert = hooks.hold_before_insert();
    let after_write = hooks.hold_after_write();
    let run = t.start(ID, "DP-1", 5);
    let stop = stop_that_misses_the_registry(&t).await;
    for _ in 0..400 {
        if t.row(ID).await.unwrap().stop_intent != StopIntent::None {
            break;
        }
        tokio::time::sleep(ms(10)).await;
    }
    assert_eq!(t.row(ID).await.unwrap().stop_intent, StopIntent::Save);
    // The operation registers: the stop is in its memory latch before its
    // first read of the store.
    before_insert.add_permits(1);
    assert_eq!(err(run.await.unwrap()).0, code::RECORDING_CANCELLED);
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::OwnerStop)
        .await;
    assert_eq!(t.recorder_runs(), 0);
    assert!(!t.dir().join("recorder.pid").exists());
    after_write.add_permits(1);
    assert_eq!(
        stop.await.unwrap().unwrap(),
        super::registry::StopOutcome::Latched
    );
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

/// A lease guard on the jumpy clocks, with the latch of a recording.
fn lease_guard(
    t: &T,
    clocks: &std::sync::Arc<super::rig_tests::Jumpy>,
    life: Duration,
) -> (std::sync::Arc<LeaseGuard>, std::sync::Arc<Latch>) {
    let latch = Latch::new(t.store().clone(), FP, ID);
    let guard = LeaseGuard::new(clocks.boot().plus(life), clocks.clone(), latch.clone());
    (guard, latch)
}

#[tokio::test]
async fn a_lease_guard_never_moves_back_and_stays_out_after_a_suspend() {
    let t = T::new().await;
    let clocks = super::rig_tests::Jumpy::new();
    let (g, latch) = lease_guard(&t, &clocks, ms(1500));
    assert!(g.valid());
    g.renew(clocks.boot().plus(ms(60_000)));
    // An earlier expiry does not move it back.
    g.renew(clocks.boot().plus(ms(10)));
    std::thread::sleep(ms(30));
    assert!(g.valid());
    assert_eq!(latch.first_discard(), None);
    // The boot clock runs ahead of the monotonic clock: a suspend.
    clocks.jump(5000);
    assert!(!g.valid());
    assert_eq!(latch.first_discard(), Some(StopReason::LeaseExpired));
    // It stays out, also with a new expiry.
    g.renew(clocks.boot().plus(ms(60_000)));
    assert!(!g.valid());
}

#[tokio::test]
async fn a_renewal_after_the_deadline_does_not_extend_the_lease() {
    // The deadline passes with no tick of the guard in between (the guard is
    // not armed), then a renewal arrives: the lease is out for good, the
    // latch holds `lease_expired`, and the check before the commit refuses.
    let t = T::new().await;
    let clocks = super::rig_tests::Jumpy::new();
    let (g, latch) = lease_guard(&t, &clocks, ms(50));
    std::thread::sleep(ms(120));
    g.renew(clocks.boot().plus(ms(60_000)));
    assert_eq!(latch.first_discard(), Some(StopReason::LeaseExpired));
    assert!(!g.valid());
    // Also an import lease that comes later does not bring it back.
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
