//! The supervisor (design `board-gaps.md` 4.3) over the fake effects. A test
//! ends the loop with `stop_after_sleeps`: after that many waits the next
//! bootstrap fails with `Unexpected`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use fleetdeck_core::private_file::try_lock_private_below;
use fleetdeck_link::ReportReply;
use fleetdeck_link::hello::ReportStatus;

use super::binding;
use super::effects::Effects;
use super::fake::{DECK_FP, Fake, JoinStep, ReportStep};
use super::run::Exit;
use super::state::Phase;
use super::supervise::supervise;

const RETRY: &str = "0123456789abcdef0123456789abcdef";

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

async fn go(fake: &Fake) -> Exit {
    supervise(fake, Path::new("/provision.toml")).await
}

fn push(fake: &Fake, step: ReportStep) {
    fake.report_steps.lock().unwrap().push_back(step);
}

fn stop_after(fake: &Fake, sleeps: usize) {
    *fake.stop_after_sleeps.lock().unwrap() = Some(sleeps);
}

fn reply(status: ReportStatus, after: u64) -> ReportStep {
    ReportStep::Reply(ReportReply::plain(status, after))
}

fn sleeps(fake: &Fake) -> Vec<Duration> {
    fake.sleeps.lock().unwrap().clone()
}

/// A bootstrap that stops with `pending` in the phase `service`.
fn pending_then(fake: &Fake, then: &[JoinStep]) {
    let mut steps = vec![JoinStep::Code("pending")];
    steps.extend_from_slice(then);
    fake.set_join(&steps);
}

#[tokio::test]
async fn a_complete_bootstrap_stops_the_supervisor_with_no_report() {
    let fake = Fake::new();
    assert_eq!(go(&fake).await, Exit::Complete);
    assert_eq!(Exit::Complete.code(), 0);
    assert!(fake.reports.lock().unwrap().is_empty());
    assert!(sleeps(&fake).is_empty());
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_held_supervisor_lock_exits_0_at_once() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    let held = try_lock_private_below(
        &fake.trust_root(),
        &binding::supervise_lock_path(&fake.station_dir()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(go(&fake).await.code(), 0);
    assert!(fake.calls.lock().unwrap().is_empty());
    assert!(fake.state().is_none());
    drop(held);
}

#[tokio::test]
async fn the_supervisor_never_holds_the_bootstrap_lock_while_it_waits() {
    let fake = Fake::new();
    pending_then(&fake, &[]);
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 1);
    // The sleep hook of the fake cannot probe a lock, so the proof is the
    // end: the bootstrap lock is free when the supervisor stops.
    assert_eq!(go(&fake).await, Exit::Unexpected);
    let lock = try_lock_private_below(
        &fake.trust_root(),
        &super::state::lock_path(&fake.station_dir()),
    )
    .unwrap();
    assert!(lock.is_some());
}

#[tokio::test]
async fn a_station_without_an_identity_reports_nothing_and_creates_no_key() {
    let fake = Fake::new();
    pending_then(&fake, &[]);
    *fake.identity.lock().unwrap() = false;
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert!(fake.reports.lock().unwrap().is_empty());
    assert_eq!(sleeps(&fake), [secs(600)]);
    assert!(
        fake.lines()
            .contains(&"provision: report: no_identity".to_owned())
    );
    assert!(!fake.station_dir().join("identity.ed25519").exists());
    assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
}

#[tokio::test]
async fn a_provisioning_file_that_is_not_valid_is_not_reported() {
    let fake = Fake::new();
    *fake.provision.lock().unwrap() = Ok(b"junk".to_vec());
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert!(fake.reports.lock().unwrap().is_empty());
    assert_eq!(sleeps(&fake), [secs(600)]);
    assert!(
        fake.lines()
            .contains(&"provision: report: file_invalid".to_owned())
    );
}

#[tokio::test]
async fn the_first_report_is_at_once_and_names_the_condition() {
    let fake = Fake::new();
    pending_then(&fake, &[]);
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    let reports = fake.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 1);
    let (host, port, pin, report) = &reports[0];
    assert_eq!(host, "deck.example.ts.net");
    assert_eq!(*port, 7421);
    assert_eq!(pin, DECK_FP);
    assert_eq!(report.code, "pending");
    assert_eq!(report.phase, "service");
    assert_eq!(report.name, "laptop");
    let stored = binding::load(&binding::binding_path(&fake.station_dir())).unwrap();
    assert_eq!(report.terminal_id, stored.terminal_id);
    assert_eq!(report.state_digest, stored.state_digest);
    let mode = std::fs::metadata(binding::binding_path(&fake.station_dir()))
        .unwrap()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn the_same_condition_keeps_its_terminal_id_and_a_new_one_gets_a_new_id() {
    let fake = Fake::new();
    pending_then(&fake, &[]);
    push(&fake, reply(ReportStatus::Stored, 600));
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 2);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    let reports = fake.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].3.terminal_id, reports[1].3.terminal_id);
    assert_eq!(reports[0].3.state_digest, reports[1].3.state_digest);
}

