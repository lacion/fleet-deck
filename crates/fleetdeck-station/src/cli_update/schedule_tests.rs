//! The schedule (design §3.9): the window, the jitter, one attempt per
//! window, the busy row, the quarantine and the external changes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::SessionId;
use fleetdeck_proto::cli_update::InstallMethod;
use fleetdeck_testkit::det::EPOCH_MS;

use super::super::run::tests::{FROM, Rig, TARGET, rig_with, test_config};
use super::*;
use crate::store::cli_update_journal::NewJournal;

const MIN: i64 = 60_000;

/// A config with the window `start_min` for `hours` hours.
fn window_cfg(start_min: u16, hours: u8) -> ProviderUpdatesConfig {
    ProviderUpdatesConfig {
        window_start_min: start_min,
        window_hours: hours,
        timezone: UpdateZone::Fixed(0),
        ..test_config()
    }
}

/// Unix ms of day `day`, `hh:mm` local time with `offset` minutes east.
fn at(day: i64, hh: i64, mm: i64, offset: i64) -> i64 {
    (day * 1440 + hh * 60 + mm - offset) * MIN
}

#[test]
fn the_window_follows_a_fixed_offset() {
    let cfg = window_cfg(3 * 60, 2);
    // 03:30 local at +02:00 is 01:30 UTC.
    let w = window_at(&cfg, 120, at(20_000, 3, 30, 120)).unwrap();
    assert_eq!(w.id, 20_000);
    assert_eq!(w.elapsed_min, 30);
    assert_eq!(w.start_utc_ms, at(20_000, 3, 0, 120));
    assert!(window_at(&cfg, 120, at(20_000, 2, 59, 120)).is_none());
    assert!(window_at(&cfg, 120, at(20_000, 5, 0, 120)).is_none());
    assert!(window_at(&cfg, 120, at(20_000, 4, 59, 120)).is_some());
    // The same instant in UTC is outside.
    assert!(window_at(&cfg, 0, at(20_000, 1, 30, 0)).is_none());
    // A negative offset.
    let w = window_at(&cfg, -300, at(20_000, 3, 10, -300)).unwrap();
    assert_eq!(w.elapsed_min, 10);
}

#[test]
fn a_window_may_cross_midnight() {
    let cfg = window_cfg(23 * 60, 3);
    let late = window_at(&cfg, 0, at(20_000, 23, 30, 0)).unwrap();
    assert_eq!(late.id, 20_000);
    assert_eq!(late.elapsed_min, 30);
    let early = window_at(&cfg, 0, at(20_001, 0, 30, 0)).unwrap();
    assert_eq!(early.id, 20_000);
    assert_eq!(early.elapsed_min, 90);
    assert_eq!(early.start_utc_ms, late.start_utc_ms);
    assert!(window_at(&cfg, 0, at(20_001, 2, 0, 0)).is_none());
    assert!(window_at(&cfg, 0, at(20_001, 12, 0, 0)).is_none());
}

#[test]
fn the_jitter_stays_below_the_bound() {
    let two = window_cfg(180, 2);
    let one = window_cfg(180, 1);
    for name in ["Lighting", "Laptop", "a", "", "Büro", "station-12"] {
        assert!(jitter_min(name, &two) < 60, "{name}");
        assert!(jitter_min(name, &one) < 30, "{name}");
    }
    assert_eq!(jitter_min("Lighting", &two), jitter_min("Lighting", &two));
}

#[test]
fn date_output_is_parsed_to_minutes() {
    assert_eq!(parse_offset("+0200\n"), Some(120));
    assert_eq!(parse_offset("-0530"), Some(-330));
    assert_eq!(parse_offset("+0000"), Some(0));
    assert_eq!(parse_offset("0200"), None);
    assert_eq!(parse_offset("+02:00"), None);
    assert_eq!(parse_offset("+1500"), None);
    assert_eq!(parse_offset(""), None);
}

/// A rig with the window 00:00 to 02:00 UTC (the test clock is 00:00 UTC).
async fn windowed(kind: ProviderKind, enabled: bool) -> (Rig, Schedule) {
    let r = rig_with(kind, enabled).await;
    *r.config.0.lock().unwrap() = window_cfg(0, 2);
    let schedule = Schedule::new(Arc::clone(&r.engine), "Lighting".to_owned());
    (r, schedule)
}

/// `minutes` after the start of the window of the test clock.
fn tick_at(minutes: i64) -> Now {
    Now {
        utc_ms: i64::try_from(EPOCH_MS).unwrap() + minutes * MIN,
        offset_min: Some(0),
    }
}

async fn attempt_count(r: &Rig) -> usize {
    r.store.cli_update_attempts(0, 100).await.unwrap().len()
}

fn after_jitter() -> i64 {
    jitter_min("Lighting", &window_cfg(0, 2)) + 1
}

#[tokio::test]
async fn one_scheduled_attempt_runs_in_a_window() {
    let (r, schedule) = windowed(ProviderKind::Claude, true).await;
    schedule.tick(tick_at(after_jitter())).await;
    r.settle().await;
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::Updated);
    assert_eq!(r.last().await.unwrap().trigger, UpdateTrigger::Schedule);
    *r.host.installed.lock().unwrap() = Some(TARGET.to_owned());
    schedule.tick(tick_at(after_jitter() + 15)).await;
    r.settle().await;
    assert_eq!(attempt_count(&r).await, 1);
}

