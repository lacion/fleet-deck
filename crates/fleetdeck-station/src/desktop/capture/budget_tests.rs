//! The budgets (design §4): one capture, the free space, the monitor size.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::desktop::MonitorInfo;

use super::{CaptureSlot, FREE_RESERVE, check_monitor, room};

fn monitor(width: u32, height: u32) -> MonitorInfo {
    MonitorInfo {
        name: "DP-1".into(),
        description: String::new(),
        width,
        height,
        scale_pct: 100,
        refresh_hz: 60,
        focused: true,
        dpms_on: Some(true),
    }
}

#[test]
fn a_second_capture_is_busy_at_once() {
    let slot = CaptureSlot::default();
    let guard = slot.try_take().unwrap();
    let e = slot.try_take().unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (code::BUSY, text::BUSY));
    drop(guard);
}

#[test]
fn the_slot_frees_on_drop() {
    let slot = CaptureSlot::default();
    drop(slot.try_take().unwrap());
    let again = slot.try_take();
    assert!(again.is_ok());
}

#[test]
fn the_monitor_limits() {
    assert!(check_monitor(&monitor(1920, 1080)).is_ok());
    assert!(check_monitor(&monitor(8192, 2048)).is_ok());
    assert!(check_monitor(&monitor(1, 1)).is_ok());
    for (w, h) in [(0, 1080), (1920, 0), (8193, 100), (100, 8193), (8192, 2049)] {
        let e = check_monitor(&monitor(w, h)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (code::CAPTURE_DIMENSIONS, text::CAPTURE_DIMENSIONS),
            "{w}x{h}"
        );
    }
}

#[test]
fn room_needs_the_reserve_plus_the_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = rustix::fs::openat(
        rustix::fs::CWD,
        tmp.path(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    // A request for more than any disk has never fits.
    let e = room(&dir, u64::MAX - FREE_RESERVE).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (code::DISK_FULL, text::DISK_FULL)
    );
    let stat = rustix::fs::fstatvfs(&dir).unwrap();
    let available = stat.f_bavail * stat.f_frsize;
    if available > FREE_RESERVE + (1 << 30) {
        assert!(room(&dir, 0).is_ok());
    }
}
