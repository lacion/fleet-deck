//! The run: the happy path, the lock and the pairing (retry table rows).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_core::private_file::try_lock_private_below;

use super::effects::Effects;
use super::fake::{DECK_FP, Fake, JoinStep, OTHER_FP, STATION_FP, file_text, go};
use super::run::Exit;
use super::sha256_hex;
use super::state::{Phase, lock_path};
use crate::station_fingerprint::grouped;

const APPROVE: &str = "Approve this station fingerprint on the board: ";

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn the_exit_codes_are_0_0_1_2() {
    assert_eq!(Exit::Complete.code(), 0);
    assert_eq!(Exit::Busy.code(), 0);
    assert_eq!(Exit::Unexpected.code(), 1);
    assert_eq!(Exit::Terminal.code(), 2);
}

#[tokio::test]
async fn a_first_run_does_every_step_in_order() {
    let fake = Fake::new();
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            "create_config:laptop",
            "install_service",
            "fingerprint",
            "join",
            "fingerprint",
            "fingerprint:probe",
            "omarchy_install",
            "omarchy_status",
            "fingerprint",
        ]
    );
    let text = grouped(STATION_FP);
    assert_eq!(
        fake.lines(),
        [
            format!("{APPROVE}{text}"),
            "No providers are configured.".to_owned()
        ]
    );
    assert_eq!(*fake.notices.lock().unwrap(), [text]);
    assert_eq!(
        *fake.join_args.lock().unwrap(),
        [(
            "deck.example.ts.net".to_owned(),
            7421,
            DECK_FP.to_owned(),
            "laptop".to_owned()
        )]
    );
    let state = fake.state().unwrap();
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(state.error, None);
    assert_eq!(state.station_fingerprint.as_deref(), Some(STATION_FP));
    assert_eq!(state.config_sha256, sha256_hex(&file_text("laptop")));
    assert!(fake.sleeps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_second_instance_exits_busy_without_work() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    let lock = try_lock_private_below(&fake.trust_root(), &lock_path(&fake.station_dir()))
        .unwrap()
        .unwrap();
    assert_eq!(go(&fake, false).await, Exit::Busy);
    assert_eq!(Exit::Busy.code(), 0);
    assert!(fake.calls.lock().unwrap().is_empty());
    assert!(fake.lines().is_empty());
    assert!(fake.state().is_none());
    drop(lock);
    assert_eq!(go(&fake, false).await, Exit::Complete);
}

#[tokio::test]
async fn every_retry_row_waits_then_tries_again() {
    let rows: [(&str, u64); 12] = [
        ("window_closed", 30),
        ("not_accepted", 30),
        ("expired", 30),
        ("busy", 30),
        ("timeout", 30),
        ("failed", 30),
        ("no_address", 30),
        ("tailscale_unavailable", 30),
        ("not_in_tailnet", 30),
        ("denied", 600),
        ("cancelled", 600),
        ("revoked", 600),
    ];
    for (code, wait) in rows {
        let fake = Fake::new();
        fake.set_join(&[JoinStep::Code(code), JoinStep::Paired]);
        assert_eq!(go(&fake, false).await, Exit::Complete, "{code}");
        assert_eq!(*fake.sleeps.lock().unwrap(), [secs(wait)], "{code}");
        assert_eq!(fake.count("join"), 2, "{code}");
        assert!(
            fake.lines()
                .contains(&format!("provision: pairing: {code}")),
            "{code}"
        );
        assert_eq!(fake.state().unwrap().phase, Phase::Complete);
    }
}

#[tokio::test]
async fn every_stop_row_is_a_saved_terminal_and_no_retry() {
    let rows = [
        ("fingerprint_mismatch", "fingerprint_mismatch"),
        ("request_id_conflict", "request_id_conflict"),
        ("pending", "pending"),
        ("unsupported", "unsupported"),
        ("invalid_request", "invalid_request"),
        ("retry_file", "retry_file"),
        ("unknown_request", "unknown_error"),
        ("client_gone", "unknown_error"),
        ("probe_failed", "unknown_error"),
        ("self_connect", "unknown_error"),
        ("handshake_version", "unknown_error"),
        ("some new code with spaces", "unknown_error"),
        ("", "unknown_error"),
    ];
    for (code, saved) in rows {
        let fake = Fake::new();
        fake.set_join(&[JoinStep::Code(code)]);
        assert_eq!(go(&fake, false).await, Exit::Terminal, "{code}");
        assert_eq!(fake.count("join"), 1, "{code}");
        assert!(fake.sleeps.lock().unwrap().is_empty(), "{code}");
        let state = fake.state().unwrap();
        assert_eq!(state.error.as_deref(), Some(saved), "{code}");
        assert_eq!(state.phase, Phase::Service);
        assert_eq!(
            fake.lines().last().unwrap(),
            &format!("provision: service: {saved}")
        );
        assert_eq!(Exit::Terminal.code(), 2);
    }
}

#[tokio::test]
async fn a_closed_window_is_retried_until_it_opens() {
    let fake = Fake::new();
    fake.set_join(&[
        JoinStep::Code("window_closed"),
        JoinStep::Code("window_closed"),
        JoinStep::Code("busy"),
        JoinStep::Paired,
    ]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30), secs(30), secs(30)]);
    assert_eq!(fake.elapsed(), secs(90));
    // The notification and the approval line come once.
    assert_eq!(fake.notices.lock().unwrap().len(), 1);
    let approvals = fake
        .lines()
        .iter()
        .filter(|l| l.starts_with(APPROVE))
        .count();
    assert_eq!(approvals, 1);
}

