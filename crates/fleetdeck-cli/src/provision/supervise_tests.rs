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

use super::binding::{self, Authorization, Binding};
use super::effects::{Effects, Terminal};
use super::fake::{DECK_FP, Fake, JoinStep, ReportStep, file_text, go as bootstrap};
use super::run::Exit;
use super::sha256_hex;
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

/// The binding of the condition that the state keeps now, with the file as
/// read now (what the supervisor stores before it reports).
fn bind(fake: &Fake) -> Binding {
    let state = fake.state().unwrap();
    let file = fake.provision.lock().unwrap().clone().unwrap();
    let digest = binding::digest(
        state.error.as_deref().unwrap(),
        state.phase,
        &sha256_hex(&file),
        DECK_FP,
        state.station_fingerprint.as_deref(),
    );
    binding::ensure(&fake.trust_root(), &fake.station_dir(), &digest).unwrap()
}

fn authorize(fake: &Fake, retry_id: &str, bound: &Binding) -> std::path::PathBuf {
    let path = binding::authorization_path(&fake.station_dir());
    Authorization {
        retry_id: retry_id.to_owned(),
        terminal_id: bound.terminal_id.clone(),
        state_digest: bound.state_digest.clone(),
    }
    .write(&fake.trust_root(), &path)
    .unwrap();
    path
}

/// A bootstrap that stopped with `pending`, then its binding.
async fn stopped(fake: &Fake, then: &[JoinStep]) -> Binding {
    pending_then(fake, then);
    assert_eq!(bootstrap(fake, false).await, Exit::Terminal);
    bind(fake)
}

#[tokio::test]
async fn a_busy_bootstrap_keeps_the_authorization_for_the_next_run() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    // Another bootstrap holds the lock for the first run, and ends before the
    // second one.
    let held = try_lock_private_below(
        &fake.trust_root(),
        &super::state::lock_path(&fake.station_dir()),
    )
    .unwrap()
    .unwrap();
    *fake.on_first_sleep.lock().unwrap() = Some(Box::new(move |_| drop(held)));
    assert_eq!(go(&fake).await, Exit::Complete);
    assert_eq!(sleeps(&fake), [secs(30)]);
    // The second run got the same authorization and consumed it.
    let state = fake.state().unwrap();
    assert_eq!(state.error, None);
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    assert_eq!(fake.count("join"), 2);
    assert!(!path.exists());
    assert!(fake.reports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_that_stops_before_the_check_keeps_the_authorization() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    // The provisioning file is missing for the first run and back at the
    // first sleep.
    let good = std::mem::replace(
        &mut *fake.provision.lock().unwrap(),
        Err(Terminal::FILE_INVALID),
    );
    *fake.on_first_sleep.lock().unwrap() = Some(Box::new(move |fake: &Fake| {
        *fake.provision.lock().unwrap() = good;
    }));
    assert_eq!(go(&fake).await, Exit::Complete);
    assert_eq!(sleeps(&fake).len(), 1);
    assert!(
        fake.lines()
            .contains(&"provision: validated: file_invalid".to_owned())
    );
    // The second run consumed the authorization, exactly once.
    let state = fake.state().unwrap();
    assert_eq!(state.error, None);
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    assert_eq!(fake.count("join"), 2);
    assert!(!path.exists());
    assert_eq!(fake.authorization_deleted.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_start_with_a_valid_authorization_file_consumes_it_in_the_first_run() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    assert_eq!(go(&fake).await, Exit::Complete);
    let state = fake.state().unwrap();
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(state.error, None);
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    assert_eq!(fake.count("join"), 2);
    assert!(fake.reports.lock().unwrap().is_empty());
    assert!(!path.exists());
}

#[tokio::test]
async fn a_start_with_a_stale_authorization_file_deletes_it_and_reports() {
    let fake = Fake::new();
    stopped(&fake, &[JoinStep::Paired]).await;
    // The stored binding is of another condition than the state.
    let stale = binding::ensure(&fake.trust_root(), &fake.station_dir(), &"44".repeat(32)).unwrap();
    let path = authorize(&fake, RETRY, &stale);
    let before = std::fs::read(fake.state_file()).unwrap();
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert!(
        fake.lines()
            .contains(&"provision: retry: report_changed".to_owned())
    );
    assert_eq!(std::fs::read(fake.state_file()).unwrap(), before);
    assert!(!path.exists());
    assert_eq!(fake.authorization_deleted.lock().unwrap().len(), 1);
    // The report step ran after the refusal.
    assert_eq!(fake.reports.lock().unwrap().len(), 1);
    assert_eq!(fake.count("join"), 1);
}

#[tokio::test]
async fn a_replayed_authorization_is_refused_and_deleted() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Code("pending")]).await;
    let path = authorize(&fake, RETRY, &bound);
    // The first use stops with the same condition again.
    assert_eq!(
        super::fake::go_authorized(&fake, &path).await,
        Exit::Terminal
    );
    assert_eq!(fake.count("join"), 2);
    assert!(!path.exists());
    let again = bind(&fake);
    assert_eq!(again, bound);
    let path = authorize(&fake, RETRY, &again);
    let before = std::fs::read(fake.state_file()).unwrap();
    push(&fake, reply(ReportStatus::Stored, 600));
    stop_after(&fake, 1);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    assert!(
        fake.lines()
            .contains(&"provision: retry: retry_replayed".to_owned())
    );
    assert_eq!(std::fs::read(fake.state_file()).unwrap(), before);
    assert!(!path.exists());
    assert_eq!(fake.count("join"), 2);
}

#[tokio::test]
async fn a_change_of_the_provisioning_file_gives_a_new_terminal_id() {
    let fake = Fake::new();
    pending_then(&fake, &[]);
    push(&fake, reply(ReportStatus::Stored, 600));
    push(&fake, reply(ReportStatus::Stored, 600));
    // After the first report the owner changes the destination of the file
    // and keeps the pin.
    *fake.on_first_sleep.lock().unwrap() = Some(Box::new(|fake: &Fake| {
        let other = String::from_utf8(file_text("laptop"))
            .unwrap()
            .replace("deck.example.ts.net", "other.example.ts.net");
        *fake.provision.lock().unwrap() = Ok(other.into_bytes());
    }));
    stop_after(&fake, 2);
    assert_eq!(go(&fake).await, Exit::Unexpected);
    let reports = fake.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[1].0, "other.example.ts.net");
    assert_ne!(reports[0].3.terminal_id, reports[1].3.terminal_id);
    assert_ne!(reports[0].3.state_digest, reports[1].3.state_digest);
    // The state itself did not change.
    assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
}
