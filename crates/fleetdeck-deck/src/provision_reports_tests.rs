//! Tests of the setup report store and of the reporting window (BG-3,
//! `docs/v2/design/board-gaps.md` section 4): the capacity of 64 rows without
//! eviction, the retention of 24 hours, the rate of five minutes, the retry
//! permission that is bound to one report and delivered once, and the global
//! limit of 120 accepted updates in a sliding hour.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use fleetdeck_core::identity::{Fingerprint, Identity};
use fleetdeck_link::hello::ReportStatus;
use fleetdeck_link::report_accept::{ReportSink, ValidReport};
use fleetdeck_testkit::Det;

use super::{ProvisionReports, retry_allowed};
use crate::store::DeckStore;
use crate::store::provision_reports::{
    AllowRetry, Allowed, Cancelled, MAX_REPORTS, Outcome, RETENTION_MS, RETRY_TTL_MS,
};

const MIN: i64 = 60 * 1000;
const T0: i64 = 1_000_000_000;

async fn store() -> (DeckStore, Det) {
    let det = Det::at(u64::try_from(T0).unwrap());
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    (store, det)
}

fn key() -> Fingerprint {
    Identity::generate().unwrap().fingerprint()
}

fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

fn terminal(c: char) -> String {
    c.to_string().repeat(32)
}

fn report(t: char, code: &str, name: &str) -> ValidReport {
    ValidReport::validate(&terminal(t), &digest(t), code, "validated", name).unwrap()
}

fn allow(fp: &Fingerprint, t: char, request: &str, now: i64) -> AllowRetry {
    AllowRetry {
        fingerprint: fp.to_string(),
        terminal_id: terminal(t),
        state_digest: digest(t),
        request_id: request.to_owned(),
        retry_id: format!("{:032x}", now),
        now,
        code_allowed: retry_allowed,
    }
}

async fn apply(s: &DeckStore, fp: &Fingerprint, r: ValidReport, now: i64) -> Outcome {
    s.apply_provision_report(fp.to_string(), r, now)
        .await
        .unwrap()
        .outcome
}

#[tokio::test]
async fn provision_report_capacity_is_64_rows_and_nothing_is_evicted() {
    let (s, _det) = store().await;
    let keys: Vec<Fingerprint> = (0..64).map(|_| key()).collect();
    for k in &keys {
        assert_eq!(
            apply(&s, k, report('a', "denied", "n"), T0).await,
            Outcome::Stored
        );
    }
    let extra = key();
    assert_eq!(
        apply(&s, &extra, report('a', "denied", "n"), T0 + 1).await,
        Outcome::Capacity
    );
    let rows = s.provision_reports_list(T0 + 1).await.unwrap();
    assert_eq!(i64::try_from(rows.len()).unwrap(), MAX_REPORTS);
    assert!(rows.iter().all(|r| r.fingerprint != extra.to_string()));
    // A known key still updates when the table is full.
    assert_eq!(
        apply(&s, &keys[0], report('b', "denied", "n2"), T0 + 6 * MIN).await,
        Outcome::Stored
    );
}

#[tokio::test]
async fn provision_report_rows_older_than_24_hours_are_deleted() {
    let (s, _det) = store().await;
    let (old, fresh) = (key(), key());
    apply(&s, &old, report('a', "denied", "old"), T0).await;
    apply(
        &s,
        &fresh,
        report('a', "denied", "fresh"),
        T0 + RETENTION_MS - MIN,
    )
    .await;
    // Just before the limit both rows live.
    let rows = s
        .provision_reports_list(T0 + RETENTION_MS - 1)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    // The retention counts from the last contact: the old row goes first.
    let rows = s
        .provision_reports_list(T0 + RETENTION_MS + 1)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "fresh");
    // A new report deletes the expired rows too, and frees a slot.
    let later = T0 + 2 * RETENTION_MS;
    assert_eq!(
        apply(&s, &key(), report('a', "denied", "x"), later).await,
        Outcome::Stored
    );
    assert_eq!(s.provision_reports_list(later).await.unwrap().len(), 1);
}

