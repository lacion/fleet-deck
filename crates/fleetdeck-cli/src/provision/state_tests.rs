//! The state file rules (design `omarchy-p8.md` 3.1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use super::effects::Effects;
use super::fake::{Fake, go, write_state};
use super::run::Exit;
use super::state::{self, Phase, ProvisionState, StateError};

fn sample() -> ProvisionState {
    ProvisionState {
        version: 1,
        config_sha256: "ab".repeat(32),
        station_fingerprint: Some("cd".repeat(32)),
        phase: Phase::Link,
        error: None,
        consumed_retry_id: None,
    }
}

const GOOD: &str = r#"{"version":1,"config_sha256":"x","station_fingerprint":null,"phase":"service","error":null}"#;

#[test]
fn an_absent_state_is_none() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    assert!(state::load(&fake.state_file()).unwrap().is_none());
}

#[test]
fn a_consumed_retry_id_is_stored_only_when_it_exists() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    let path = fake.state_file();
    state::save(&fake.trust_root(), &path, &sample()).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("consumed_retry_id"), "{text}");
    assert_eq!(state::load(&path).unwrap().unwrap().consumed_retry_id, None);
    let mut with = sample();
    with.consumed_retry_id = Some("ab".repeat(16));
    state::save(&fake.trust_root(), &path, &with).unwrap();
    assert_eq!(state::load(&path).unwrap(), Some(with));
}

#[test]
fn a_state_is_saved_privately_and_read_back() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    let path = fake.state_file();
    state::save(&fake.trust_root(), &path, &sample()).unwrap();
    assert_eq!(state::load(&path).unwrap(), Some(sample()));
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(r#""phase":"link""#), "{text}");
    // A second save replaces the first.
    let mut next = sample();
    next.phase = Phase::Complete;
    state::save(&fake.trust_root(), &path, &next).unwrap();
    assert_eq!(state::load(&path).unwrap(), Some(next));
}

#[test]
fn every_phase_has_its_snake_case_word() {
    for (phase, word) in [
        (Phase::Validated, "validated"),
        (Phase::Service, "service"),
        (Phase::Paired, "paired"),
        (Phase::Link, "link"),
        (Phase::Integration, "integration"),
        (Phase::Complete, "complete"),
    ] {
        assert_eq!(phase.word(), word);
        assert_eq!(
            serde_json::to_string(&phase).unwrap(),
            format!("\"{word}\"")
        );
    }
}

#[test]
fn the_paths_are_in_the_station_directory() {
    let dir = std::path::Path::new("/d");
    assert_eq!(
        state::state_path(dir),
        std::path::Path::new("/d/provision-state.json")
    );
    assert_eq!(
        state::lock_path(dir),
        std::path::Path::new("/d/provision.lock")
    );
}

#[test]
fn a_file_that_others_can_read_is_unsafe() {
    let fake = Fake::new();
    let path = write_state(&fake, GOOD.as_bytes(), 0o644);
    assert!(matches!(state::load(&path), Err(StateError::Unsafe)));
}

#[test]
fn a_symlink_is_unsafe() {
    let fake = Fake::new();
    let real = write_state(&fake, GOOD.as_bytes(), 0o600);
    let other = real.with_file_name("elsewhere.json");
    std::fs::rename(&real, &other).unwrap();
    symlink(&other, &real).unwrap();
    assert!(matches!(state::load(&real), Err(StateError::Unsafe)));
}

#[test]
fn bad_json_a_version_other_than_one_and_an_unknown_key_are_invalid() {
    let fake = Fake::new();
    for text in [
        "not json".to_owned(),
        GOOD.replace("\"version\":1", "\"version\":2"),
        GOOD.replace("\"error\":null", "\"error\":null,\"extra\":1"),
        GOOD.replace("\"service\"", "\"nowhere\""),
        "x".repeat(16 * 1024 + 1),
    ] {
        let path = write_state(&fake, text.as_bytes(), 0o600);
        assert!(
            matches!(state::load(&path), Err(StateError::Invalid)),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_bad_state_exits_1_with_state_invalid_and_the_file_is_unchanged() {
    for (text, mode) in [
        ("not json".to_owned(), 0o600),
        (GOOD.replace("\"version\":1", "\"version\":2"), 0o600),
        (GOOD.to_owned(), 0o644),
    ] {
        let fake = Fake::new();
        let path = write_state(&fake, text.as_bytes(), mode);
        assert_eq!(go(&fake, false).await, Exit::Unexpected);
        assert_eq!(go(&fake, true).await, Exit::Unexpected);
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, mode);
        assert_eq!(fake.lines()[0], "provision: validated: state_invalid");
        assert!(fake.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_symlinked_state_exits_1_and_nothing_is_written() {
    let fake = Fake::new();
    let real = write_state(&fake, GOOD.as_bytes(), 0o600);
    let other = real.with_file_name("elsewhere.json");
    std::fs::rename(&real, &other).unwrap();
    symlink(&other, &real).unwrap();
    assert_eq!(go(&fake, false).await, Exit::Unexpected);
    assert_eq!(std::fs::read(&other).unwrap(), GOOD.as_bytes());
    assert_eq!(std::fs::read_link(&real).unwrap(), other);
    assert_eq!(fake.lines()[0], "provision: validated: state_invalid");
    assert_eq!(Exit::Unexpected.code(), 1);
}

#[tokio::test]
async fn a_station_directory_that_is_not_private_is_exit_1() {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    std::fs::set_permissions(fake.station_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(go(&fake, false).await, Exit::Unexpected);
    assert_eq!(fake.lines(), ["provision: validated: unexpected_error"]);
    assert!(!fake.state_file().exists());
}

#[tokio::test]
async fn a_directory_that_cannot_be_prepared_is_exit_1() {
    let fake = Fake::new();
    *fake.dir_error.lock().unwrap() = true;
    assert_eq!(go(&fake, false).await, Exit::Unexpected);
    assert_eq!(fake.lines(), ["provision: validated: unexpected_error"]);
}

#[test]
fn the_phases_are_ordered_as_the_sequence() {
    use Phase::{Complete, Integration, Link, Paired, Service, Validated};
    let order = [Validated, Service, Paired, Link, Integration, Complete];
    assert!(order.windows(2).all(|w| w[0] < w[1]));
}
