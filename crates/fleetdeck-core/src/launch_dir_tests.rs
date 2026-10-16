//! Tests of [`LaunchDir`] (B9).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::symlink;

use super::*;

/// A canonical temp root (the temp dir itself may sit below a link).
fn root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    (tmp, root)
}

#[test]
fn a_plain_dir_opens_with_its_identity() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    let dir = LaunchDir::open(&root, Path::new("a/b")).unwrap();
    let meta = std::fs::metadata(root.join("a/b")).unwrap();
    let id = dir.identity();
    assert_eq!(id.root, root.to_str().unwrap());
    assert_eq!(id.rel, "a/b");
    assert_eq!((id.dev, id.ino), (meta.dev(), meta.ino()));
    assert_eq!(dir.path(), root.join("a/b"));
    let top = LaunchDir::open(&root, Path::new("")).unwrap();
    assert_eq!(top.identity().rel, "");
    assert_eq!(top.path(), root);
}

#[test]
fn a_link_as_the_last_component_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("real")).unwrap();
    symlink(root.join("real"), root.join("link")).unwrap();
    let err = LaunchDir::open(&root, Path::new("link")).unwrap_err();
    assert!(matches!(err, LaunchDirError::Changed), "{err:?}");
    assert_eq!(err.code(), "launch_dir_changed");
    assert_eq!(err.to_string(), CHANGED);
}

#[test]
fn a_link_as_an_inner_component_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("real/sub")).unwrap();
    symlink(root.join("real"), root.join("link")).unwrap();
    let err = LaunchDir::open(&root, Path::new("link/sub")).unwrap_err();
    assert!(matches!(err, LaunchDirError::Changed), "{err:?}");
}

#[test]
fn a_link_in_the_root_path_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("real/sub")).unwrap();
    symlink(root.join("real"), root.join("link")).unwrap();
    let err = LaunchDir::open(&root.join("link"), Path::new("sub")).unwrap_err();
    assert!(matches!(err, LaunchDirError::Changed), "{err:?}");
}

#[test]
fn a_rel_with_dotdot_or_a_missing_folder_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a")).unwrap();
    for rel in ["a/../a", "..", "/abs", "./a", "missing"] {
        let err = LaunchDir::open(&root, Path::new(rel)).unwrap_err();
        assert!(matches!(err, LaunchDirError::Changed), "{rel}: {err:?}");
    }
}

#[test]
fn reopen_of_the_same_dir_works() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let dir = LaunchDir::open(&root, Path::new("a")).unwrap();
    let again = LaunchDir::reopen(dir.identity()).unwrap();
    assert_eq!(again.identity(), dir.identity());
}

#[test]
fn reopen_after_a_rename_and_a_link_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    let dir = LaunchDir::open(&root, Path::new("a")).unwrap();
    std::fs::rename(root.join("a"), root.join("moved")).unwrap();
    symlink(root.join("other"), root.join("a")).unwrap();
    let err = LaunchDir::reopen(dir.identity()).unwrap_err();
    assert!(matches!(err, LaunchDirError::Changed), "{err:?}");
}

#[test]
fn reopen_after_a_replacement_by_another_dir_fails() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let dir = LaunchDir::open(&root, Path::new("a")).unwrap();
    std::fs::rename(root.join("a"), root.join("moved")).unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let err = LaunchDir::reopen(dir.identity()).unwrap_err();
    assert!(matches!(err, LaunchDirError::Changed), "{err:?}");
}

#[test]
fn a_child_starts_in_the_original_dir_after_a_swap() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    let dir = LaunchDir::open(&root, Path::new("a")).unwrap();
    std::fs::rename(root.join("a"), root.join("moved")).unwrap();
    symlink(root.join("other"), root.join("a")).unwrap();
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", "pwd -P"])
        .current_dir(dir.cwd().unwrap())
        .output()
        .unwrap();
    assert!(out.status.success());
    let printed = String::from_utf8(out.stdout).unwrap();
    assert_eq!(printed.trim_end(), root.join("moved").to_str().unwrap());
}

#[test]
fn open_in_picks_the_longest_root() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("outer/inner/x")).unwrap();
    let roots = vec![root.join("outer"), root.join("outer/inner")];
    let dir = LaunchDir::open_in(&roots, &root.join("outer/inner/x")).unwrap();
    assert_eq!(
        dir.identity().root,
        root.join("outer/inner").to_str().unwrap()
    );
    assert_eq!(dir.identity().rel, "x");
    let only = LaunchDir::open_in(&roots[..1], &root.join("outer/inner/x")).unwrap();
    assert_eq!(only.identity().rel, "inner/x");
    let none = LaunchDir::open_in(&roots, &root.join("elsewhere")).unwrap_err();
    assert!(matches!(none, LaunchDirError::Changed), "{none:?}");
}

#[test]
fn the_texts_and_codes_are_stable() {
    assert_eq!(LaunchDirError::Unavailable.code(), "launch_dir_unavailable");
    assert_eq!(LaunchDirError::Unsupported.code(), "launch_dir_unsupported");
    assert_eq!(LaunchDirError::Unavailable.to_string(), UNAVAILABLE);
    assert_eq!(LaunchDirError::Unsupported.to_string(), UNSUPPORTED);
}

#[test]
fn the_path_of_the_root_itself_has_no_trailing_slash() {
    let (_tmp, root) = root();
    let dir = LaunchDir::open(&root, Path::new("")).unwrap();
    assert_eq!(dir.path(), root);
    assert!(!dir.path().to_string_lossy().ends_with('/'));
}
