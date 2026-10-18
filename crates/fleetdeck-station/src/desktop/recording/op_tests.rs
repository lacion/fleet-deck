//! The recording operation on a fake desktop and a fake `wf-recorder`: the
//! happy path, the stop before the spawn, the refusals of the start, the
//! checks of the finished file, the import lease and the cleanup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::fd::OwnedFd;
use std::sync::Arc;

use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::capture::{CaptureKind, code, text};
use fleetdeck_proto::recording::{
    LeasePhase, PEAK_RESERVATION, RecordingMeta, RecordingPhase, StopReason,
};

use super::registry::StopOutcome;
use super::rig_tests::{ID, ID2, Reply, T, err, timing};
use crate::artifacts::capture_import::DurableFs;
use crate::artifacts::capture_import::tests::FP;
use crate::desktop::code as desktop_code;
use crate::desktop::fake::{Opts, Rig};
use crate::media::mp4_fixture::Mp4Fixture;
use crate::store::capture_ops::CaptureState;

/// A filesystem whose syncs always fail.
struct FailFs;

impl DurableFs for FailFs {
    fn sync_file(&self, _: &std::fs::File) -> std::io::Result<()> {
        Err(std::io::Error::other("a sync failed"))
    }

    fn sync_dir(&self, _: &OwnedFd) -> std::io::Result<()> {
        Err(std::io::Error::other("a sync failed"))
    }
}

const ARGV: [&str; 22] = [
    "-o",
    "DP-1",
    "-r",
    "30",
    "-D",
    "-c",
    "libx264",
    "-x",
    "yuv420p",
    "-b",
    "0",
    "-p",
    "preset=veryfast",
    "-p",
    "crf=23",
    "-F",
    "fps=30,trim=duration=1",
    "-m",
    "mp4",
    "-y",
    "-f",
    "/proc/self/fd/3",
];

#[tokio::test]
async fn a_recording_becomes_one_video_artifact_with_its_provenance() {
    let t = T::new().await;
    let done = t.run(ID, "DP-1", 1).await.unwrap();
    assert_eq!(done.address, None);
    assert_eq!(done.info_fresh, None);
    let result = done.capture.unwrap();
    assert_eq!(result.capture_id, ID);
    let p = result.published;
    assert!(p.created);
    assert_eq!(p.artifact.title, "Desktop recording \u{b7} srv/DP-1");
    assert_eq!(p.artifact.kind, ArtifactKind::Video);
    assert_eq!(p.artifact.author, "owner");
    assert_eq!(p.artifact.session_id, None);
    assert_eq!(p.version.version, 1);
    assert_eq!(p.version.files, ["capture.mp4"]);
    let c = p.version.capture.clone().unwrap();
    assert_eq!(c.kind, CaptureKind::Recording);
    assert_eq!((c.capture_id.as_str(), c.monitor.as_str()), (ID, "DP-1"));
    assert_eq!((c.caller_station, c.caller_session), (None, None));
    assert_eq!(
        c.recording,
        Some(RecordingMeta {
            requested_seconds: 1,
            duration_ms: 1000,
            width: 3440,
            height: 1440,
            stop_reason: StopReason::Duration,
        })
    );
    // The version file is the exact fixture, from the retained descriptor.
    let id = p.artifact.artifact_id.clone();
    let bytes = std::fs::read(t.s.root.join(&id).join("1/capture.mp4")).unwrap();
    assert_eq!(bytes, super::rig_tests::fixture());
    assert_eq!(p.version.size, bytes.len() as u64);
    // The row: committed, the reservation freed after the staging went.
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Committed);
    assert_eq!(row.op.reserved_bytes, 0);
    assert_eq!(row.stop_reason, Some(StopReason::Duration));
    assert_eq!(row.duration_ms, Some(1000));
    assert_eq!((row.width, row.height), (Some(3440), Some(1440)));
    assert_eq!(t.leftovers(), (Vec::new(), vec![id]));
    assert_eq!(t.artifact_count().await, 1);
    // The recorder ran once with the exact argv.
    let log = std::fs::read_to_string(t.dir().join("recorder.log")).unwrap();
    let mut want: Vec<&str> = ARGV.to_vec();
    want.push("--");
    assert_eq!(log.lines().collect::<Vec<_>>(), want);
    // The leases: start first, then `record` renewals, and one import
    // request that was answered before the commit. Renewals go on while the
    // recording finalizes, so `record` may come after `import`.
    let phases = t.peer.phases();
    assert_eq!(phases[0], LeasePhase::Start);
    let first_record = phases.iter().position(|p| *p == LeasePhase::Record);
    let import = phases.iter().position(|p| *p == LeasePhase::Import);
    assert!(first_record.is_some());
    assert!(first_record < import, "{phases:?}");
    assert_eq!(t.peer.count(LeasePhase::Import), 1);
    let seq: Vec<u64> = t
        .peer
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.sequence)
        .collect();
    assert!(seq.windows(2).all(|w| w[1] == w[0] + 1), "{seq:?}");
    assert_eq!(t.peer.peak.load(std::sync::atomic::Ordering::SeqCst), 1);
    // The slot is free again.
    assert!(t.desktop().recording_of(FP).is_none());
}

