//! The dispatch of `desktop.act` on a fake desktop (R13 P3b): the start
//! check before the first program, the quiet-hours re-check, the total
//! deadlines, the fresh info and the local offset.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_link::desktop_authority::issue;
use fleetdeck_proto::desktop::DesktopAction as A;

use super::authority::{EXPIRED, QUIET_ACTIVE};
use super::authority_tests::{EPOCH, Harness, NODE, id};
use super::deadline::{ACT_TIMEOUT, INFO_TIMEOUT, Limits};
use super::fake::{Opts, Rig, call};
use super::quiet;
use super::tests::{on, url};

/// An authority for `action` with quiet hours.
fn with_quiet(h: &Harness, action: &A, n: u64, quiet: &str) -> super::authority::Admitted {
    let a = issue(
        action,
        NODE,
        id(n),
        EPOCH.to_owned(),
        h.now(),
        Some(quiet.to_owned()),
    );
    h.admit(action, Some(a)).unwrap()
}

fn monitor_reads(rig: &Rig) -> usize {
    rig.reads()
        .iter()
        .filter(|c| c.program == "hyprctl" && c.args == ["-j", "monitors"])
        .count()
}

fn dnd() -> A {
    A::ScreenDnd { on: true }
}

fn show() -> A {
    A::WorkspaceShow {
        monitor: "DP-1".into(),
        workspace: 2,
    }
}

#[tokio::test]
async fn an_authorized_action_runs() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&dnd(), 1);
    rig.desktop.act(&on, dnd(), admitted).await.unwrap();
    assert_eq!(
        rig.calls(),
        vec![call("makoctl", &["mode", "-a", "do-not-disturb"])]
    );
}

#[tokio::test]
async fn an_expired_authority_runs_no_program() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&show(), 1);
    // The wall clock passes the deadline before the first program.
    h.wall.fetch_add(10_001, Ordering::SeqCst);
    let e = rig.desktop.act(&on, show(), admitted).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", EXPIRED));
    assert!(rig.calls().is_empty());
    assert_eq!(monitor_reads(&rig), 0, "not even the monitor read");
}

#[tokio::test]
async fn the_monotonic_limit_stops_an_action_that_waited_too_long() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&dnd(), 1);
    h.advance_mono(10_001);
    let e = rig.desktop.act(&on, dnd(), admitted).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", EXPIRED));
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn allow_desktop_off_after_the_admission_runs_no_program() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&show(), 1);
    let off = || false;
    let e = rig.desktop.act(&off, show(), admitted).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(rig.calls().is_empty());
    assert_eq!(monitor_reads(&rig), 0);
}

#[tokio::test]
async fn a_connection_that_is_no_longer_current_runs_no_program() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&show(), 1);
    h.current.store(false, Ordering::SeqCst);
    let e = rig.desktop.act(&on, show(), admitted).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(rig.calls().is_empty());
    assert_eq!(monitor_reads(&rig), 0);
}

#[tokio::test]
async fn an_authority_for_another_action_runs_no_program() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let admitted = h.admitted(&dnd(), 1);
    let e = rig
        .desktop
        .act(&on, A::ScreenLock, admitted)
        .await
        .unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn a_wait_for_the_display_lock_counts_against_the_deadline() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let open = |display: &str| A::WallOpen {
        display_id: display.into(),
        monitor: "DP-1".into(),
        workspace: None,
        url: url(),
    };
    rig.desktop
        .act(&on, open("wall-1"), h.admitted(&open("wall-1"), 1))
        .await
        .unwrap();
    // The close request is held, so the close keeps the lock of the display.
    rig.flag("hold_close", true);
    let close = A::WallClose {
        display_id: "wall-1".into(),
    };
    let admitted = h.admitted(&close, 2);
    let desktop = std::sync::Arc::clone(&rig.desktop);
    let closing = tokio::spawn(async move { desktop.act(&on, close, admitted).await });
    let dir = rig.dir.path().to_owned();
    let requests =
        move || std::fs::read_to_string(dir.join("close.req")).map_or(0, |t| t.lines().count());
    for _ in 0..200 {
        if requests() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(requests(), 1);
    // The second open waits for the lock; the 10 s of monotonic time pass.
    let second = open("wall-1");
    let admitted = h.admitted(&second, 3);
    let desktop = std::sync::Arc::clone(&rig.desktop);
    let opening = tokio::spawn(async move { desktop.act(&on, second, admitted).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!opening.is_finished());
    h.advance_mono(10_001);
    rig.flag("hold_close", false);
    closing.await.unwrap().unwrap();
    let e = opening.await.unwrap().unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", EXPIRED));
    assert_eq!(rig.calls_of("chromium").len(), 1, "no second launch");
}

#[tokio::test]
async fn quiet_hours_stop_an_action_before_any_program() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    // 23:00 UTC at +0000: inside 22:00-07:00.
    h.wall.store(1_704_150_000_000, Ordering::SeqCst);
    rig.set_offset("+0000\n");
    let admitted = with_quiet(&h, &show(), 1, "22:00-07:00");
    let e = rig.desktop.act(&on, show(), admitted).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", QUIET_ACTIVE));
    assert!(rig.calls().is_empty());
    assert_eq!(monitor_reads(&rig), 0);
    assert!(
        rig.reads().iter().any(|c| c.program == "date"),
        "the offset is read again"
    );
}

