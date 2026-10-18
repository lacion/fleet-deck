//! The staging file (design §5.2, §5.3 items 3 and 4): private modes, no
//! link followed, one link, the retained descriptor.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use fleetdeck_proto::capture::{code, text};

use super::Staging;
use crate::desktop::code as desktop_code;

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ID2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
/// The SHA-256 of `abc`.
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn failed_code(r: Result<Staging, crate::desktop::Fail>) -> (&'static str, String) {
    let e = r.unwrap_err();
    (e.code, e.message)
}

fn failed() -> (&'static str, String) {
    (code::CAPTURE_FAILED, text::CAPTURE_FAILED.to_owned())
}

#[test]
fn create_makes_private_folders_and_a_private_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let staging = Staging::create(&root, ID).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join(ID)), 0o700);
    assert_eq!(mode(&root.join(ID).join("capture.png")), 0o600);
    assert!(rustix::fs::fstatvfs(staging.dir()).is_ok());
    // A second capture uses the same `captures/`.
    assert!(Staging::create(&root, ID2).is_ok());
}

#[test]
fn a_capture_id_that_is_not_a_ulid_is_invalid_params() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    for id in [
        "",
        "../x",
        "abc",
        "01ARZ3NDEKTSV4RRFFQ69G5FAI",
        "91ARZ3NDEKTSV4RRFFQ69G5FAV",
    ] {
        let e = Staging::create(&root, id).unwrap_err();
        assert_eq!(e.code, desktop_code::INVALID_PARAMS, "{id}");
    }
    assert!(!root.exists());
}

#[test]
fn a_symlinked_captures_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let root = tmp.path().join("captures");
    symlink(&elsewhere, &root).unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID)), failed());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn captures_that_is_a_file_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    std::fs::write(&root, b"x").unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID)), failed());
}

#[test]
fn a_symlinked_id_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let root = tmp.path().join("captures");
    std::fs::create_dir(&root).unwrap();
    symlink(&elsewhere, root.join(ID)).unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID)), failed());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(
        std::fs::symlink_metadata(root.join(ID))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn an_existing_id_folder_is_refused_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    std::fs::create_dir_all(root.join(ID)).unwrap();
    std::fs::write(root.join(ID).join("keep"), b"x").unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID)), failed());
    assert!(root.join(ID).join("keep").exists());
}

#[test]
fn a_hard_link_fails_the_recheck() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let mut staging = Staging::create(&root, ID).unwrap();
    staging.file().write_all(b"abc").unwrap();
    assert!(staging.recheck(3).is_ok());
    std::fs::hard_link(root.join(ID).join("capture.png"), tmp.path().join("other")).unwrap();
    assert_eq!(staging.recheck(3).unwrap_err().code, code::CAPTURE_FAILED);
}

#[test]
fn the_recheck_compares_size_and_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let mut staging = Staging::create(&root, ID).unwrap();
    staging.file().write_all(b"abc").unwrap();
    assert!(staging.recheck(3).is_ok());
    assert!(staging.recheck(2).is_err());
    assert!(staging.recheck(4).is_err());
    let path = root.join(ID).join("capture.png");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(staging.recheck(3).is_err());
}

#[test]
fn a_replaced_file_does_not_change_the_hash_or_the_recheck() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let mut staging = Staging::create(&root, ID).unwrap();
    staging.file().write_all(b"abc").unwrap();
    let path = root.join(ID).join("capture.png");
    // Another inode takes the place of the path.
    std::fs::rename(&path, root.join(ID).join("moved")).unwrap();
    std::fs::write(&path, b"zzzzzz").unwrap();
    assert_ne!(
        std::fs::metadata(&path).unwrap().ino(),
        staging.file().metadata().unwrap().ino()
    );
    assert_eq!(staging.sha256().unwrap(), ABC);
    // The hash rewinds the descriptor: a second call gives the same value.
    assert_eq!(staging.sha256().unwrap(), ABC);
    assert!(staging.recheck(3).is_ok());
}

#[test]
fn remove_deletes_the_tree_and_a_missing_one_is_fine() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    drop(Staging::create(&root, ID).unwrap());
    Staging::remove(&root, ID).unwrap();
    assert!(!root.join(ID).exists());
    Staging::remove(&root, ID).unwrap();
    Staging::remove(&tmp.path().join("none"), ID).unwrap();
}

#[test]
fn remove_does_not_follow_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), b"x").unwrap();
    let root = tmp.path().join("captures");
    std::fs::create_dir(&root).unwrap();
    symlink(&elsewhere, root.join(ID)).unwrap();
    assert_eq!(
        Staging::remove(&root, ID).unwrap_err().code,
        code::CAPTURE_FAILED
    );
    assert!(elsewhere.join("keep").exists());
    // A link in place of `captures/` is refused too.
    let linked = tmp.path().join("linked");
    symlink(&elsewhere, &linked).unwrap();
    assert!(Staging::remove(&linked, "keep").is_err());
    assert!(elsewhere.join("keep").exists());
}

#[test]
fn remove_all_clears_captures_and_follows_no_link() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), b"x").unwrap();
    let root = tmp.path().join("captures");
    drop(Staging::create(&root, ID).unwrap());
    drop(Staging::create(&root, ID2).unwrap());
    std::fs::write(root.join("stray-file"), b"x").unwrap();
    symlink(&elsewhere, root.join("link-to-dir")).unwrap();
    symlink(elsewhere.join("keep"), root.join("link-to-file")).unwrap();
    std::fs::create_dir_all(root.join("nested").join("deep")).unwrap();
    Staging::remove_all(&root);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    assert!(elsewhere.join("keep").exists());
}

#[test]
fn remove_all_refuses_a_linked_root_and_ignores_a_missing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), b"x").unwrap();
    let linked = tmp.path().join("captures");
    symlink(&elsewhere, &linked).unwrap();
    Staging::remove_all(&linked);
    assert!(elsewhere.join("keep").exists());
    Staging::remove_all(&tmp.path().join("missing"));
}
