//! The probe of a recording: what counts as a change, an unknown lock state,
//! the swapped size of a rotated monitor, and the probe of the policy
//! answer on the fake desktop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::desktop::MonitorInfo;
use fleetdeck_proto::recording::StopReason;

use super::probe::{Snapshot, verdict};
use super::rig_tests::T;
use crate::desktop::info::RawMonitor;
use crate::desktop::{Fail, code};

fn snapshot() -> Snapshot {
    Snapshot {
        signature: "sig_a_1".to_owned(),
        monitor: RawMonitor {
            info: MonitorInfo {
                name: "DP-1".to_owned(),
                description: String::new(),
                width: 3440,
                height: 1440,
                scale_pct: 100,
                refresh_hz: 60,
                focused: true,
                dpms_on: Some(true),
            },
            active_workspace: Some(1),
            x: 0,
            y: 0,
            transform: 0,
        },
        locked: Some(false),
    }
}

#[test]
fn an_unchanged_unlocked_monitor_is_fine() {
    let base = snapshot();
    assert_eq!(verdict(&base, &Ok(snapshot())), None);
    // The scale and the workspace do not change what is recorded.
    let mut now = snapshot();
    now.monitor.info.scale_pct = 150;
    now.monitor.active_workspace = Some(5);
    assert_eq!(verdict(&base, &Ok(now)), None);
}

#[test]
fn a_changed_instance_or_geometry_is_a_monitor_change() {
    let base = snapshot();
    let mut cases = Vec::new();
    let mut s = snapshot();
    s.signature = "sig_b_2".to_owned();
    cases.push(s);
    for edit in [
        (|m: &mut RawMonitor| m.x = 1) as fn(&mut RawMonitor),
        |m| m.y = -1,
        |m| m.transform = 2,
        |m| m.info.width = 1920,
        |m| m.info.height = 900,
    ] {
        let mut s = snapshot();
        edit(&mut s.monitor);
        cases.push(s);
    }
    for now in cases {
        assert_eq!(verdict(&base, &Ok(now)), Some(StopReason::MonitorChanged));
    }
}

#[test]
fn a_failed_probe_is_a_safety_failure() {
    let base = snapshot();
    let failed = Err(Fail::new(code::FAILED, "no"));
    assert_eq!(verdict(&base, &failed), Some(StopReason::MonitorChanged));
}

#[test]
fn a_locked_or_unknown_lock_state_is_a_lock() {
    let base = snapshot();
    for locked in [Some(true), None] {
        let mut now = snapshot();
        now.locked = locked;
        assert_eq!(verdict(&base, &Ok(now)), Some(StopReason::ScreenLocked));
    }
}

#[test]
fn the_recorded_size_swaps_for_the_odd_transforms() {
    for (transform, want) in [
        (0, (3440, 1440)),
        (1, (1440, 3440)),
        (2, (3440, 1440)),
        (3, (1440, 3440)),
        (4, (3440, 1440)),
        (5, (1440, 3440)),
        (6, (3440, 1440)),
        (7, (1440, 3440)),
    ] {
        let mut s = snapshot();
        s.monitor.transform = transform;
        assert_eq!(s.recorded_size(), want, "transform {transform}");
    }
}

#[tokio::test]
async fn the_policy_probe_reads_the_fake_desktop() {
    let t = T::new().await;
    let limit = Duration::from_secs(1);
    assert_eq!(t.desktop().policy_probe("DP-1", limit).await, (true, true));
    assert_eq!(
        t.desktop().policy_probe("DP-9", limit).await,
        (false, false)
    );
    t.flag("hyprlock");
    assert_eq!(t.desktop().policy_probe("DP-1", limit).await, (true, false));
    t.set_monitors("[]");
    assert_eq!(
        t.desktop().policy_probe("DP-1", limit).await,
        (false, false)
    );
}