#[tokio::test]
async fn nothing_runs_before_the_jitter_or_outside_the_window() {
    let (r, schedule) = windowed(ProviderKind::Claude, true).await;
    if after_jitter() > 1 {
        schedule.tick(tick_at(0)).await;
    }
    schedule.tick(tick_at(3 * 60)).await;
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(r.last().await.is_none());
}

#[tokio::test]
async fn a_busy_window_writes_one_skipped_busy_row_after_the_window() {
    let (r, schedule) = windowed(ProviderKind::Claude, true).await;
    r.host
        .live
        .lock()
        .unwrap()
        .push(SessionId::from("s1".to_owned()));
    schedule.tick(tick_at(after_jitter())).await;
    schedule.tick(tick_at(after_jitter() + 15)).await;
    assert!(r.last().await.is_none(), "no row while the window is open");
    schedule.tick(tick_at(3 * 60)).await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::SkippedBusy);
    assert_eq!(last.reason.as_deref(), Some(reason::SESSIONS_RUNNING));
    assert_eq!(last.trigger, UpdateTrigger::Schedule);
    schedule.tick(tick_at(3 * 60 + 15)).await;
    assert_eq!(attempt_count(&r).await, 1);
}

#[tokio::test]
async fn a_quarantined_provider_is_skipped() {
    let (r, schedule) = windowed(ProviderKind::Claude, true).await;
    r.quarantine_now().await;
    schedule.tick(tick_at(after_jitter())).await;
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(attempt_count(&r).await, 1, "only the quarantining row");
}

#[tokio::test]
async fn a_provider_with_a_journal_row_is_skipped() {
    let (r, schedule) = windowed(ProviderKind::Claude, true).await;
    let a = new_attempt(
        "j1",
        r.kind,
        UpdateTrigger::Owner,
        InstallMethod::Native,
        "stable",
        Some(FROM),
        Some(TARGET),
    );
    let j = NewJournal {
        provider: r.kind,
        method: InstallMethod::Native,
        from_version: FROM.to_owned(),
        target_version: TARGET.to_owned(),
        launcher: "/x".into(),
        dest: "/x".into(),
        stage_root: r.dir.path().join("none"),
        rollback_sha256: None,
    };
    r.store.cli_update_begin(a, j).await.unwrap();
    schedule.tick(tick_at(after_jitter())).await;
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_external_version_change_writes_one_external_row_and_refreshes() {
    let (r, schedule) = windowed(ProviderKind::Claude, false).await;
    r.installer
        .versions
        .lock()
        .unwrap()
        .insert(ProviderKind::Claude, TARGET.to_owned());
    let no_window = Now {
        utc_ms: 0,
        offset_min: None,
    };
    schedule.tick(no_window).await;
    let last = r.last().await.unwrap();
    assert_eq!(last.trigger, UpdateTrigger::External);
    assert_eq!(last.outcome, UpdateOutcome::Updated);
    assert_eq!(last.reason.as_deref(), Some(reason::OUTSIDE_FLEET_DECK));
    assert_eq!(last.from_version.as_deref(), Some(FROM));
    assert_eq!(last.to_version.as_deref(), Some(TARGET));
    assert!(r.events().contains(&"refresh".to_owned()));
    // One try for one changed version.
    schedule.tick(no_window).await;
    assert_eq!(attempt_count(&r).await, 1);
}

#[tokio::test]
async fn updates_off_run_no_attempt_but_still_record_external_changes() {
    let (r, schedule) = windowed(ProviderKind::Claude, false).await;
    schedule.tick(tick_at(after_jitter())).await;
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(r.last().await.is_none());
    r.installer
        .versions
        .lock()
        .unwrap()
        .insert(ProviderKind::Claude, TARGET.to_owned());
    schedule.tick(tick_at(after_jitter() + 15)).await;
    assert_eq!(r.last().await.unwrap().trigger, UpdateTrigger::External);
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_external_change_of_a_quarantined_provider_runs_verify_current() {
    let (r, schedule) = windowed(ProviderKind::Claude, false).await;
    r.quarantine_now().await;
    r.installer
        .versions
        .lock()
        .unwrap()
        .insert(ProviderKind::Claude, TARGET.to_owned());
    let none = Now {
        utc_ms: 0,
        offset_min: None,
    };
    schedule.tick(none).await;
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Verified);
    assert_eq!(last.trigger, UpdateTrigger::External);
    assert!(r.quarantine().await.is_none());
}

#[tokio::test]
async fn a_failed_check_of_a_changed_quarantined_provider_writes_no_row_and_tries_once() {
    let (r, schedule) = windowed(ProviderKind::Claude, false).await;
    r.quarantine_now().await;
    r.installer
        .versions
        .lock()
        .unwrap()
        .insert(ProviderKind::Claude, TARGET.to_owned());
    r.installer
        .handshake_ok
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let none = Now {
        utc_ms: 0,
        offset_min: None,
    };
    schedule.tick(none).await;
    schedule.tick(none).await;
    assert_eq!(attempt_count(&r).await, 1, "only the quarantining row");
    assert!(r.quarantine().await.is_some());
    assert_eq!(r.channel.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
