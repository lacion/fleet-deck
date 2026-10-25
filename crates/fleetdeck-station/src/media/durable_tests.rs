//! The durable removal: the order of the syncs, a failed sync, a link, a
//! missing folder and a bad name.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;
use std::sync::Mutex;

use super::*;

/// A `DurableFs` that records the inode of every synced folder and fails
/// the `fail_at`-th `sync_dir` call (counted from 1) when it is set.
struct Recording {
    dirs: Mutex<Vec<u64>>,
    fail_at: Option<usize>,
}

impl Recording {
    fn new(fail_at: Option<usize>) -> Self {
        Self {
            dirs: Mutex::new(Vec::new()),
            fail_at,
        }
    }

    fn dirs(&self) -> Vec<u64> {
        self.dirs.lock().unwrap().clone()
    }
}

impl DurableFs for Recording {
    fn sync_file(&self, _: &File) -> io::Result<()> {
        Ok(())
    }

    fn sync_dir(&self, d: &OwnedFd) -> io::Result<()> {
        let ino = rustix::fs::fstat(d).map_err(io::Error::from)?.st_ino;
        let mut dirs = self.dirs.lock().unwrap();
        dirs.push(ino);
        if self.fail_at == Some(dirs.len()) {
            return Err(io::Error::other("injected sync failure"));
        }
        Ok(())
    }
}

fn open_dir(path: &Path) -> OwnedFd {
    rustix::fs::open(path, DIR_FLAGS, Mode::empty()).unwrap()
}

fn ino(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().ino()
}

/// `<root>/top` with `a/b/deep`, `a/mid`, `c/` (empty) and `top-file`.
fn tree(root: &Path) {
    let top = root.join("top");
    std::fs::create_dir_all(top.join("a/b")).unwrap();
    std::fs::create_dir(top.join("c")).unwrap();
    std::fs::write(top.join("a/b/deep"), b"1").unwrap();
    std::fs::write(top.join("a/mid"), b"2").unwrap();
    std::fs::write(top.join("top-file"), b"3").unwrap();
}

#[test]
fn a_nested_tree_is_removed_and_every_folder_and_the_parent_are_synced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    let top = root.join("top");
    let mut expected = vec![
        ino(&top),
        ino(&top.join("a")),
        ino(&top.join("a/b")),
        ino(&top.join("c")),
        ino(&root),
    ];
    let fs = Recording::new(None);
    remove_tree_durable(&open_dir(&root), "top", &fs).unwrap();
    assert!(!top.exists());
    let mut synced = fs.dirs();
    // The parent is synced last; each folder is synced after its content.
    assert_eq!(synced.last(), Some(&ino(&root)));
    synced.sort_unstable();
    expected.sort_unstable();
    assert_eq!(synced, expected);
}

#[test]
fn a_folder_is_synced_after_its_children_and_before_its_own_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    let top = root.join("top");
    let (top_ino, deep_dir) = (ino(&top), ino(&top.join("a/b")));
    let fs = Recording::new(None);
    remove_tree_durable(&open_dir(&root), "top", &fs).unwrap();
    let synced = fs.dirs();
    let at = |n: u64| synced.iter().position(|i| *i == n).unwrap();
    assert!(at(deep_dir) < at(top_ino));
    assert!(at(top_ino) < at(ino(&root)));
}

#[test]
fn a_failed_directory_sync_is_an_error_and_the_caller_stays_not_done() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    let fs = Recording::new(Some(1));
    let mut done = false;
    let result = remove_tree_durable(&open_dir(&root), "top", &fs);
    if result.is_ok() {
        done = true;
    }
    assert!(result.is_err());
    assert!(!done);
    // The failure stopped the removal: the tree is still there for a retry.
    assert!(root.join("top").exists());
    remove_tree_durable(&open_dir(&root), "top", &Recording::new(None)).unwrap();
    assert!(!root.join("top").exists());
}

#[test]
fn a_failed_sync_of_the_parent_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    // The tree has 4 folders (top, a, a/b, c): the parent is the 5th sync.
    let fs = Recording::new(Some(5));
    let e = remove_tree_durable(&open_dir(&root), "top", &fs).unwrap_err();
    assert_eq!(e.to_string(), "injected sync failure");
    assert_eq!(fs.dirs().len(), 5);
}

#[test]
fn a_link_inside_is_an_error_and_its_target_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"keep").unwrap();
    symlink(&outside, root.join("top/c/dir-link")).unwrap();
    let e = remove_tree_durable(&open_dir(&root), "top", &Recording::new(None)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
    assert!(root.join("top/c/dir-link").symlink_metadata().is_ok());
}

#[test]
fn a_link_to_a_file_inside_is_an_error_and_its_target_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    tree(&root);
    std::fs::write(root.join("keep-file"), b"keep").unwrap();
    symlink(root.join("keep-file"), root.join("top/a/file-link")).unwrap();
    let e = remove_tree_durable(&open_dir(&root), "top", &Recording::new(None)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(root.join("keep-file")).unwrap(), b"keep");
    assert!(root.join("top/a/file-link").symlink_metadata().is_ok());
}

#[test]
fn a_link_as_the_folder_is_an_error_and_its_target_is_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"keep").unwrap();
    symlink(&outside, root.join("top")).unwrap();
    assert!(remove_tree_durable(&open_dir(&root), "top", &Recording::new(None)).is_err());
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
}

#[test]
fn a_missing_name_is_done_after_a_sync_of_the_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let fs = Recording::new(None);
    remove_tree_durable(&open_dir(&root), "gone", &fs).unwrap();
    assert_eq!(fs.dirs(), [ino(&root)]);
    // The sync of the parent can still fail: the removal is then not done.
    assert!(remove_tree_durable(&open_dir(&root), "gone", &Recording::new(Some(1))).is_err());
}

#[test]
fn a_name_that_is_not_one_plain_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let fs = Recording::new(None);
    for name in ["", ".", "..", "a/b", "../x"] {
        let e = remove_tree_durable(&open_dir(&root), name, &fs).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{name}");
    }
    assert!(fs.dirs().is_empty());
}