#[tokio::test]
async fn provision_report_contact_inside_five_minutes_changes_only_last_contact() {
    let (s, _det) = store().await;
    let fp = key();
    assert_eq!(
        apply(&s, &fp, report('a', "denied", "first"), T0).await,
        Outcome::Stored
    );
    // 60 seconds later: 240 seconds remain; nothing else is replaced.
    let again = apply(&s, &fp, report('b', "pairing_failed", "second"), T0 + MIN).await;
    assert_eq!(again, Outcome::RateLimited(240));
    let row = s.provision_reports_list(T0 + MIN).await.unwrap().remove(0);
    assert_eq!(
        (
            row.name.as_str(),
            row.code.as_str(),
            row.received_at,
            row.last_contact
        ),
        ("first", "denied", T0, T0 + MIN)
    );
    assert_eq!(row.terminal_id, terminal('a'));
    // Five minutes after the receipt the report is rewritten.
    let again = apply(
        &s,
        &fp,
        report('a', "pairing_failed", "third"),
        T0 + 5 * MIN,
    )
    .await;
    assert_eq!(again, Outcome::Stored);
    let row = s
        .provision_reports_list(T0 + 5 * MIN)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.name.as_str(), row.code.as_str(), row.received_at),
        ("third", "pairing_failed", T0 + 5 * MIN)
    );
}

#[tokio::test]
async fn provision_report_a_changed_binding_deletes_the_retry_permission() {
    let (s, _det) = store().await;
    let fp = key();
    apply(&s, &fp, report('a', "denied", "n"), T0).await;
    let granted = s
        .allow_provision_retry(allow(&fp, 'a', "r1", T0 + MIN))
        .await
        .unwrap();
    assert!(matches!(granted, Allowed::Granted(_)));
    assert!(
        s.provision_reports_list(T0 + MIN).await.unwrap()[0]
            .retry
            .is_some()
    );
    // A report with another terminal id replaces the report.
    let out = apply(&s, &fp, report('b', "denied", "n"), T0 + 6 * MIN).await;
    assert_eq!(out, Outcome::Stored);
    let row = s
        .provision_reports_list(T0 + 6 * MIN)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(row.terminal_id, terminal('b'));
    assert_eq!(row.retry, None);
}

#[tokio::test]
async fn provision_report_a_retry_is_delivered_once_and_only_for_the_exact_binding() {
    let (s, _det) = store().await;
    let fp = key();
    apply(&s, &fp, report('a', "denied", "n"), T0).await;
    let Allowed::Granted(state) = s
        .allow_provision_retry(allow(&fp, 'a', "r1", T0 + MIN))
        .await
        .unwrap()
    else {
        panic!("a permission");
    };
    assert!(!state.delivered);
    assert_eq!(state.expires_at, T0 + MIN + RETRY_TTL_MS);
    // A report of another binding inside the rate window gets no retry.
    let other = apply(&s, &fp, report('b', "denied", "n"), T0 + 2 * MIN).await;
    assert_eq!(other, Outcome::RateLimited(180));
    // The exact binding gets the permission, also inside the rate window.
    let out = apply(&s, &fp, report('a', "denied", "n"), T0 + 3 * MIN).await;
    assert_eq!(
        out,
        Outcome::Retry {
            retry_id: format!("{:032x}", T0 + MIN)
        }
    );
    let row = s
        .provision_reports_list(T0 + 3 * MIN)
        .await
        .unwrap()
        .remove(0);
    assert!(row.retry.unwrap().delivered);
    // A permission is delivered once.
    let out = apply(&s, &fp, report('a', "denied", "n"), T0 + 9 * MIN).await;
    assert_eq!(out, Outcome::Stored);
}

#[tokio::test]
async fn provision_report_a_delivered_retry_stores_the_row_like_any_report() {
    let (s, _det) = store().await;
    let fp = key();
    apply(&s, &fp, report('a', "denied", "old name"), T0).await;
    s.allow_provision_retry(allow(&fp, 'a', "r1", T0 + MIN))
        .await
        .unwrap();
    let out = apply(
        &s,
        &fp,
        report('a', "pairing_failed", "new name"),
        T0 + 6 * MIN,
    )
    .await;
    assert!(matches!(out, Outcome::Retry { .. }));
    let row = s
        .provision_reports_list(T0 + 6 * MIN)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (row.name.as_str(), row.code.as_str(), row.received_at),
        ("new name", "pairing_failed", T0 + 6 * MIN)
    );
}

