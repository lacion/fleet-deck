//! The retry authorization of a setup report (design `board-gaps.md` 4.3):
//! the atomic consumption under the lock, the mismatch, the replay and a
//! crash on each side of the state write.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::binding::{self, Authorization, Binding};
use super::effects::{Effects, ServiceState};
use super::fake::{DECK_FP, Fake, JoinStep, file_text, go, go_authorized};
use super::run::Exit;
use super::state::{self, Phase};

const RETRY: &str = "0123456789abcdef0123456789abcdef";

fn hex(unit: &str, times: usize) -> String {
    unit.repeat(times)
}

/// The binding of the terminal condition that the state keeps now (what the
/// supervisor stores before it reports).
fn bind(fake: &Fake) -> Binding {
    let state = fake.state().unwrap();
    let digest = binding::digest(
        state.error.as_deref().unwrap(),
        state.phase,
        &state.config_sha256,
        DECK_FP,
        state.station_fingerprint.as_deref(),
    );
    binding::ensure(&fake.trust_root(), &fake.station_dir(), &digest).unwrap()
}

fn auth_path(fake: &Fake) -> PathBuf {
    binding::authorization_path(&fake.station_dir())
}

fn authorize(fake: &Fake, retry_id: &str, binding: &Binding) -> PathBuf {
    let path = auth_path(fake);
    Authorization {
        retry_id: retry_id.to_owned(),
        terminal_id: binding.terminal_id.clone(),
        state_digest: binding.state_digest.clone(),
    }
    .write(&fake.trust_root(), &path)
    .unwrap();
    path
}

/// A run that stops with `pending` in the phase `service`, then the binding.
async fn stopped(fake: &Fake, then: &[JoinStep]) -> Binding {
    let mut steps = vec![JoinStep::Code("pending")];
    steps.extend_from_slice(then);
    fake.set_join(&steps);
    assert_eq!(go(fake, false).await, Exit::Terminal);
    assert_eq!(fake.state().unwrap().error.as_deref(), Some("pending"));
    bind(fake)
}

fn last_line(fake: &Fake) -> String {
    fake.lines().last().unwrap().clone()
}

fn state_bytes(fake: &Fake) -> Vec<u8> {
    std::fs::read(fake.state_file()).unwrap()
}

#[tokio::test]
async fn a_good_authorization_clears_the_error_and_records_the_id_in_one_write() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Complete);
    // The checks of the sequence ran again: the join, then the link.
    assert_eq!(fake.count("join"), 2);
    let state = fake.state().unwrap();
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(state.error, None);
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    assert!(!path.exists());
    assert!(!fake.lines().iter().any(|l| l.contains("provision: retry:")));
}

#[tokio::test]
async fn the_state_write_clears_the_error_and_records_the_id_together() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[]).await;
    let path = authorize(&fake, RETRY, &bound);
    // The deck of the file is paired elsewhere: the run stops again, and the
    // saved state shows what the first write did.
    fake.set_paired(Some(&"ee".repeat(32)));
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("paired_elsewhere"));
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
}

#[tokio::test]
async fn an_authorization_never_repairs_another_condition() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    *fake.service.lock().unwrap() = ServiceState::Conflict;
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(last_line(&fake), "provision: service: service_conflict");
    let state = fake.state().unwrap();
    assert_eq!(state.error.as_deref(), Some("service_conflict"));
    assert_eq!(state.consumed_retry_id.as_deref(), Some(RETRY));
    assert_eq!(fake.count("join"), 1);
}

#[tokio::test]
async fn a_condition_that_changed_before_the_lock_is_report_changed() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    // Another run stopped with another code after the supervisor's check.
    let mut state = fake.state().unwrap();
    state.error = Some("denied".to_owned());
    state::save(&fake.trust_root(), &fake.state_file(), &state).unwrap();
    let before = state_bytes(&fake);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(last_line(&fake), "provision: retry: report_changed");
    assert_eq!(state_bytes(&fake), before);
    assert!(path.exists());
    assert_eq!(fake.count("join"), 1);
}

#[tokio::test]
async fn a_replayed_retry_id_is_refused() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Code("pending")]).await;
    let path = authorize(&fake, RETRY, &bound);
    // The retry run stops with the same condition again.
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(fake.count("join"), 2);
    let again = bind(&fake);
    assert_eq!(again, bound);
    let path = authorize(&fake, RETRY, &again);
    let before = state_bytes(&fake);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(last_line(&fake), "provision: retry: retry_replayed");
    assert_eq!(state_bytes(&fake), before);
    assert_eq!(fake.count("join"), 2);
    // Another retry id for the same condition is a new permission.
    let path = authorize(&fake, &hex("ab", 16), &again);
    fake.set_join(&[JoinStep::Paired]);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Complete);
    assert_eq!(
        fake.state().unwrap().consumed_retry_id.as_deref(),
        Some(hex("ab", 16).as_str())
    );
}

#[tokio::test]
async fn a_crash_before_the_state_write_keeps_the_error_and_the_authorization() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    let before = state_bytes(&fake);
    // The write of the state fails: the directory is not private any more.
    let dir = fake.station_dir();
    *fake.after_auth_read.lock().unwrap() = Some(Box::new({
        let dir = dir.clone();
        move || std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap()
    }));
    assert_eq!(go_authorized(&fake, &path).await, Exit::Unexpected);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(state_bytes(&fake), before);
    assert_eq!(fake.state().unwrap().consumed_retry_id, None);
    assert!(path.exists());
    assert_eq!(fake.count("join"), 1);
    // The same authorization still works.
    assert_eq!(go_authorized(&fake, &path).await, Exit::Complete);
    assert_eq!(
        fake.state().unwrap().consumed_retry_id.as_deref(),
        Some(RETRY)
    );
}

