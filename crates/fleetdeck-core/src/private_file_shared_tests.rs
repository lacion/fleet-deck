//! Tests of `write_new_shared_parent` (P8 T3b): the same rules as
//! `write_new_private`, except that the parent may be 0755.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use super::*;

const PREFIX: &str = ".station-toml.";

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A directory with `mode`; it is the root of the ancestor walk, so the
/// walk checks the parent only.
fn root_with_mode(mode: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    chmod(dir.path(), mode);
    dir
}

fn leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(PREFIX))
        .collect()
}

#[test]
fn a_parent_with_mode_0755_or_0700_gets_a_0600_file() {
    for mode in [0o755, 0o700] {
        let root = root_with_mode(mode);
        let path = root.path().join("station.toml");
        write_new_shared_below(root.path(), &path, b"name = \"a\"\n", PREFIX).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"name = \"a\"\n");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(leftovers(root.path()).is_empty());
    }
}

#[test]
fn a_parent_that_group_or_others_can_write_is_unsafe() {
    for mode in [0o775, 0o757, 0o777] {
        let root = root_with_mode(mode);
        let path = root.path().join("station.toml");
        let r = write_new_shared_below(root.path(), &path, b"data", PREFIX);
        assert!(
            matches!(r, Err(PrivateWriteError::Unsafe)),
            "{mode:o}: {r:?}"
        );
        assert!(!path.exists());
    }
}

#[test]
fn an_existing_file_and_a_dangling_symlink_are_not_replaced() {
    let root = root_with_mode(0o755);
    let path = root.path().join("station.toml");
    fs::write(&path, b"first").unwrap();
    let r = write_new_shared_below(root.path(), &path, b"second", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Exists)), "{r:?}");
    assert_eq!(fs::read(&path).unwrap(), b"first");
    let link = root.path().join("link.toml");
    symlink(root.path().join("nowhere"), &link).unwrap();
    let r = write_new_shared_below(root.path(), &link, b"data", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Exists)), "{r:?}");
    assert!(!root.path().join("nowhere").exists());
    assert!(leftovers(root.path()).is_empty());
}

#[test]
fn a_symlinked_parent_is_unsafe() {
    let root = root_with_mode(0o755);
    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    chmod(&real, 0o755);
    let link = root.path().join("link");
    symlink(&real, &link).unwrap();
    let r = write_new_shared_below(root.path(), &link.join("station.toml"), b"data", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    assert!(!real.join("station.toml").exists());
}

#[test]
fn an_ancestor_that_group_can_write_is_unsafe() {
    let root = root_with_mode(0o700);
    let middle = root.path().join("middle");
    let inner = middle.join("inner");
    fs::create_dir_all(&inner).unwrap();
    chmod(&inner, 0o755);
    chmod(&middle, 0o770);
    let path = inner.join("station.toml");
    let r = write_new_shared_below(root.path(), &path, b"data", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    chmod(&middle, 0o755);
    write_new_shared_below(root.path(), &path, b"data", PREFIX).unwrap();
}

#[test]
fn the_public_writer_refuses_a_path_below_tmp() {
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    chmod(dir.path(), 0o755);
    let path = dir.path().join("station.toml");
    let r = write_new_shared_parent(&path, b"data", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    assert!(!path.exists());
}

#[test]
fn the_private_writer_still_refuses_a_0755_parent() {
    let root = root_with_mode(0o755);
    let path = root.path().join("out.toml");
    let r = write_new_below(root.path(), &path, b"data", PREFIX);
    assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
}
