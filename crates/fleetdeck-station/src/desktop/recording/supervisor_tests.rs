//! The stop triggers of a running recording (design §7.4, §7.5): each one
//! discards with its reason and its owner terminal code, and leaves no
//! artifact, no folder and no reservation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::capture::code;
use fleetdeck_proto::coordinator_desktop_text::code as p6;
use fleetdeck_proto::recording::{MAX_RECORDING_BYTES, StopReason};
use tokio::task::JoinHandle;

use super::rig_tests::{ID, T, err};
use crate::artifacts::capture_import::tests::FP;
use crate::desktop::Fail;
use crate::store::capture_ops::StopIntent;
use fleetdeck_proto::desktop::DesktopActResult;

type Run = JoinHandle<Result<DesktopActResult, Fail>>;

/// A long recording that is running.
async fn running(t: &T) -> Run {
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    run
}

/// The recording ends with `code` and `reason`, long before its 60 s.
async fn ends(t: &T, run: Run, want: &str, reason: StopReason) {
    let result = tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .expect("the recording did not stop")
        .unwrap();
    assert_eq!(err(result).0, want);
    t.assert_discarded(ID, want, reason).await;
}

#[tokio::test]
async fn the_station_policy_going_off_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.allowed.lock().unwrap().desktop = false;
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::PolicyOff).await;
}

#[tokio::test]
async fn the_capture_permission_going_off_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.allowed.lock().unwrap().capture = false;
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::PolicyOff).await;
}

#[tokio::test]
async fn a_monitor_allowlist_that_excludes_the_target_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.allowed.lock().unwrap().monitors = vec!["HDMI-A-1".to_owned()];
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::PolicyOff).await;
}

#[tokio::test]
async fn a_locked_screen_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.flag("hyprlock");
    ends(&t, run, code::SCREEN_LOCKED, StopReason::ScreenLocked).await;
}

#[tokio::test]
async fn an_unknown_lock_state_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    let pgrep = t.dir().join("bin/pgrep");
    std::fs::write(&pgrep, "#!/bin/sh\nexit 2\n").unwrap();
    std::fs::set_permissions(&pgrep, std::fs::Permissions::from_mode(0o755)).unwrap();
    ends(&t, run, code::SCREEN_LOCKED, StopReason::ScreenLocked).await;
}

#[tokio::test]
async fn a_removed_monitor_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.set_monitors("[]");
    ends(&t, run, p6::DESKTOP_NOT_FOUND, StopReason::MonitorChanged).await;
}

#[tokio::test]
async fn a_geometry_or_transform_change_discards() {
    for (from, to) in [
        ("\"focused\":true", "\"focused\":true,\"x\":10"),
        ("\"focused\":true", "\"focused\":true,\"y\":-4"),
        ("\"focused\":true", "\"focused\":true,\"transform\":3"),
        ("\"width\":3440", "\"width\":2560"),
        ("\"height\":1440", "\"height\":1080"),
    ] {
        let t = T::new().await;
        let run = running(&t).await;
        let monitors = std::fs::read_to_string(t.dir().join("monitors.json")).unwrap();
        t.set_monitors(&monitors.replacen(from, to, 1));
        ends(&t, run, p6::DESKTOP_NOT_FOUND, StopReason::MonitorChanged).await;
    }
}

#[tokio::test]
async fn a_new_compositor_instance_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    // A second, newer Hyprland instance: the signature changes.
    let runtime = t.fake.runtime.join("hypr");
    let newer = runtime.join("sig_b_2");
    std::fs::create_dir_all(&newer).unwrap();
    let listener = tokio::net::UnixListener::bind(newer.join(".socket.sock")).unwrap();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            drop(s);
        }
    });
    std::fs::write(
        t.dir().join("instances.json"),
        r#"[{"instance":"sig_b_2","time":2,"pid":2,"wl_socket":"wayland-1"}]"#,
    )
    .unwrap();
    ends(&t, run, p6::DESKTOP_NOT_FOUND, StopReason::MonitorChanged).await;
}

#[tokio::test]
async fn a_link_that_closes_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.peer.close();
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::LinkClosed).await;
}