#[tokio::test]
async fn an_owner_stop_in_the_middle_saves_the_recording() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    let outcome = t
        .desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::Live);
    let p = run.await.unwrap().unwrap().capture.unwrap().published;
    let meta = p.version.capture.unwrap().recording.unwrap();
    assert_eq!(meta.stop_reason, StopReason::OwnerStop);
    assert_eq!(meta.requested_seconds, 60);
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Committed);
    assert_eq!(row.stop_reason, Some(StopReason::OwnerStop));
}

#[tokio::test]
async fn a_lease_save_answer_saves_the_recording() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    // The recorder has written its file; the deck now says: save.
    t.until_recording(ID).await;
    t.peer.set_phase(LeasePhase::Record, Reply::Save);
    let p = run.await.unwrap().unwrap().capture.unwrap().published;
    let meta = p.version.capture.unwrap().recording.unwrap();
    assert_eq!(meta.stop_reason, StopReason::OwnerStop);
}

#[tokio::test]
async fn the_phase_of_the_row_follows_the_recording() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.phase, Some(RecordingPhase::Recording));
    assert!(row.recorder_started_at.is_some());
    assert_eq!(row.requested_seconds, 60);
    assert_eq!(row.link_epoch.as_deref(), Some("epoch-1"));
    // The slot names this recording while it runs.
    assert_eq!(t.desktop().recording_of(FP).unwrap().capture_id, ID);
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_stop_before_the_spawn_is_cancelled_and_no_recorder_runs() {
    let t = T::new().await;
    t.peer.set_phase(
        LeasePhase::Start,
        Reply::Delay(500, Box::new(Reply::Continue)),
    );
    let run = t.start(ID, "DP-1", 5);
    for _ in 0..200 {
        if t.row(ID).await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    let (c, m) = err(run.await.unwrap());
    assert_eq!(
        (c, m.as_str()),
        (code::RECORDING_CANCELLED, text::RECORDING_CANCELLED)
    );
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::OwnerStop)
        .await;
    assert_eq!(t.recorder_runs(), 0);
    assert_eq!(t.peer.count(LeasePhase::Record), 0);
}

