#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;

fn tree() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("a/b")).unwrap();
    fs::write(d.path().join("a/b/f.txt"), "one").unwrap();
    fs::write(d.path().join("g.txt"), "two").unwrap();
    symlink("g.txt", d.path().join("l")).unwrap();
    d
}

#[test]
fn the_digest_is_stable_for_one_tree() {
    let d = tree();
    let first = tree_digest(d.path()).unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(first, tree_digest(d.path()).unwrap());
}

#[test]
fn the_digest_changes_with_content() {
    let d = tree();
    let before = tree_digest(d.path()).unwrap();
    fs::write(d.path().join("a/b/f.txt"), "uno").unwrap();
    assert_ne!(before, tree_digest(d.path()).unwrap());
}

#[test]
fn the_digest_changes_with_mode() {
    let d = tree();
    let before = tree_digest(d.path()).unwrap();
    fs::set_permissions(d.path().join("g.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(before, tree_digest(d.path()).unwrap());
}

#[test]
fn the_digest_changes_with_link_text_and_does_not_follow_links() {
    let d = tree();
    let before = tree_digest(d.path()).unwrap();
    // The link target content changes: the digest of the link does not.
    let other = tempfile::tempdir().unwrap();
    fs::write(other.path().join("x"), "1").unwrap();
    symlink(other.path(), d.path().join("out")).unwrap();
    let with_link = tree_digest(d.path()).unwrap();
    fs::write(other.path().join("x"), "2").unwrap();
    fs::write(other.path().join("y"), "3").unwrap();
    assert_eq!(with_link, tree_digest(d.path()).unwrap());
    // The link text changes.
    fs::remove_file(d.path().join("l")).unwrap();
    symlink("a", d.path().join("l")).unwrap();
    assert_ne!(before, tree_digest(d.path()).unwrap());
}

#[test]
fn a_missing_folder_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(tree_digest(&d.path().join("none")).is_err());
}
