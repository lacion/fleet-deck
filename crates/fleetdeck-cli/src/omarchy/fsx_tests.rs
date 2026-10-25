//! Tests of the no-follow atomic writer (R13 P4 task C, decision 3).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::Path;

use super::fsx::{FsError, Home, Kind};

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

#[test]
fn create_new_makes_the_file_once_and_never_replaces_it() {
    let (dir, home) = home();
    assert!(
        home.create_new(Path::new("a/b/once"), b"one", 0o644)
            .unwrap()
    );
    assert_eq!(mode(&dir.path().join("a/b/once")), 0o644);
    assert_eq!(mode(&dir.path().join("a/b")), 0o755);
    assert!(
        !home
            .create_new(Path::new("a/b/once"), b"two", 0o644)
            .unwrap()
    );
    assert_eq!(std::fs::read(dir.path().join("a/b/once")).unwrap(), b"one");
    let names: Vec<_> = std::fs::read_dir(dir.path().join("a/b"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("once")]);
}

#[test]
fn create_new_follows_no_link_at_the_leaf_or_above_it() {
    let (dir, home) = home();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let target = outside.path().join("victim");
    std::fs::write(&target, "keep").unwrap();
    symlink(&target, dir.path().join("leaf")).unwrap();
    assert!(!home.create_new(Path::new("leaf"), b"new", 0o644).unwrap());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    symlink(outside.path(), dir.path().join("plugins")).unwrap();
    assert!(matches!(
        home.create_new(Path::new("plugins/f"), b"x", 0o644)
            .unwrap_err(),
        FsError::Unsafe(_)
    ));
    assert!(!outside.path().join("f").exists());
}

#[test]
fn a_hard_linked_file_is_refused_for_a_read() {
    let (dir, home) = home();
    home.write(Path::new("one"), b"data", 0o644).unwrap();
    assert!(home.read(Path::new("one")).unwrap().is_some());
    std::fs::hard_link(dir.path().join("one"), dir.path().join("two")).unwrap();
    for name in ["one", "two"] {
        assert!(matches!(
            home.read(Path::new(name)).unwrap_err(),
            FsError::Unsafe(_)
        ));
    }
    std::fs::remove_file(dir.path().join("two")).unwrap();
    assert!(home.read(Path::new("one")).unwrap().is_some());
}

#[test]
fn a_fifo_is_refused_without_blocking_the_read() {
    let (dir, home) = home();
    let fifo = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !matches!(made, Ok(s) if s.success()) {
        return;
    }
    assert!(matches!(
        home.read(Path::new("pipe")).unwrap_err(),
        FsError::Unsafe(_)
    ));
}

#[test]
fn a_read_returns_the_identity_of_the_file() {
    let (dir, home) = home();
    home.write(Path::new("f"), b"12345", 0o640).unwrap();
    let data = home.read(Path::new("f")).unwrap().unwrap();
    let meta = std::fs::metadata(dir.path().join("f")).unwrap();
    assert_eq!((data.dev, data.ino, data.size), (meta.dev(), meta.ino(), 5));
    assert_eq!(data.full_mode, 0o640);
}

#[test]
fn lstat_tells_the_kind_without_following_a_link() {
    let (dir, home) = home();
    home.write(Path::new("d/f"), b"x", 0o644).unwrap();
    symlink(dir.path().join("d/f"), dir.path().join("link")).unwrap();
    let file = home.lstat(Path::new("d/f")).unwrap().unwrap();
    assert_eq!(file.kind, Kind::Regular);
    assert!(file.is_mine() && file.single_link);
    assert_eq!((file.mode, file.size), (0o644, 1));
    assert_eq!(
        home.lstat(Path::new("d")).unwrap().unwrap().kind,
        Kind::Directory
    );
    assert_eq!(
        home.lstat(Path::new("link")).unwrap().unwrap().kind,
        Kind::Link
    );
    assert!(home.lstat(Path::new("d/none")).unwrap().is_none());
    assert!(home.lstat(Path::new("none/f")).unwrap().is_none());
    std::fs::hard_link(dir.path().join("d/f"), dir.path().join("d/g")).unwrap();
    assert!(!home.lstat(Path::new("d/g")).unwrap().unwrap().single_link);
    symlink(dir.path().join("d"), dir.path().join("dlink")).unwrap();
    assert!(matches!(
        home.lstat(Path::new("dlink/f")).unwrap_err(),
        FsError::Unsafe(_)
    ));
}

