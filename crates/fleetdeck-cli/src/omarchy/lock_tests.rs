//! Tests of the Omarchy lock.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt as _;

use super::*;

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("lock").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn held(take: Take) -> LockGuard {
    match take {
        Take::Held(guard) => guard,
        other => panic!("the lock is not held: {other:?}"),
    }
}

#[test]
fn lock_without_create_and_without_a_file_is_no_lock_file() {
    let (dir, home) = home();
    assert!(matches!(
        take(&home, Duration::ZERO, false).unwrap(),
        Take::NoLockFile
    ));
    // The folder exists, the file does not.
    std::fs::create_dir(dir.path().join("fleetdeck")).unwrap();
    assert!(matches!(
        take(&home, Duration::ZERO, false).unwrap(),
        Take::NoLockFile
    ));
    assert!(!dir.path().join(LOCK_PATH).exists());
}

#[test]
fn lock_with_create_makes_the_folder_and_a_0600_file() {
    let (dir, home) = home();
    let _guard = held(take(&home, Duration::ZERO, true).unwrap());
    let meta = std::fs::metadata(dir.path().join(LOCK_PATH)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let folder = std::fs::metadata(dir.path().join("fleetdeck")).unwrap();
    assert_eq!(folder.permissions().mode() & 0o777, 0o755);
}

#[test]
fn lock_a_second_taker_gets_busy_after_the_wait() {
    let (_dir, home) = home();
    let first = held(take(&home, Duration::ZERO, true).unwrap());
    let started = Instant::now();
    let second = take(&home, Duration::from_millis(250), false).unwrap();
    assert!(matches!(second, Take::Busy), "{second:?}");
    assert!(started.elapsed() >= Duration::from_millis(250));
    // No wait: Busy at once.
    assert!(matches!(
        take(&home, Duration::ZERO, false).unwrap(),
        Take::Busy
    ));
    drop(first);
    assert!(matches!(
        take(&home, Duration::ZERO, false).unwrap(),
        Take::Held(_)
    ));
}

#[test]
fn lock_a_waiting_taker_gets_it_when_the_holder_lets_go() {
    let (_dir, home) = home();
    let first = held(take(&home, Duration::ZERO, true).unwrap());
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(first);
    });
    let second = take(&home, Duration::from_secs(5), false).unwrap();
    assert!(matches!(second, Take::Held(_)), "{second:?}");
    releaser.join().unwrap();
}

#[test]
fn lock_refuses_a_link_as_the_lock_file() {
    let (dir, home) = home();
    std::fs::create_dir(dir.path().join("fleetdeck")).unwrap();
    std::fs::write(dir.path().join("victim"), "keep").unwrap();
    std::os::unix::fs::symlink(dir.path().join("victim"), dir.path().join(LOCK_PATH)).unwrap();
    assert!(take(&home, Duration::ZERO, true).is_err());
    assert!(take(&home, Duration::ZERO, false).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("victim")).unwrap(),
        "keep"
    );
}