#[tokio::test]
async fn a_discard_latched_before_the_spawn_is_cancelled_too() {
    let t = T::new().await;
    t.peer.set_phase(
        LeasePhase::Start,
        Reply::Delay(500, Box::new(Reply::Continue)),
    );
    let run = t.start(ID, "DP-1", 5);
    for _ in 0..200 {
        if t.row(ID).await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::DeckPolicy)
        .await
        .unwrap();
    assert_eq!(err(run.await.unwrap()).0, code::RECORDING_CANCELLED);
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::DeckPolicy)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_start_lease_that_the_deck_refuses_discards_with_its_reason() {
    let t = T::new().await;
    t.peer
        .set_phase(LeasePhase::Start, Reply::Discard(StopReason::DeckPolicy));
    let (c, m) = err(t.run(ID, "DP-1", 5).await);
    assert_eq!(
        (c, m.as_str()),
        (code::RECORDING_INTERRUPTED, text::RECORDING_INTERRUPTED)
    );
    t.assert_discarded(ID, code::RECORDING_INTERRUPTED, StopReason::DeckPolicy)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_start_lease_without_an_answer_is_denied() {
    for reply in [
        Reply::Hang,
        Reply::Transport,
        Reply::Wire("forbidden"),
        Reply::Garbage,
        Reply::WrongSequence,
        Reply::Discard(StopReason::Duration),
    ] {
        let t = T::new().await;
        t.peer.set_phase(LeasePhase::Start, reply);
        let (c, _) = err(t.run(ID, "DP-1", 5).await);
        assert_eq!(c, code::RECORDING_INTERRUPTED);
        t.assert_discarded(ID, code::RECORDING_INTERRUPTED, StopReason::LeaseDenied)
            .await;
        assert_eq!(t.recorder_runs(), 0);
    }
}

#[tokio::test]
async fn a_start_lease_with_the_owner_stop_cancels() {
    let t = T::new().await;
    t.peer.set_phase(LeasePhase::Start, Reply::Save);
    assert_eq!(err(t.run(ID, "DP-1", 5).await).0, code::RECORDING_CANCELLED);
    t.assert_discarded(ID, code::RECORDING_CANCELLED, StopReason::OwnerStop)
        .await;
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn a_file_that_does_not_validate_is_recording_invalid() {
    let wrong_size = Mp4Fixture::default().size(1920, 1080).samples(30).build();
    let fragmented = Mp4Fixture::default()
        .size(3440, 1440)
        .samples(30)
        .moof()
        .build();
    let too_long = Mp4Fixture::default().size(3440, 1440).samples(60).build();
    for bytes in [
        wrong_size,
        fragmented,
        too_long,
        b"not an mp4 file at all".to_vec(),
    ] {
        let t = T::with(Rig::new(Opts::default()), Some(bytes)).await;
        let (c, m) = err(t.run(ID, "DP-1", 1).await);
        assert_eq!(
            (c, m.as_str()),
            (code::RECORDING_INVALID, text::RECORDING_INVALID)
        );
        // The save reason stays; nothing was imported.
        t.assert_discarded(ID, code::RECORDING_INVALID, StopReason::Duration)
            .await;
        assert_eq!(t.peer.count(LeasePhase::Import), 0);
    }
}

#[tokio::test]
async fn an_empty_file_is_recording_invalid() {
    let t = T::with(Rig::new(Opts::default()), Some(Vec::new())).await;
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, code::RECORDING_INVALID);
    t.assert_discarded(ID, code::RECORDING_INVALID, StopReason::Duration)
        .await;
}

#[tokio::test]
async fn the_dimensions_swap_for_an_odd_transform() {
    let t = T::with(
        Rig::new(Opts::default()),
        Some(Mp4Fixture::default().size(1440, 3440).samples(30).build()),
    )
    .await;
    let monitors = std::fs::read_to_string(t.dir().join("monitors.json")).unwrap();
    t.set_monitors(&monitors.replacen("\"focused\":true", "\"focused\":true,\"transform\":1", 1));
    let p = t
        .run(ID, "DP-1", 1)
        .await
        .unwrap()
        .capture
        .unwrap()
        .published;
    let meta = p.version.capture.unwrap().recording.unwrap();
    assert_eq!((meta.width, meta.height), (1440, 3440));
}

#[tokio::test]
async fn the_import_lease_decides_before_the_import() {
    for (reply, reason) in [
        (
            Reply::Discard(StopReason::DeckPolicy),
            StopReason::DeckPolicy,
        ),
        (
            Reply::Discard(StopReason::ArtifactDeleted),
            StopReason::ArtifactDeleted,
        ),
        (Reply::Transport, StopReason::LeaseDenied),
        (Reply::Wire("forbidden"), StopReason::LeaseDenied),
        (Reply::Garbage, StopReason::LeaseDenied),
    ] {
        let t = T::new().await;
        t.peer.set_phase(LeasePhase::Import, reply);
        let (c, _) = err(t.run(ID, "DP-1", 1).await);
        assert_eq!(c, code::RECORDING_INTERRUPTED);
        t.assert_discarded(ID, code::RECORDING_INTERRUPTED, reason)
            .await;
        assert_eq!(t.peer.count(LeasePhase::Import), 1);
    }
}

#[tokio::test]
async fn an_import_lease_with_a_save_intent_imports() {
    let t = T::new().await;
    t.peer.set_phase(LeasePhase::Import, Reply::Save);
    assert!(t.run(ID, "DP-1", 1).await.is_ok());
}

#[tokio::test]
async fn a_failed_sync_keeps_the_reservation_until_the_cleanup_is_durable() {
    // Validation fails, the cleanup cannot sync: the row is `failed` and the
    // peak reservation stays.
    let t = T::with(
        Rig::new(Opts::default()),
        Some(Mp4Fixture::default().size(1920, 1080).samples(30).build()),
    )
    .await;
    let req = t.request_with(ID, "DP-1", 1, Arc::new(FailFs));
    assert_eq!(
        err(t.desktop().screen_record(req).await).0,
        code::RECORDING_INVALID
    );
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.op.reserved_bytes, PEAK_RESERVATION);
    assert_eq!(t.artifact_count().await, 0);
}

