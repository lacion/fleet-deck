//! Tests of the service inspection of the bootstrap (P8 T3b, D13). The
//! machine is the recording rig of the service tests: no program runs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::service::fake::ok;
use crate::service::program;
use crate::service::tests::Rig;

/// A station service as `fleetdeck service install station` leaves it: the
/// unit with our data directory and the program link at the running binary.
fn installed(rig: &Rig) {
    rig.put_installed(Kind::Station);
    let root = Kind::Station.root(&rig.ctx.dirs);
    program::ensure(&root, &rig.ctx.exe, "1.2.3", "01aaa").unwrap();
}

fn active(rig: &Rig, yes: bool) {
    let answer = if yes {
        ok("active\n")
    } else {
        crate::service::fake::fail(3, "")
    };
    rig.fake
        .always("systemctl", &["--user", "is-active"], answer);
}

fn inspected(rig: &Rig) -> ServiceInspect {
    inspect(&rig.ctx, &rig.fake)
}

#[test]
fn no_unit_is_absent() {
    let rig = Rig::systemd();
    assert_eq!(inspected(&rig), ServiceInspect::Absent);
    // A deck service alone is not a station service.
    rig.put_installed(Kind::Deck);
    assert_eq!(inspected(&rig), ServiceInspect::Absent);
}

#[test]
fn a_matching_unit_reports_whether_it_is_active() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, false);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: false });
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
}

#[test]
fn a_both_service_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    rig.put_installed(Kind::Both);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // Also without a station unit.
    let rig = Rig::systemd();
    rig.put_installed(Kind::Both);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn another_data_directory_or_no_marker_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    rig.put_file(Kind::Station, Some("/somewhere/else"));
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    rig.put_file(Kind::Station, None);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn a_missing_program_link_is_a_conflict() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Station);
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
    // A link whose target is gone.
    let root = Kind::Station.root(&rig.ctx.dirs);
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink("releases/none/fleetdeck", root.join("current")).unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn another_program_than_the_running_binary_is_a_conflict() {
    let rig = Rig::systemd();
    installed(&rig);
    active(&rig, true);
    assert_eq!(inspected(&rig), ServiceInspect::Matching { active: true });
    std::fs::write(&rig.ctx.exe, b"another build").unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn an_unreadable_unit_is_a_conflict() {
    let rig = Rig::systemd();
    // A directory where the unit file should be: reading it fails.
    std::fs::create_dir_all(rig.file(Kind::Station)).unwrap();
    assert_eq!(inspected(&rig), ServiceInspect::Conflict);
}

#[test]
fn sha256_file_is_the_lowercase_digest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        sha256_file(&path).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // More than one 64 KiB read.
    std::fs::write(&path, vec![b'a'; 200_000]).unwrap();
    assert_eq!(
        sha256_file(&path).unwrap(),
        crate::provision::sha256_hex(&vec![b'a'; 200_000])
    );
    assert!(sha256_file(&dir.path().join("missing")).is_err());
}