#[tokio::test]
async fn a_link_that_is_no_longer_current_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.peer.supersede();
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::LinkClosed).await;
}

#[tokio::test]
async fn a_file_at_the_ceiling_discards_as_too_large() {
    let t = T::new().await;
    let run = running(&t).await;
    std::fs::OpenOptions::new()
        .write(true)
        .open(t.staging_file(ID))
        .unwrap()
        .set_len(MAX_RECORDING_BYTES)
        .unwrap();
    ends(&t, run, code::RECORDING_TOO_LARGE, StopReason::OutputLimit).await;
}

#[tokio::test]
async fn a_lost_free_space_reserve_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.low.store(true, Ordering::SeqCst);
    ends(&t, run, code::DISK_FULL, StopReason::DiskReserve).await;
}

#[tokio::test]
async fn the_station_shutdown_discards() {
    let t = T::new().await;
    let run = running(&t).await;
    t.desktop().shutdown_recordings();
    ends(
        &t,
        run,
        code::RECORDING_INTERRUPTED,
        StopReason::StationShutdown,
    )
    .await;
}

#[tokio::test]
async fn the_total_deadline_discards() {
    let t = T::new().await;
    // No time beyond the requested duration: the total deadline comes first.
    t.set_timing(|timing| timing.slack = Duration::ZERO);
    let run = t.start(ID, "DP-1", 2);
    ends(&t, run, p6::DESKTOP_TIMEOUT, StopReason::Deadline).await;
}

#[tokio::test]
async fn a_suspend_discards_as_an_expired_lease() {
    let t = T::new().await;
    let run = running(&t).await;
    t.clocks.jump(5000);
    ends(
        &t,
        run,
        code::RECORDING_INTERRUPTED,
        StopReason::LeaseExpired,
    )
    .await;
}

#[tokio::test]
async fn the_first_discard_wins_over_a_later_save_and_a_later_discard() {
    let t = T::new().await;
    // A recorder that ignores SIGINT keeps running, so the later requests
    // really arrive after the first discard was latched.
    t.flag("ignore_int");
    let run = running(&t).await;
    let stop = |reason| t.desktop().stop_recording(t.store(), FP, ID, reason);
    stop(StopReason::DeckPolicy).await.unwrap();
    stop(StopReason::OwnerStop).await.unwrap();
    stop(StopReason::ArtifactDeleted).await.unwrap();
    ends(&t, run, code::RECORDING_INTERRUPTED, StopReason::DeckPolicy).await;
}

#[tokio::test]
async fn a_discard_after_an_owner_stop_still_discards() {
    let t = T::new().await;
    t.flag("ignore_int");
    t.set_timing(|timing| timing.grace = Duration::from_secs(10));
    let run = running(&t).await;
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    // The recorder ignores SIGINT: the owner stop is still pending.
    assert_eq!(t.row(ID).await.unwrap().stop_intent, StopIntent::Save);
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerLoginEnded)
        .await
        .unwrap();
    ends(
        &t,
        run,
        code::RECORDING_INTERRUPTED,
        StopReason::OwnerLoginEnded,
    )
    .await;
    assert_eq!(t.recorder_runs(), 1);
}

#[tokio::test]
async fn a_trigger_during_the_finalize_discards_before_the_commit() {
    // The import lease is slow: a discard that arrives meanwhile wins.
    let t = T::new().await;
    t.set_timing(|timing| timing.request = Duration::from_secs(3));
    t.peer.set_phase(
        fleetdeck_proto::recording::LeasePhase::Import,
        super::rig_tests::Reply::Delay(800, Box::new(super::rig_tests::Reply::Continue)),
    );
    let run = t.start(ID, "DP-1", 1);
    // Wait for the import request, then discard.
    for _ in 0..400 {
        if t.peer.count(fleetdeck_proto::recording::LeasePhase::Import) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::ArtifactDeleted)
        .await
        .unwrap();
    ends(
        &t,
        run,
        code::RECORDING_INTERRUPTED,
        StopReason::ArtifactDeleted,
    )
    .await;
}