#[tokio::test]
async fn a_failed_sync_of_the_import_is_a_publish_failure_without_an_artifact() {
    let t = T::new().await;
    let req = t.request_with(ID, "DP-1", 1, Arc::new(FailFs));
    let (c, m) = err(t.desktop().screen_record(req).await);
    assert_eq!(
        (c, m.as_str()),
        (
            code::RECORDING_PUBLISH_FAILED,
            text::RECORDING_PUBLISH_FAILED
        )
    );
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.stop_reason, Some(StopReason::Duration));
    assert_eq!(t.artifact_count().await, 0);
}

#[tokio::test]
async fn a_recorder_that_ignores_sigint_is_an_escalated_stop_and_is_discarded() {
    let t = T::new().await;
    t.flag("ignore_int");
    let (c, m) = err(t.run(ID, "DP-1", 1).await);
    assert_eq!(
        (c, m.as_str()),
        (code::RECORDING_FAILED, text::RECORDING_FAILED)
    );
    t.assert_discarded(ID, code::RECORDING_FAILED, StopReason::RecorderExited)
        .await;
    assert_eq!(t.peer.count(LeasePhase::Import), 0);
}

#[tokio::test]
async fn a_recorder_that_exits_on_its_own_is_discarded_even_with_code_zero() {
    let t = T::new().await;
    t.flag("exit_early");
    let (c, _) = err(t.run(ID, "DP-1", 60).await);
    assert_eq!(c, code::RECORDING_FAILED);
    t.assert_discarded(ID, code::RECORDING_FAILED, StopReason::RecorderExited)
        .await;
}

#[tokio::test]
async fn a_recorder_that_fails_at_once_is_discarded() {
    let t = T::new().await;
    t.flag("recorder_fail");
    assert_eq!(err(t.run(ID, "DP-1", 60).await).0, code::RECORDING_FAILED);
    t.assert_discarded(ID, code::RECORDING_FAILED, StopReason::RecorderExited)
        .await;
}

#[tokio::test]
async fn the_gates_and_the_admission_refuse_before_any_row() {
    let t = T::new().await;
    t.allowed.lock().unwrap().desktop = false;
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, desktop_code::FORBIDDEN);
    t.allowed.lock().unwrap().desktop = true;
    t.allowed.lock().unwrap().capture = false;
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, desktop_code::FORBIDDEN);
    t.allowed.lock().unwrap().capture = true;
    t.allowed.lock().unwrap().monitors = vec!["HDMI-A-1".to_owned()];
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, desktop_code::FORBIDDEN);
    t.allowed.lock().unwrap().monitors = Vec::new();
    assert_eq!(err(t.run(ID, "DP-9", 1).await).0, desktop_code::NOT_FOUND);
    for seconds in [0, 121] {
        assert_eq!(
            err(t.run(ID, "DP-1", seconds).await).0,
            desktop_code::INVALID_PARAMS
        );
    }
    t.flag("hyprlock");
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, code::SCREEN_LOCKED);
    assert!(t.row(ID).await.is_none());
    assert_eq!(t.recorder_runs(), 0);
    assert_eq!(t.leftovers(), (Vec::new(), Vec::new()));
    assert!(t.peer.phases().is_empty());
    assert!(t.desktop().recording_of(FP).is_none());
}

