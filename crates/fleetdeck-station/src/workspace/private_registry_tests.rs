//! The inode registry of the private paths (PRE-PR r2 finding 1): the
//! file, the cap, the failures. The routes are tested in
//! `manager/files_private_tests.rs` and `artifacts/private_tests.rs`.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use super::Registry;

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_entry_is_one_line_in_a_0600_file_and_is_loaded_again() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("private-inodes");
    let r = Registry::load(file.clone());
    assert!(r.is_empty());
    r.add((7, 100));
    r.add((7, 101));
    // The same identity is one line.
    r.add((7, 100));
    assert_eq!(lines(&file), ["7:100", "7:101"]);
    assert_eq!(mode(&file), 0o600);
    let again = Registry::load(file);
    assert!(again.contains((7, 100)) && again.contains((7, 101)));
    assert!(!again.contains((7, 102)));
    assert_eq!(again.len(), 2);
}

#[test]
fn a_file_register_takes_the_inode_of_the_opened_file_and_leaves_a_link_out() {
    let tmp = tempfile::tempdir().unwrap();
    let r = Registry::load(tmp.path().join("private-inodes"));
    let real = tmp.path().join("real");
    std::fs::write(&real, "x").unwrap();
    let meta = std::fs::metadata(&real).unwrap();
    r.register(&real);
    assert!(r.contains((meta.dev(), meta.ino())));
    // A symlink is not followed: its target is not registered through it.
    let other = tmp.path().join("other");
    std::fs::write(&other, "y").unwrap();
    let other_meta = std::fs::metadata(&other).unwrap();
    std::os::unix::fs::symlink(&other, tmp.path().join("sym")).unwrap();
    r.register(&tmp.path().join("sym"));
    assert!(!r.contains((other_meta.dev(), other_meta.ino())));
    // A directory and a missing path add nothing.
    r.register(tmp.path());
    r.register(&tmp.path().join("missing"));
    assert_eq!(r.len(), 1);
}

#[test]
fn a_directory_register_takes_every_regular_file_below_it() {
    let tmp = tempfile::tempdir().unwrap();
    let r = Registry::load(tmp.path().join("private-inodes"));
    let dir = tmp.path().join("secrets");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a"), "a").unwrap();
    std::fs::write(dir.join("sub/b"), "b").unwrap();
    std::os::unix::fs::symlink(tmp.path(), dir.join("loop")).unwrap();
    r.register_dir(&dir);
    for name in ["a", "sub/b"] {
        let meta = std::fs::metadata(dir.join(name)).unwrap();
        assert!(r.contains((meta.dev(), meta.ino())), "{name}");
    }
    assert_eq!(r.len(), 2);
    // A missing directory is not an error.
    r.register_dir(&tmp.path().join("missing"));
    assert_eq!(r.len(), 2);
}

#[test]
fn over_the_cap_the_oldest_half_is_dropped_and_the_file_is_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("private-inodes");
    let r = Registry::load_with_cap(file.clone(), 4);
    for ino in 1..=4 {
        r.add((1, ino));
    }
    assert_eq!(r.len(), 4);
    r.add((1, 5));
    // 5 entries, cap 4: the oldest 2 are dropped.
    assert_eq!(r.len(), 3);
    assert!(!r.contains((1, 1)) && !r.contains((1, 2)));
    assert!(r.contains((1, 3)) && r.contains((1, 4)) && r.contains((1, 5)));
    assert_eq!(lines(&file), ["1:3", "1:4", "1:5"]);
    assert_eq!(mode(&file), 0o600);
    r.add((1, 6));
    assert_eq!(lines(&file), ["1:3", "1:4", "1:5", "1:6"]);
}

#[test]
fn a_file_over_the_cap_is_trimmed_at_load_and_a_bad_line_is_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("private-inodes");
    std::fs::write(&file, "1:1\n1:2\nnot a line\n1:3\n1:4\n1:5\n\n9:x\n").unwrap();
    let r = Registry::load_with_cap(file.clone(), 4);
    // 5 entries, cap 4: the oldest 2 are dropped.
    assert_eq!(r.len(), 3);
    assert!(r.contains((1, 3)) && r.contains((1, 4)) && r.contains((1, 5)));
    assert_eq!(lines(&file), ["1:3", "1:4", "1:5"]);
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn a_file_that_cannot_be_written_keeps_the_entry_in_memory() {
    let tmp = tempfile::tempdir().unwrap();
    // The directory of the file does not exist.
    let r = Registry::load(tmp.path().join("missing/private-inodes"));
    r.add((3, 33));
    assert!(r.contains((3, 33)));
    // A file that is a link is not followed: the entry stays in memory.
    let target = tmp.path().join("target");
    std::fs::write(&target, "").unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let r = Registry::load(link);
    r.add((4, 44));
    assert!(r.contains((4, 44)));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
}

#[test]
fn a_credential_directory_walk_stops_at_the_depth_and_at_the_file_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let r = Registry::load(tmp.path().join("private-inodes"));
    let dir = tmp.path().join("cred");
    let deep = dir.join("a/b/c/d/e");
    std::fs::create_dir_all(&deep).unwrap();
    let id = |p: &Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.dev(), m.ino())
    };
    let ok = dir.join("a/b/c/d/ok");
    let too_deep = deep.join("deep");
    std::fs::write(&ok, "x").unwrap();
    std::fs::write(&too_deep, "x").unwrap();
    r.register_credential_dir(&dir);
    assert!(r.contains(id(&ok)));
    assert!(!r.contains(id(&too_deep)));
    // The file budget: a directory over it registers the budget only.
    let many = tmp.path().join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..5 {
        std::fs::write(many.join(format!("f{i}")), "x").unwrap();
    }
    let other = Registry::load(tmp.path().join("private-inodes-2"));
    other.register_credential_dir_with(&many, 4, 3);
    assert_eq!(other.len(), 3);
}