#[tokio::test]
async fn a_denial_waits_600_seconds_and_then_an_approval_pairs() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Code("denied"), JoinStep::Paired]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(600)]);
    assert!(fake.elapsed() >= secs(600));
    assert_eq!(fake.state().unwrap().error, None);
}

#[tokio::test]
async fn the_same_existing_deck_skips_the_join() {
    let fake = Fake::new();
    fake.set_paired(Some(DECK_FP));
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 0);
    assert_eq!(fake.state().unwrap().phase, Phase::Complete);
}

#[tokio::test]
async fn a_different_existing_deck_is_paired_elsewhere() {
    let fake = Fake::new();
    fake.set_paired(Some(OTHER_FP));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.count("join"), 0);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("paired_elsewhere"));
    assert_eq!(state.phase, Phase::Service);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: service: paired_elsewhere"
    );
}

#[tokio::test]
async fn already_paired_with_a_matching_pairing_goes_on_without_a_wait() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::CodeCommitted("already_paired")]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(fake.count("join"), 1);
    assert!(fake.sleeps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_second_already_paired_without_a_pairing_waits_30_seconds() {
    let fake = Fake::new();
    fake.set_join(&[
        JoinStep::Code("already_paired"),
        JoinStep::Code("already_paired"),
        JoinStep::Code("already_paired"),
        JoinStep::Paired,
    ]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30), secs(30)]);
    assert_eq!(fake.count("join"), 4);
}

#[tokio::test]
async fn another_outcome_resets_the_already_paired_count() {
    let fake = Fake::new();
    fake.set_join(&[
        JoinStep::Code("already_paired"),
        JoinStep::Code("timeout"),
        JoinStep::Code("already_paired"),
        JoinStep::Paired,
    ]);
    assert_eq!(go(&fake, false).await, Exit::Complete);
    assert_eq!(*fake.sleeps.lock().unwrap(), [secs(30)]);
    assert!(
        !fake
            .lines()
            .contains(&"provision: pairing: already_paired".to_owned())
    );
}

#[tokio::test]
async fn an_approval_before_the_local_commit_is_found_by_the_rerun() {
    let fake = Fake::new();
    // The deck committed, the local side did not: the join fails.
    fake.set_join(&[JoinStep::CodeCommitted("client_gone")]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.count("join"), 1);
    // Without `--retry` the terminal error is kept and nothing runs.
    let calls = fake.calls.lock().unwrap().len();
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
    assert_eq!(
        fake.lines().last().unwrap(),
        "provision: service: unknown_error"
    );
    // With `--retry` the run sees the pairing and skips the join.
    assert_eq!(go(&fake, true).await, Exit::Complete);
    assert_eq!(fake.count("join"), 1);
    let state = fake.state().unwrap();
    assert_eq!(state.error, None);
    assert_eq!(state.phase, Phase::Complete);
}

#[tokio::test]
async fn a_terminal_error_that_stays_is_saved_again_after_retry() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Code("pending"), JoinStep::Code("pending")]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    assert_eq!(go(&fake, true).await, Exit::Terminal);
    assert_eq!(fake.count("join"), 2);
    assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
}

#[tokio::test]
async fn the_retry_clears_the_error_in_the_file_before_the_sequence() {
    let fake = Fake::new();
    fake.set_join(&[JoinStep::Code("pending")]);
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    // A station that never answers keeps the run in its loop; stop it with a
    // conflicting service, which saves its own error. The cleared error is
    // visible in the saved phase and error after the run.
    *fake.service.lock().unwrap() = super::effects::ServiceState::Conflict;
    assert_eq!(go(&fake, true).await, Exit::Terminal);
    assert_eq!(
        fake.state().unwrap().error.as_deref(),
        Some("service_conflict")
    );
}

#[tokio::test]
async fn the_lock_is_held_while_a_terminal_condition_is_saved() {
    use std::sync::{Arc, Mutex};

    let fake = Fake::new();
    fake.edit_file(super::release::HOOK_PATH, |f| f.bytes.push(b'#'));
    let root = fake.trust_root();
    let lock = lock_path(&fake.station_dir());
    let state_file = fake.state_file();
    type Seen = Vec<(String, bool, Option<String>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let record = Arc::clone(&seen);
    fake.progress_hooks
        .lock()
        .unwrap()
        .push(Box::new(move |line| {
            // The terminal line comes after the save of the state.
            let held = try_lock_private_below(&root, &lock).unwrap().is_none();
            let error = std::fs::read(&state_file)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .filter(|text| text.contains("release_untrusted"))
                .map(|_| "release_untrusted".to_owned());
            record.lock().unwrap().push((line.to_owned(), held, error));
        }));
    assert_eq!(go(&fake, false).await, Exit::Terminal);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [(
            "provision: validated: release_untrusted".to_owned(),
            true,
            Some("release_untrusted".to_owned())
        )]
    );
    // After the run the lock is free.
    assert!(
        try_lock_private_below(&fake.trust_root(), &lock_path(&fake.station_dir()))
            .unwrap()
            .is_some()
    );
}
