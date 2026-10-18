//! The staging file (design §5.2, §5.3 items 3 and 4): private modes, no
//! link followed, one link, the retained descriptor.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use fleetdeck_proto::capture::{code, text};

use super::Staging;
use crate::artifacts::capture_import::SyncFs;
use crate::artifacts::capture_import::tests::Recorder;
use crate::desktop::code as desktop_code;

const FILE: &str = "capture.png";

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
    let staging = Staging::create(&root, ID, FILE).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join(ID)), 0o700);
    assert_eq!(mode(&root.join(ID).join("capture.png")), 0o600);
    assert!(rustix::fs::fstatvfs(staging.dir()).is_ok());
    // A second capture uses the same `captures/`.
    assert!(Staging::create(&root, ID2, FILE).is_ok());
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
        let e = Staging::create(&root, id, FILE).unwrap_err();
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
    assert_eq!(failed_code(Staging::create(&root, ID, FILE)), failed());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn captures_that_is_a_file_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    std::fs::write(&root, b"x").unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID, FILE)), failed());
}

#[test]
fn a_symlinked_id_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let root = tmp.path().join("captures");
    std::fs::create_dir(&root).unwrap();
    symlink(&elsewhere, root.join(ID)).unwrap();
    assert_eq!(failed_code(Staging::create(&root, ID, FILE)), failed());
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
    assert_eq!(failed_code(Staging::create(&root, ID, FILE)), failed());
    assert!(root.join(ID).join("keep").exists());
}

#[test]
fn a_hard_link_fails_the_recheck() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let mut staging = Staging::create(&root, ID, FILE).unwrap();
    staging.file().write_all(b"abc").unwrap();
    assert!(staging.recheck(3).is_ok());
    std::fs::hard_link(root.join(ID).join("capture.png"), tmp.path().join("other")).unwrap();
    assert_eq!(staging.recheck(3).unwrap_err().code, code::CAPTURE_FAILED);
}

#[test]
fn the_recheck_compares_size_and_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let mut staging = Staging::create(&root, ID, FILE).unwrap();
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
    let mut staging = Staging::create(&root, ID, FILE).unwrap();
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
    drop(Staging::create(&root, ID, FILE).unwrap());
    Staging::remove(&root, ID, &SyncFs).unwrap();
    assert!(!root.join(ID).exists());
    Staging::remove(&root, ID, &SyncFs).unwrap();
    Staging::remove(&tmp.path().join("none"), ID, &SyncFs).unwrap();
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
        Staging::remove(&root, ID, &SyncFs).unwrap_err().code,
        code::CAPTURE_FAILED
    );
    assert!(elsewhere.join("keep").exists());
    // A link in place of `captures/` is refused too.
    let linked = tmp.path().join("linked");
    symlink(&elsewhere, &linked).unwrap();
    assert!(Staging::remove(&linked, "keep", &SyncFs).is_err());
    assert!(elsewhere.join("keep").exists());
}

#[test]
fn an_mp4_staging_file_has_its_own_name_and_a_bad_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    let staging = Staging::create(&root, ID, "capture.mp4").unwrap();
    let path = root.join(ID).join("capture.mp4");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!root.join(ID).join("capture.png").exists());
    drop(staging);
    for bad in ["", ".", "..", "a/b", "../x"] {
        let e = Staging::create(&root, ID2, bad).unwrap_err();
        assert_eq!((e.code, e.message), failed(), "{bad:?}");
    }
    assert!(!root.join(ID2).exists());
}

#[test]
fn remove_syncs_the_emptied_folder_and_then_the_captures_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    drop(Staging::create(&root, ID, FILE).unwrap());
    let rec = Recorder::default();
    Staging::remove(&root, ID, &rec).unwrap();
    assert_eq!(
        rec.count(),
        2,
        "the emptied folder, then the captures folder"
    );
    assert!(!root.join(ID).exists());
}

#[test]
fn a_failed_sync_is_a_failed_removal_and_a_later_call_finishes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("captures");
    for at in 0..2 {
        drop(Staging::create(&root, ID, FILE).unwrap());
        let rec = Recorder::failing_at(at);
        let e = Staging::remove(&root, ID, &rec).unwrap_err();
        assert_eq!((e.code, e.message), failed(), "sync {at}");
        Staging::remove(&root, ID, &SyncFs).unwrap();
        assert!(!root.join(ID).exists());
    }
}