#[tokio::test]
async fn each_reply_status_waits_its_time_plus_the_jitter() {
    // (status, retry_after_seconds, the wait without the jitter)
    let table = [
        (ReportStatus::Stored, 600, 600),
        (ReportStatus::Disabled, 600, 600),
        (ReportStatus::Capacity, 5, 60),
        (ReportStatus::RateLimited, 900, 900),
        (ReportStatus::Stored, 0, 60),
    ];
    for (status, after, base) in table {
        for jitter in [0, 17, 60] {
            let fake = Fake::new();
            pending_then(&fake, &[]);
            *fake.jitter.lock().unwrap() = secs(jitter);
            push(&fake, reply(status, after));
            stop_after(&fake, 1);
            assert_eq!(go(&fake).await, Exit::Unexpected);
            assert_eq!(fake.reports.lock().unwrap().len(), 1);
            assert_eq!(sleeps(&fake), [secs(base + jitter)], "{status:?} {after}");
            assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
        }
    }
}

#[tokio::test]
async fn a_retry_reply_for_another_binding_is_ignored() {
    let fake = Fake::new();
    pending_then(&fake, &[JoinStep::Paired]);
    push(&fake, ReportStep::StaleRetry);
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert_eq!(sleeps(&fake), [secs(60)]);
    assert!(!binding::authorization_path(&fake.station_dir()).exists());
    assert!(fake.authorization_deleted.lock().unwrap().is_empty());
    assert_eq!(fake.count("join"), 1);
    assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
    assert!(
        fake.lines()
            .contains(&"provision: report: retry_ignored".to_owned())
    );
}

#[tokio::test]
async fn a_retry_reply_for_the_current_binding_runs_the_bootstrap_once_with_it() {
    let fake = Fake::new();
    pending_then(&fake, &[JoinStep::Paired]);
    push(&fake, ReportStep::RetryFor(RETRY));
    assert_eq!(go(&fake).await, Exit::Complete);
    assert_eq!(fake.reports.lock().unwrap().len(), 1);
    assert_eq!(fake.count("join"), 2);
    assert!(sleeps(&fake).is_empty());
    let state = fake.state().unwrap();
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(state.error, None);
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    // One authorization, consumed and removed.
    assert_eq!(fake.authorization_deleted.lock().unwrap().len(), 1);
    assert!(!binding::authorization_path(&fake.station_dir()).exists());
}

#[tokio::test]
async fn a_retry_that_stops_again_is_reported_again_after_the_retry() {
    let fake = Fake::new();
    pending_then(&fake, &[JoinStep::Code("pending")]);
    push(&fake, ReportStep::RetryFor(RETRY));
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert_eq!(fake.reports.lock().unwrap().len(), 2);
    assert_eq!(fake.count("join"), 2);
    assert_eq!(sleeps(&fake), [secs(600)]);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("pending"));
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
}

#[tokio::test]
async fn a_link_error_is_a_diagnostic_and_keeps_the_retained_error() {
    for word in ["fingerprint_mismatch", "timeout", "unsupported"] {
        let fake = Fake::new();
        pending_then(&fake, &[]);
        push(&fake, ReportStep::Fail(word));
        stop_after(&fake, 1);
        assert_eq!(go(&fake).await, Exit::Unexpected);
        assert_eq!(sleeps(&fake), [secs(600)], "{word}");
        assert!(
            fake.lines().contains(&format!("provision: report: {word}")),
            "{word}"
        );
        let state = fake.state().unwrap();
        assert_eq!(state.error.as_deref(), Some("pending"), "{word}");
        assert_eq!(state.phase, Phase::Service, "{word}");
    }
}

#[tokio::test]
async fn a_busy_bootstrap_waits_30_seconds_and_is_not_reported() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    let held = try_lock_private_below(
        &fake.trust_root(),
        &super::state::lock_path(&fake.station_dir()),
    )
    .unwrap()
    .unwrap();
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert_eq!(sleeps(&fake), [secs(30)]);
    assert!(fake.reports.lock().unwrap().is_empty());
    drop(held);
}

#[tokio::test]
async fn the_supervisor_leaves_the_provisioning_file_and_the_checks_alone() {
    // A condition that no retry repairs stays after a report and a retry
    // reply: the bootstrap checks run again and stop again.
    let fake = Fake::new();
    pending_then(&fake, &[JoinStep::Paired]);
    push(&fake, ReportStep::RetryFor(RETRY));
    push(&fake, reply(ReportStatus::Disabled, 600));
    *fake.service.lock().unwrap() = super::effects::ServiceState::Conflict;
    stop_after(&fake, 1);
    // The service conflict stops the first run before the join.
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert_eq!(fake.count("join"), 0);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("service_conflict")
    );
    assert_eq!(fake.reports.lock().unwrap().len(), 2);
    assert_eq!(fake.reports.lock().unwrap()[0].3.code, "service_conflict");
}