#[tokio::test]
async fn quiet_hours_use_the_local_time_of_the_station() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    h.wall.store(1_704_150_000_000, Ordering::SeqCst);
    // 23:00 UTC at +0900 is 08:00: outside 22:00-07:00.
    rig.set_offset("+0900\n");
    let admitted = with_quiet(&h, &dnd(), 1, "22:00-07:00");
    rig.desktop.act(&on, dnd(), admitted).await.unwrap();
    assert_eq!(rig.calls_of("makoctl").len(), 1);
    // 23:00 UTC at -0100 is 22:00: the start is inside.
    rig.set_offset("-0100\n");
    let admitted = with_quiet(&h, &dnd(), 2, "22:00-07:00");
    let e = rig.desktop.act(&on, dnd(), admitted).await.unwrap_err();
    assert_eq!(e.message, QUIET_ACTIVE);
    assert_eq!(rig.calls_of("makoctl").len(), 1);
}

#[tokio::test]
async fn equal_endpoints_are_no_quiet_hours_and_an_unreadable_offset_fails_closed() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    h.wall.store(1_704_150_000_000, Ordering::SeqCst);
    rig.set_offset("+0000\n");
    let admitted = with_quiet(&h, &dnd(), 1, "23:00-23:00");
    rig.desktop.act(&on, dnd(), admitted).await.unwrap();
    assert_eq!(rig.calls_of("makoctl").len(), 1);
    // The offset cannot be read: the action does not run.
    rig.flag("fail_date", true);
    let admitted = with_quiet(&h, &dnd(), 2, "10:00-11:00");
    let e = rig.desktop.act(&on, dnd(), admitted).await.unwrap_err();
    assert_eq!(e.message, QUIET_ACTIVE);
    // Without quiet hours the offset does not matter.
    let admitted = h.admitted(&dnd(), 3);
    rig.desktop.act(&on, dnd(), admitted).await.unwrap();
    assert_eq!(rig.calls_of("makoctl").len(), 2);
}

#[tokio::test]
async fn an_action_that_passes_its_total_deadline_fails_with_a_fixed_text() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let open = A::WallOpen {
        display_id: "wall-1".into(),
        monitor: "DP-1".into(),
        workspace: None,
        url: url(),
    };
    rig.desktop
        .act(&on, open.clone(), h.admitted(&open, 1))
        .await
        .unwrap();
    // The window never goes away, so the close waits for it.
    rig.flag("hold_close", true);
    rig.desktop.set_limits(Limits {
        long: Duration::from_secs(90),
        short: Duration::from_millis(300),
    });
    let close = A::WallClose {
        display_id: "wall-1".into(),
    };
    let e = rig
        .desktop
        .act(&on, close.clone(), h.admitted(&close, 2))
        .await
        .unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", ACT_TIMEOUT));
}

#[tokio::test]
async fn info_has_a_total_deadline_too() {
    let rig = Rig::new(Opts::default());
    rig.desktop.set_limits(Limits {
        long: Duration::from_secs(90),
        short: Duration::from_millis(1),
    });
    let e = rig.desktop.info(&on).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", INFO_TIMEOUT));
    let e = rig.desktop.info_fresh(&on).await.unwrap_err();
    assert_eq!(e.message, INFO_TIMEOUT);
}

#[tokio::test]
async fn fresh_info_ignores_the_cache() {
    let rig = Rig::new(Opts::default());
    rig.desktop.info(&on).await.unwrap();
    assert_eq!(monitor_reads(&rig), 1);
    rig.desktop.info(&on).await.unwrap();
    assert_eq!(monitor_reads(&rig), 1, "cached");
    rig.desktop.info_fresh(&on).await.unwrap();
    assert_eq!(monitor_reads(&rig), 2, "fresh");
    // The live parts follow the gate.
    let off = || false;
    assert!(!rig.desktop.info_fresh(&off).await.unwrap().allowed.desktop);
}

#[tokio::test]
async fn the_info_carries_the_local_offset_and_the_sample_time() {
    let rig = Rig::new(Opts::default());
    rig.set_offset("-0530\n");
    let before = quiet::now_ms();
    let info = rig.desktop.info(&on).await.unwrap();
    let after = quiet::now_ms();
    assert_eq!(info.utc_offset_minutes, Some(-330));
    let sampled = info.offset_sampled_at.unwrap();
    assert!(
        (before..=after).contains(&sampled),
        "{before} {sampled} {after}"
    );
    // The cache keeps the sample; a fresh call reads the offset again.
    rig.set_offset("+0200\n");
    let cached = rig.desktop.info(&on).await.unwrap();
    assert_eq!(cached.utc_offset_minutes, Some(-330));
    assert_eq!(cached.offset_sampled_at, Some(sampled));
    let fresh = rig.desktop.info_fresh(&on).await.unwrap();
    assert_eq!(fresh.utc_offset_minutes, Some(120));
    assert!(fresh.offset_sampled_at.unwrap() >= sampled);
}

#[tokio::test]
async fn an_offset_that_cannot_be_read_is_none_for_both_fields() {
    let rig = Rig::new(Opts::default());
    rig.flag("fail_date", true);
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(
        (info.utc_offset_minutes, info.offset_sampled_at),
        (None, None)
    );
    rig.flag("fail_date", false);
    for bad in ["UTC\n", "+02:00\n", "+2400\n", "\n", "+0200 x\n"] {
        rig.set_offset(bad);
        let info = rig.desktop.info_fresh(&on).await.unwrap();
        assert_eq!(
            (info.utc_offset_minutes, info.offset_sampled_at),
            (None, None),
            "{bad:?}"
        );
    }
    // No `date` at all.
    let rig = Rig::without(Opts::default(), &["date"]);
    let info = rig.desktop.info(&on).await.unwrap();
    assert_eq!(
        (info.utc_offset_minutes, info.offset_sampled_at),
        (None, None)
    );
}
