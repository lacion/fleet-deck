//! Tests of the no-follow atomic writer (R13 P4 task C, decision 3).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::Path;

use super::fsx::{FsError, Home};

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("fsx").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().mode() & 0o777
}

#[test]
fn a_write_makes_the_directories_0755_and_the_file_with_its_mode() {
    let (dir, home) = home();
    home.write(Path::new("a/b/c.txt"), b"one", 0o644).unwrap();
    home.write(Path::new("a/b/run.sh"), b"#!/bin/sh\n", 0o755)
        .unwrap();
    assert_eq!(mode(&dir.path().join("a")), 0o755);
    assert_eq!(mode(&dir.path().join("a/b")), 0o755);
    assert_eq!(mode(&dir.path().join("a/b/c.txt")), 0o644);
    assert_eq!(mode(&dir.path().join("a/b/run.sh")), 0o755);
    home.write(Path::new("a/b/secret"), b"x", 0o600).unwrap();
    assert_eq!(mode(&dir.path().join("a/b/secret")), 0o600);
}

#[test]
fn a_write_replaces_the_file_and_leaves_no_temporary_file() {
    let (dir, home) = home();
    home.write(Path::new("f"), b"one", 0o644).unwrap();
    home.write(Path::new("f"), b"two", 0o644).unwrap();
    assert_eq!(std::fs::read(dir.path().join("f")).unwrap(), b"two");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("f")]);
}

#[test]
fn a_read_returns_the_bytes_and_the_mode_or_none() {
    let (_dir, home) = home();
    assert!(home.read(Path::new("x/y")).unwrap().is_none());
    home.write(Path::new("x/y"), b"data", 0o640).unwrap();
    let data = home.read(Path::new("x/y")).unwrap().unwrap();
    assert_eq!(data.bytes, b"data");
    assert_eq!(data.mode, 0o640);
}

#[test]
fn a_symlink_directory_is_refused_and_nothing_is_written_through_it() {
    let (dir, home) = home();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    symlink(outside.path(), dir.path().join("plugins")).unwrap();
    let err = home
        .write(
            Path::new("plugins/fleetdeck.fleet/manifest.json"),
            b"x",
            0o644,
        )
        .unwrap_err();
    assert!(matches!(err, FsError::Unsafe(_)), "{err}");
    assert!(
        err.to_string()
            .ends_with("is a link or is not yours; change nothing."),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(matches!(
        home.read(Path::new("plugins/x")).unwrap_err(),
        FsError::Unsafe(_)
    ));
}

#[test]
fn a_symlink_leaf_is_refused_for_a_read_and_a_write() {
    let (dir, home) = home();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let target = outside.path().join("victim");
    std::fs::write(&target, "keep").unwrap();
    symlink(&target, dir.path().join("leaf")).unwrap();
    assert!(matches!(
        home.read(Path::new("leaf")).unwrap_err(),
        FsError::Unsafe(_)
    ));
    assert!(matches!(
        home.write(Path::new("leaf"), b"new", 0o644).unwrap_err(),
        FsError::Unsafe(_)
    ));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
}

#[test]
fn the_config_home_itself_may_be_a_link() {
    let real = tempfile::Builder::new().prefix("real").tempdir().unwrap();
    let holder = tempfile::Builder::new().prefix("link").tempdir().unwrap();
    let link = holder.path().join("config");
    symlink(real.path(), &link).unwrap();
    let home = Home::open(&link).unwrap();
    home.write(Path::new("d/f"), b"ok", 0o644).unwrap();
    assert_eq!(std::fs::read(real.path().join("d/f")).unwrap(), b"ok");
}

#[test]
fn a_path_with_a_parent_component_is_refused() {
    let (_dir, home) = home();
    assert!(home.write(Path::new("../escape"), b"x", 0o644).is_err());
    assert!(home.read(Path::new("/etc/passwd")).is_err());
}

#[test]
fn remove_takes_a_file_and_remove_empty_dir_only_an_empty_directory() {
    let (dir, home) = home();
    home.write(Path::new("d/f"), b"x", 0o644).unwrap();
    assert!(!home.remove_empty_dir(Path::new("d")));
    assert!(home.remove(Path::new("d/f")).unwrap());
    assert!(!home.remove(Path::new("d/f")).unwrap());
    assert!(home.remove_empty_dir(Path::new("d")));
    assert!(!dir.path().join("d").exists());
    assert!(!home.remove_empty_dir(Path::new("d")));
}

#[test]
fn a_file_that_is_not_regular_is_refused() {
    let (dir, home) = home();
    std::fs::create_dir(dir.path().join("dirleaf")).unwrap();
    std::fs::set_permissions(
        dir.path().join("dirleaf"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        home.read(Path::new("dirleaf")).unwrap_err(),
        FsError::Unsafe(_)
    ));
    assert!(matches!(
        home.write(Path::new("dirleaf"), b"x", 0o644).unwrap_err(),
        FsError::Unsafe(_)
    ));
}