#[test]
fn list_gives_the_sorted_names_and_none_for_a_missing_directory() {
    let (dir, home) = home();
    home.write(Path::new("d/b"), b"x", 0o644).unwrap();
    home.write(Path::new("d/a"), b"x", 0o644).unwrap();
    assert_eq!(
        home.list(Path::new("d")).unwrap(),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(home.list(Path::new("none")).unwrap(), None);
    symlink(dir.path().join("d"), dir.path().join("dlink")).unwrap();
    assert!(matches!(
        home.list(Path::new("dlink")).unwrap_err(),
        FsError::Unsafe(_)
    ));
}

#[test]
fn private_dir_makes_the_directory_0700_and_the_ones_above_it_0755() {
    let (dir, home) = home();
    home.private_dir(Path::new("a/b/private")).unwrap();
    assert_eq!(mode(&dir.path().join("a/b/private")), 0o700);
    assert_eq!(mode(&dir.path().join("a/b")), 0o755);
    // An existing directory of yours is set to 0700; again is fine.
    std::fs::set_permissions(
        dir.path().join("a/b/private"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    home.private_dir(Path::new("a/b/private")).unwrap();
    assert_eq!(mode(&dir.path().join("a/b/private")), 0o700);
    // A file or a link in the way is refused.
    std::fs::write(dir.path().join("file"), "x").unwrap();
    assert!(home.private_dir(Path::new("file")).is_err());
    symlink(dir.path().join("a"), dir.path().join("alink")).unwrap();
    assert!(home.private_dir(Path::new("alink")).is_err());
}

fn chmod(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

#[test]
fn others_can_write_checks_the_file_and_each_directory_from_the_first_component() {
    let (dir, home) = home();
    let rel = Path::new("a/b/f");
    home.write(rel, b"x", 0o644).unwrap();
    assert!(!home.others_can_write(rel).unwrap());
    // A missing file or directory adds nothing.
    assert!(!home.others_can_write(Path::new("a/b/none")).unwrap());
    assert!(!home.others_can_write(Path::new("a/none/f")).unwrap());
    // The file: a group bit or an other bit.
    for bits in [0o664, 0o646] {
        chmod(&dir.path().join("a/b/f"), bits);
        assert!(home.others_can_write(rel).unwrap(), "{bits:o}");
    }
    chmod(&dir.path().join("a/b/f"), 0o644);
    // The first directory, a middle one, and the parent.
    for d in ["a", "a/b"] {
        chmod(&dir.path().join(d), 0o775);
        assert!(home.others_can_write(rel).unwrap(), "{d}");
        assert!(home.others_can_write(Path::new("a/b/none")).unwrap(), "{d}");
        chmod(&dir.path().join(d), 0o755);
    }
    assert!(!home.others_can_write(rel).unwrap());
    // The config home itself is not checked.
    chmod(dir.path(), 0o777);
    assert!(!home.others_can_write(rel).unwrap());
}

#[test]
fn create_new_held_gives_the_descriptor_and_none_for_a_taken_name() {
    let (dir, home) = home();
    let rel = Path::new("a/held");
    let fd = home.create_new_held(rel, b"one", 0o644).unwrap().unwrap();
    assert_eq!(std::fs::read(dir.path().join(rel)).unwrap(), b"one");
    assert!(home.create_new_held(rel, b"two", 0o644).unwrap().is_none());
    assert_eq!(std::fs::read(dir.path().join(rel)).unwrap(), b"one");
    // The same file by name: its bytes come back.
    let data = home.read_same(rel, &fd).unwrap().unwrap();
    assert_eq!(data.bytes, b"one");
}

#[test]
fn read_same_refuses_another_file_a_loose_mode_a_second_name_and_a_link() {
    let (dir, home) = home();
    let rel = Path::new("a/held");
    let path = dir.path().join(rel);
    let fd = home.create_new_held(rel, b"one", 0o644).unwrap().unwrap();
    // A group write bit.
    chmod(&path, 0o664);
    assert!(home.read_same(rel, &fd).unwrap().is_none());
    chmod(&path, 0o644);
    assert!(home.read_same(rel, &fd).unwrap().is_some());
    // A second name.
    std::fs::hard_link(&path, dir.path().join("a/other")).unwrap();
    assert!(home.read_same(rel, &fd).unwrap().is_none());
    std::fs::remove_file(dir.path().join("a/other")).unwrap();
    // Another file under the same name (another inode).
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"one").unwrap();
    chmod(&path, 0o644);
    assert!(home.read_same(rel, &fd).unwrap().is_none());
    // A link under the name.
    std::fs::remove_file(&path).unwrap();
    symlink(dir.path().join("a"), &path).unwrap();
    assert!(home.read_same(rel, &fd).unwrap().is_none());
    // Nothing under the name.
    std::fs::remove_file(&path).unwrap();
    assert!(home.read_same(rel, &fd).unwrap().is_none());
}
