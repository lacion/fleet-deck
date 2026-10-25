//! Tests of the permission check of the install plan (PRE-PR round 1): a
//! file that a group or other user can write, or a directory of its path
//! from the base folder down, is not replaceable.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt as _;

use fleetdeck_proto::omarchy_repair::{PlanBlock, PlanClassification};

use super::*;

const MANIFEST: &str = "omarchy/plugins/fleetdeck.fleet/manifest.json";

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("planr1").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    home.write(Path::new(MANIFEST), b"theirs\n", 0o644).unwrap();
    (dir, home)
}

fn chmod(dir: &tempfile::TempDir, rel: &str, bits: u32) {
    std::fs::set_permissions(dir.path().join(rel), std::fs::Permissions::from_mode(bits)).unwrap();
}

fn the_file(home: &Home) -> PlanFile {
    let out = facts(home, &Record::new()).unwrap();
    assert_eq!(out.files.len(), 1);
    out.files.into_iter().next().unwrap()
}

#[test]
fn a_file_that_only_you_can_write_is_replaceable() {
    let (_dir, home) = home();
    let f = the_file(&home);
    assert_eq!(f.classification, PlanClassification::UnrecordedConflict);
    assert!(f.replaceable);
    assert_eq!(f.blocked, None);
}

#[test]
fn a_target_with_a_group_or_other_write_bit_is_blocked_with_its_facts() {
    for bits in [0o664, 0o646, 0o666] {
        let (dir, home) = home();
        chmod(&dir, MANIFEST, bits);
        let f = the_file(&home);
        assert!(!f.replaceable, "{bits:o}");
        assert_eq!(f.blocked, Some(PlanBlock::UnsafePath), "{bits:o}");
        // The facts of the file are still there.
        assert_eq!(f.mode, Some(bits));
        assert!(f.sha256.is_some() && f.ino.is_some());
    }
}

#[test]
fn a_directory_with_a_group_or_other_write_bit_blocks_the_file() {
    for dir_rel in [
        "omarchy",
        "omarchy/plugins",
        "omarchy/plugins/fleetdeck.fleet",
    ] {
        let (dir, home) = home();
        chmod(&dir, dir_rel, 0o775);
        let f = the_file(&home);
        assert!(!f.replaceable, "{dir_rel}");
        assert_eq!(f.blocked, Some(PlanBlock::UnsafePath), "{dir_rel}");
        chmod(&dir, dir_rel, 0o757);
        assert!(!the_file(&home).replaceable, "{dir_rel}");
    }
}

#[test]
fn the_config_home_itself_is_not_checked() {
    let (dir, home) = home();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(the_file(&home).replaceable);
}