#[tokio::test]
async fn provision_report_retry_requests_are_idempotent_and_single() {
    let (s, _det) = store().await;
    let fp = key();
    // No report.
    assert_eq!(
        s.allow_provision_retry(allow(&fp, 'a', "r1", T0))
            .await
            .unwrap(),
        Allowed::NoReport
    );
    apply(&s, &fp, report('a', "denied", "n"), T0).await;
    // A stale binding.
    assert_eq!(
        s.allow_provision_retry(allow(&fp, 'b', "r1", T0))
            .await
            .unwrap(),
        Allowed::Changed
    );
    let first = s
        .allow_provision_retry(allow(&fp, 'a', "r1", T0 + MIN))
        .await
        .unwrap();
    // The same request id again: the same permission, not a second one.
    let again = s
        .allow_provision_retry(allow(&fp, 'a', "r1", T0 + 2 * MIN))
        .await
        .unwrap();
    assert_eq!(first, again);
    // Another request id while the permission is live.
    assert_eq!(
        s.allow_provision_retry(allow(&fp, 'a', "r2", T0 + 2 * MIN))
            .await
            .unwrap(),
        Allowed::Queued
    );
    // An expired permission counts as none.
    let late = T0 + MIN + RETRY_TTL_MS + 1;
    assert_eq!(s.provision_reports_list(late).await.unwrap()[0].retry, None);
    assert!(matches!(
        s.allow_provision_retry(allow(&fp, 'a', "r2", late))
            .await
            .unwrap(),
        Allowed::Granted(_)
    ));
}

#[tokio::test]
async fn provision_report_a_code_without_a_board_repair_gets_no_retry() {
    let (s, _det) = store().await;
    let fp = key();
    apply(&s, &fp, report('a', "service_conflict", "n"), T0).await;
    assert_eq!(
        s.allow_provision_retry(allow(&fp, 'a', "r1", T0))
            .await
            .unwrap(),
        Allowed::NotAllowed
    );
    for code in [
        "denied",
        "cancelled",
        "revoked",
        "pending",
        "pairing_failed",
        "provider_probe_failed",
        "integration_failed",
        "unknown_error",
    ] {
        assert!(retry_allowed(code), "{code}");
    }
    for code in fleetdeck_link::report_accept::TERMINAL_CODES {
        let allowed = retry_allowed(code);
        let disabled = [
            "file_invalid",
            "release_untrusted",
            "build_mismatch",
            "config_conflict",
            "config_changed",
            "service_conflict",
            "station_identity_changed",
            "paired_elsewhere",
            "fingerprint_mismatch",
            "request_id_conflict",
            "unsupported",
            "invalid_request",
            "retry_file",
        ];
        assert_eq!(allowed, !disabled.contains(&code), "{code}");
    }
}

#[tokio::test]
async fn provision_report_cancel_and_dismiss() {
    let (s, _det) = store().await;
    let fp = key();
    let now = |n: i64| T0 + n * MIN;
    assert_eq!(
        s.cancel_provision_retry(fp.to_string(), now(0))
            .await
            .unwrap(),
        Cancelled::NoReport
    );
    apply(&s, &fp, report('a', "denied", "n"), now(0)).await;
    assert_eq!(
        s.cancel_provision_retry(fp.to_string(), now(0))
            .await
            .unwrap(),
        Cancelled::NoRetry
    );
    s.allow_provision_retry(allow(&fp, 'a', "r1", now(1)))
        .await
        .unwrap();
    assert_eq!(
        s.cancel_provision_retry(fp.to_string(), now(1))
            .await
            .unwrap(),
        Cancelled::Done
    );
    // Delivered: it cannot be cancelled.
    s.allow_provision_retry(allow(&fp, 'a', "r2", now(2)))
        .await
        .unwrap();
    apply(&s, &fp, report('a', "denied", "n"), now(3)).await;
    assert_eq!(
        s.cancel_provision_retry(fp.to_string(), now(4))
            .await
            .unwrap(),
        Cancelled::Delivered
    );
    // A delivered permission with another request id counts as queued.
    assert_eq!(
        s.allow_provision_retry(allow(&fp, 'a', "r3", now(4)))
            .await
            .unwrap(),
        Allowed::Queued
    );
    // Dismissing removes the report and its permission.
    assert!(s.dismiss_provision_report(fp.to_string()).await.unwrap());
    assert!(s.provision_reports_list(now(4)).await.unwrap().is_empty());
    assert!(!s.dismiss_provision_report(fp.to_string()).await.unwrap());
    // A new report of the key starts without a permission.
    apply(&s, &fp, report('a', "denied", "n"), now(5)).await;
    assert_eq!(
        s.provision_reports_list(now(5)).await.unwrap()[0].retry,
        None
    );
}

#[tokio::test]
async fn provision_report_the_window_is_one_hour_and_off_by_default() {
    let (s, det) = store().await;
    let reports = ProvisionReports::new(s);
    assert!(!reports.is_open());
    assert_eq!(reports.window().deadline_ms, None);
    let w = reports.open_window();
    assert!(w.open && reports.is_open());
    assert_eq!((w.generation, w.deadline_ms), (1, Some(T0 + 60 * MIN)));
    det.advance(Duration::from_secs(59 * 60));
    assert!(reports.is_open());
    det.advance(Duration::from_secs(2 * 60));
    assert!(!reports.is_open());
    // Opening again is a new generation; closing ends it.
    assert_eq!(reports.open_window().generation, 2);
    let w = reports.close_window();
    assert!(!w.open && w.deadline_ms.is_none() && w.generation == 2);
}