#[tokio::test]
async fn a_crash_after_the_state_write_cannot_replay_the_authorization() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Code("pending")]).await;
    let path = authorize(&fake, RETRY, &bound);
    *fake.fail_delete.lock().unwrap() = true;
    // The delete fails after the write: the run goes on as a retry run.
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert!(
        fake.lines()
            .contains(&"provision: retry: authorization_not_removed".to_owned())
    );
    assert!(path.exists());
    assert_eq!(fake.count("join"), 2);
    assert_eq!(
        fake.state().unwrap().consumed_retry_id.as_deref(),
        Some(RETRY)
    );
    // The file that stayed is a replay.
    let before = state_bytes(&fake);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(last_line(&fake), "provision: retry: retry_replayed");
    assert_eq!(state_bytes(&fake), before);
    assert_eq!(fake.count("join"), 2);
}

fn write_raw(path: &std::path::Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn good_json(bound: &Binding) -> String {
    format!(
        r#"{{"retry_id":"{RETRY}","terminal_id":"{}","state_digest":"{}"}}"#,
        bound.terminal_id, bound.state_digest
    )
}

/// An unusable authorization: `report_changed`, exit 2, the state is the
/// same bytes as before.
async fn refused(fake: &Fake, path: &std::path::Path, what: &str) {
    let before = std::fs::read(fake.state_file()).ok();
    assert_eq!(go_authorized(fake, path).await, Exit::Terminal, "{what}");
    assert_eq!(
        last_line(fake),
        "provision: retry: report_changed",
        "{what}"
    );
    assert_eq!(std::fs::read(fake.state_file()).ok(), before, "{what}");
}

#[tokio::test]
async fn every_unusable_authorization_is_report_changed_and_changes_nothing() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = auth_path(&fake);
    let good = good_json(&bound);
    let other_terminal = good.replace(&bound.terminal_id, &hex("77", 16));
    let other_digest = good.replace(&bound.state_digest, &hex("77", 32));
    let cases: Vec<(&str, Vec<u8>, u32)> = vec![
        ("not private", good.clone().into_bytes(), 0o644),
        (
            "over 1 KiB",
            format!("{good}{}", " ".repeat(1100)).into_bytes(),
            0o600,
        ),
        ("not JSON", b"junk".to_vec(), 0o600),
        (
            "unknown key",
            good.replace('}', r#","x":1}"#).into_bytes(),
            0o600,
        ),
        ("wrong form", good.replace(RETRY, "ABC").into_bytes(), 0o600),
        ("another terminal id", other_terminal.into_bytes(), 0o600),
        ("another digest", other_digest.into_bytes(), 0o600),
    ];
    // A missing file.
    refused(&fake, &path, "missing").await;
    for (what, bytes, mode) in cases {
        write_raw(&path, &bytes, mode);
        refused(&fake, &path, what).await;
        std::fs::remove_file(&path).unwrap();
    }
    // The stored binding is unusable.
    let stored = binding::binding_path(&fake.station_dir());
    write_raw(&path, good.as_bytes(), 0o600);
    std::fs::set_permissions(&stored, std::fs::Permissions::from_mode(0o644)).unwrap();
    refused(&fake, &path, "binding not private").await;
    write_raw(&stored, b"{}", 0o600);
    refused(&fake, &path, "binding invalid").await;
    std::fs::remove_file(&stored).unwrap();
    refused(&fake, &path, "binding missing").await;
    assert_eq!(fake.count("join"), 1);
    assert_eq!(fake.state().unwrap().consumed_retry_id, None);
}

#[tokio::test]
async fn a_stale_digest_with_a_current_looking_binding_is_report_changed() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    // The stored binding is of another condition than the state: the
    // supervisor never stored a binding for this one.
    let stale = binding::ensure(&fake.trust_root(), &fake.station_dir(), &hex("44", 32)).unwrap();
    let path = authorize(&fake, RETRY, &stale);
    refused(&fake, &path, "stale binding").await;
    assert_ne!(stale, bound);
}

#[tokio::test]
async fn no_state_and_no_retained_error_are_report_changed() {
    let fake = Fake::new();
    let any = Binding {
        terminal_id: hex("12", 16),
        state_digest: hex("34", 32),
    };
    fake.ensure_station_dir().unwrap();
    let path = authorize(&fake, RETRY, &any);
    refused(&fake, &path, "no state").await;
    assert!(fake.state().is_none());
    // A complete station has no retained error.
    assert_eq!(go(&fake, false).await, Exit::Complete);
    let path = authorize(&fake, RETRY, &any);
    refused(&fake, &path, "no error").await;
    assert_eq!(fake.state().unwrap().consumed_retry_id, None);
}

#[tokio::test]
async fn a_provisioning_file_that_is_not_valid_is_still_file_invalid() {
    let fake = Fake::new();
    let bound = stopped(&fake, &[JoinStep::Paired]).await;
    let path = authorize(&fake, RETRY, &bound);
    *fake.provision.lock().unwrap() = Ok(file_text("laptop")[1..].to_vec());
    let before = state_bytes(&fake);
    assert_eq!(go_authorized(&fake, &path).await, Exit::Terminal);
    assert_eq!(last_line(&fake), "provision: validated: file_invalid");
    assert_eq!(state_bytes(&fake), before);
    assert!(path.exists());
}