#[tokio::test]
async fn a_monitor_with_unsupported_dimensions_is_refused() {
    let t = T::new().await;
    // Odd, too wide, too many pixels.
    for (w, h) in [(3441, 1440), (8194, 2), (8192, 4096)] {
        let monitors = format!(
            r#"[{{"id":0,"name":"DP-1","description":"x","width":{w},"height":{h},"refreshRate":60.0,"scale":1.0,"focused":true,"dpmsStatus":true,"activeWorkspace":{{"id":1,"name":"1"}}}}]"#
        );
        t.set_monitors(&monitors);
        let (c, m) = err(t.run(ID, "DP-1", 1).await);
        assert_eq!(
            (c, m.as_str()),
            (code::RECORDING_DIMENSIONS, text::RECORDING_DIMENSIONS)
        );
    }
    assert!(t.row(ID).await.is_none());
}

#[tokio::test]
async fn a_station_without_the_recorder_tool_says_so() {
    let t = T::with(Rig::new(Opts::default()), None).await;
    let (c, m) = err(t.run(ID, "DP-1", 1).await);
    assert_eq!(
        (c, m.as_str()),
        (desktop_code::UNSUPPORTED, text::NO_RECORDER)
    );
    assert!(t.row(ID).await.is_none());
}

#[tokio::test]
async fn too_little_free_space_refuses_before_the_row() {
    let t = T::new().await;
    t.low.store(true, std::sync::atomic::Ordering::SeqCst);
    let (c, m) = err(t.run(ID, "DP-1", 1).await);
    assert_eq!((c, m.as_str()), (code::DISK_FULL, text::DISK_FULL));
    assert!(t.row(ID).await.is_none());
    assert_eq!(t.leftovers(), (Vec::new(), Vec::new()));
}

#[tokio::test]
async fn a_second_capture_is_busy_while_a_recording_runs() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    let (c, m) = err(t.run(ID2, "DP-1", 1).await);
    assert_eq!((c, m.as_str()), (code::BUSY, text::BUSY));
    assert!(t.row(ID2).await.is_none());
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    run.await.unwrap().unwrap();
    // The slot is free again.
    assert!(t.run(ID2, "DP-1", 1).await.is_ok());
}

#[tokio::test]
async fn a_recording_is_busy_while_a_screenshot_holds_the_slot() {
    let t = T::new().await;
    let guard = t.desktop().hold_screenshot();
    assert_eq!(err(t.run(ID, "DP-1", 1).await).0, code::BUSY);
    drop(guard);
    assert!(t.run(ID, "DP-1", 1).await.is_ok());
}

#[tokio::test]
async fn a_repeated_request_answers_from_the_row_and_another_one_conflicts() {
    let t = T::new().await;
    let first = t
        .run(ID, "DP-1", 1)
        .await
        .unwrap()
        .capture
        .unwrap()
        .published;
    let again = t
        .run(ID, "DP-1", 1)
        .await
        .unwrap()
        .capture
        .unwrap()
        .published;
    assert_eq!(again.artifact.artifact_id, first.artifact.artifact_id);
    assert_eq!(t.recorder_runs(), 1);
    // Another monitor or other seconds with the same ID conflict.
    let (c, _) = err(t.run(ID, "HDMI-A-1", 1).await);
    assert_eq!(c, code::CAPTURE_ID_CONFLICT);
    let (c, _) = err(t.run(ID, "DP-1", 2).await);
    assert_eq!(c, code::CAPTURE_ID_CONFLICT);
    assert_eq!(t.recorder_runs(), 1);
}

#[tokio::test]
async fn a_failed_recording_answers_its_stored_code_when_repeated() {
    let t = T::new().await;
    t.flag("exit_early");
    assert_eq!(err(t.run(ID, "DP-1", 60).await).0, code::RECORDING_FAILED);
    let (c, m) = err(t.run(ID, "DP-1", 60).await);
    assert_eq!(
        (c, m.as_str()),
        (code::RECORDING_FAILED, text::RECORDING_FAILED)
    );
    assert_eq!(t.recorder_runs(), 1);
}

#[tokio::test]
async fn the_default_timing_is_the_design_timing() {
    let d = super::Timing::default();
    assert_eq!(d.tick.as_secs(), 1);
    assert_eq!(d.request.as_secs(), 5);
    assert_eq!(d.validity.as_secs(), 6);
    assert_eq!(d.grace.as_secs(), 3);
    assert_eq!(d.probe.as_secs(), 1);
    assert_eq!(d.slack.as_secs(), 30);
    assert!(timing().validity > timing().tick);
}
