//! Tests of the state file read and the lock probe (design
//! `omarchy-admin.md` §3.2, §3.3).

use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use rustix::fs::{CWD, FileType, FlockOperation, Mode, flock, mknodat};

use super::provision::{
    LOCK_FILE, STATE_FILE, StateRead, bootstrap_running, open_private, read_state, view,
};

fn state(dir: &Path, text: &str, mode: u32) {
    let path = dir.join(STATE_FILE);
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

fn found(phase: &str, error: Option<&str>) -> StateRead {
    StateRead::Found {
        phase: phase.to_owned(),
        error: error.map(str::to_owned),
    }
}

#[test]
fn omarchy_a_missing_state_file_is_absent_and_has_no_view() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_state(dir.path()), StateRead::Absent);
    assert_eq!(view(dir.path(), None), None);
}

#[test]
fn omarchy_a_good_state_file_gives_its_phase_and_error() {
    let dir = tempfile::tempdir().unwrap();
    state(
        dir.path(),
        r#"{"version":1,"config_sha256":"x","station_fingerprint":"y","phase":"link","error":"denied","more":[1]}"#,
        0o600,
    );
    assert_eq!(read_state(dir.path()), found("link", Some("denied")));
    state(dir.path(), r#"{"phase":"complete","error":null}"#, 0o644);
    assert_eq!(read_state(dir.path()), found("complete", None));
    state(dir.path(), r#"{"phase":"paired"}"#, 0o600);
    assert_eq!(read_state(dir.path()), found("paired", None));
}

#[test]
fn omarchy_an_unknown_error_code_becomes_unknown_error() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["client_gone", "", "Denied", "denied "] {
        state(
            dir.path(),
            &format!(r#"{{"phase":"link","error":"{bad}"}}"#),
            0o600,
        );
        assert_eq!(
            read_state(dir.path()),
            found("link", Some("unknown_error")),
            "{bad:?}"
        );
    }
    for good in super::provision::TERMINAL_CODES {
        state(
            dir.path(),
            &format!(r#"{{"phase":"link","error":"{good}"}}"#),
            0o600,
        );
        assert_eq!(read_state(dir.path()), found("link", Some(good)));
    }
    assert_eq!(super::provision::TERMINAL_CODES.len(), 21);
}

#[test]
fn omarchy_a_bad_phase_or_a_wrong_type_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    for text in [
        r#"{"phase":"launching","error":null}"#,
        r#"{"phase":3,"error":null}"#,
        r#"{"phase":"link","error":7}"#,
        r#"{"error":null}"#,
        "not json",
        "",
        "[]",
    ] {
        state(dir.path(), text, 0o600);
        assert_eq!(read_state(dir.path()), StateRead::Unreadable, "{text}");
    }
    let view = view(dir.path(), Some("failed")).unwrap();
    assert_eq!(view.phase, None);
    assert_eq!(view.error.as_deref(), Some("unknown_error"));
    assert_eq!(view.last_retry.as_deref(), Some("failed"));
}

#[test]
fn omarchy_a_state_file_over_16_kib_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let pad = " ".repeat(16 * 1024 - 30);
    let ok = format!(r#"{{"phase":"link","error":null}}{pad}"#);
    assert!(ok.len() <= 16 * 1024);
    state(dir.path(), &ok, 0o600);
    assert_eq!(read_state(dir.path()), found("link", None));
    state(dir.path(), &format!("{ok}{}", " ".repeat(40)), 0o600);
    assert_eq!(read_state(dir.path()), StateRead::Unreadable);
}

#[test]
fn omarchy_a_state_file_that_others_can_write_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    for mode in [0o620, 0o602, 0o666, 0o664] {
        state(dir.path(), r#"{"phase":"link","error":null}"#, mode);
        assert_eq!(read_state(dir.path()), StateRead::Unreadable, "{mode:o}");
    }
}

#[test]
fn omarchy_a_file_of_another_owner_is_refused() {
    if rustix::process::getuid().is_root() {
        return;
    }
    // `/etc/passwd` is a regular file of root that nobody else can write.
    assert!(open_private(Path::new("/etc/passwd"), None).is_err());
}

#[test]
fn omarchy_a_link_or_a_fifo_or_a_directory_is_unreadable_and_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    fs::write(&real, r#"{"phase":"link","error":null}"#).unwrap();
    symlink(&real, dir.path().join(STATE_FILE)).unwrap();
    assert_eq!(read_state(dir.path()), StateRead::Unreadable);
    fs::remove_file(dir.path().join(STATE_FILE)).unwrap();
    // A FIFO with no writer: a blocking open or read would hang here.
    mknodat(
        CWD,
        dir.path().join(STATE_FILE),
        FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    assert_eq!(read_state(dir.path()), StateRead::Unreadable);
    fs::remove_file(dir.path().join(STATE_FILE)).unwrap();
    fs::create_dir(dir.path().join(STATE_FILE)).unwrap();
    assert_eq!(read_state(dir.path()), StateRead::Unreadable);
}

#[test]
fn omarchy_the_lock_probe_sees_a_holder_and_releases_at_once() {
    let dir = tempfile::tempdir().unwrap();
    // No lock file: not running.
    assert!(!bootstrap_running(dir.path()));
    let path = dir.path().join(LOCK_FILE);
    fs::write(&path, "").unwrap();
    // Free: not running, and the probe does not keep the lock.
    assert!(!bootstrap_running(dir.path()));
    assert!(!bootstrap_running(dir.path()));
    let holder = File::open(&path).unwrap();
    flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert!(bootstrap_running(dir.path()));
    drop(holder);
    assert!(!bootstrap_running(dir.path()));
}

#[test]
fn omarchy_the_lock_probe_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(LOCK_FILE);
    // A group-writable lock file.
    fs::write(&path, "").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
    assert!(bootstrap_running(dir.path()));
    fs::remove_file(&path).unwrap();
    // A link.
    let real = dir.path().join("real.lock");
    fs::write(&real, "").unwrap();
    symlink(&real, &path).unwrap();
    assert!(bootstrap_running(dir.path()));
    fs::remove_file(&path).unwrap();
    // A FIFO.
    mknodat(CWD, &path, FileType::Fifo, Mode::from_raw_mode(0o600), 0).unwrap();
    assert!(bootstrap_running(dir.path()));
    fs::remove_file(&path).unwrap();
    // A directory.
    fs::create_dir(&path).unwrap();
    assert!(bootstrap_running(dir.path()));
}

#[test]
fn omarchy_the_view_reports_the_lock_and_the_last_retry() {
    let dir = tempfile::tempdir().unwrap();
    state(dir.path(), r#"{"phase":"integration","error":null}"#, 0o600);
    let path = dir.path().join(LOCK_FILE);
    fs::write(&path, "").unwrap();
    let holder = File::open(&path).unwrap();
    flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();
    let v = view(dir.path(), Some("running")).unwrap();
    assert_eq!(v.phase.as_deref(), Some("integration"));
    assert_eq!(v.error, None);
    assert!(v.bootstrap_running);
    assert_eq!(v.last_retry.as_deref(), Some("running"));
}