#[tokio::test]
async fn provision_report_a_closed_window_answers_disabled_and_stores_nothing() {
    let (s, _det) = store().await;
    let reports = ProvisionReports::new(s.clone());
    let reply = reports.report(key(), report('a', "denied", "n"), T0).await;
    assert_eq!(
        (reply.status, reply.retry_after_seconds),
        (ReportStatus::Disabled, 600)
    );
    assert!(s.provision_reports_list(T0).await.unwrap().is_empty());
}

#[tokio::test]
async fn provision_report_the_replies_carry_the_fixed_seconds() {
    let (s, _det) = store().await;
    let reports = ProvisionReports::new(s.clone());
    reports.open_window();
    let fp = key();
    let stored = reports.report(fp, report('a', "denied", "n"), T0).await;
    assert_eq!(
        (stored.status, stored.retry_after_seconds),
        (ReportStatus::Stored, 600)
    );
    let limited = reports
        .report(fp, report('a', "denied", "n"), T0 + 2 * MIN)
        .await;
    assert_eq!(
        (limited.status, limited.retry_after_seconds),
        (ReportStatus::RateLimited, 180)
    );
    s.allow_provision_retry(allow(&fp, 'a', "r1", T0 + 3 * MIN))
        .await
        .unwrap();
    let retry = reports
        .report(fp, report('a', "denied", "n"), T0 + 4 * MIN)
        .await;
    assert_eq!(
        (retry.status, retry.retry_after_seconds),
        (ReportStatus::Retry, 0)
    );
    assert_eq!(retry.terminal_id, Some(terminal('a')));
    assert_eq!(retry.state_digest, Some(digest('a')));
    assert_eq!(retry.retry_id.as_deref().map(str::len), Some(32));
    assert!(retry.checked().is_some());
    // The table is full for a new key.
    for _ in 0..63 {
        reports
            .report(key(), report('a', "denied", "n"), T0 + 4 * MIN)
            .await;
    }
    let full = reports
        .report(key(), report('a', "denied", "n"), T0 + 4 * MIN)
        .await;
    assert_eq!(
        (full.status, full.retry_after_seconds),
        (ReportStatus::Capacity, 3600)
    );
}

#[tokio::test]
async fn provision_report_the_global_limit_is_120_updates_in_a_sliding_hour() {
    let (s, _det) = store().await;
    let reports = ProvisionReports::new(s.clone());
    reports.open_window();
    let keys: Vec<Fingerprint> = (0..64).map(|_| key()).collect();
    for k in &keys {
        let r = reports.report(*k, report('a', "denied", "n"), T0).await;
        assert_eq!(r.status, ReportStatus::Stored);
    }
    // 56 rewrites after five minutes: 64 + 56 = 120 counted updates.
    for k in &keys[..56] {
        let r = reports
            .report(*k, report('a', "pairing_failed", "n"), T0 + 5 * MIN)
            .await;
        assert_eq!(r.status, ReportStatus::Stored);
    }
    // The 121st is refused until the oldest counted update leaves the hour.
    let r = reports
        .report(keys[56], report('a', "pairing_failed", "n"), T0 + 5 * MIN)
        .await;
    assert_eq!(
        (r.status, r.retry_after_seconds),
        (ReportStatus::RateLimited, 3300)
    );
    // A refused update changed nothing.
    let rows = s.provision_reports_list(T0 + 5 * MIN).await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.fingerprint == keys[56].to_string())
        .unwrap();
    assert_eq!(row.code, "denied");
    // After an hour the counted updates leave the window.
    let r = reports
        .report(keys[56], report('a', "pairing_failed", "n"), T0 + 61 * MIN)
        .await;
    assert_eq!(r.status, ReportStatus::Stored);
}

#[tokio::test]
async fn provision_report_rows_hold_only_the_validated_fields() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::store::schema::migrations()
        .to_latest(&mut conn)
        .unwrap();
    let columns = |table: &str| -> Vec<String> {
        conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        columns("provision_reports"),
        [
            "fingerprint",
            "terminal_id",
            "state_digest",
            "code",
            "phase",
            "name",
            "received_at",
            "last_contact",
            "updated_at"
        ]
    );
    assert_eq!(
        columns("provision_retries"),
        [
            "fingerprint",
            "retry_id",
            "terminal_id",
            "state_digest",
            "request_id",
            "created_at",
            "expires_at",
            "delivered_at"
        ]
    );
}
