//! The program copy (design service.md §4).

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::service::program;

struct Dirs {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    exe: PathBuf,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("station/bin");
    let exe = tmp.path().join("build/fleetdeck");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"build-one").unwrap();
    Dirs {
        root,
        exe,
        _tmp: tmp,
    }
}

fn target(root: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_link(root.join(name)).ok()
}

fn releases(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("releases"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn the_first_install_copies_and_points_current() {
    let d = dirs();
    let link = program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    assert_eq!(link, d.root.join("current"));
    let copy = target(&d.root, "current").unwrap();
    assert_eq!(copy, d.root.join("releases/1.2.3-svc-01aaa/fleetdeck"));
    assert_eq!(std::fs::read(&copy).unwrap(), b"build-one");
    assert_eq!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(target(&d.root, "previous"), None);
}

#[test]
fn the_same_binary_again_makes_no_copy() {
    let d = dirs();
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    assert!(!program::needs_copy(&d.root, &d.exe).unwrap());
    program::ensure(&d.root, &d.exe, "1.2.3", "01bbb").unwrap();
    assert_eq!(releases(&d.root), ["1.2.3-svc-01aaa"]);
    assert_eq!(
        target(&d.root, "current").unwrap(),
        d.root.join("releases/1.2.3-svc-01aaa/fleetdeck")
    );
}

#[test]
fn a_new_binary_makes_a_new_release_and_moves_previous() {
    let d = dirs();
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    std::fs::write(&d.exe, b"build-two").unwrap();
    assert!(program::needs_copy(&d.root, &d.exe).unwrap());
    program::ensure(&d.root, &d.exe, "1.2.3", "01bbb").unwrap();
    assert_eq!(
        target(&d.root, "current").unwrap(),
        d.root.join("releases/1.2.3-svc-01bbb/fleetdeck")
    );
    assert_eq!(
        target(&d.root, "previous").unwrap(),
        d.root.join("releases/1.2.3-svc-01aaa/fleetdeck")
    );
    assert_eq!(releases(&d.root), ["1.2.3-svc-01aaa", "1.2.3-svc-01bbb"]);
}

#[test]
fn a_broken_current_is_replaced() {
    let d = dirs();
    std::fs::create_dir_all(&d.root).unwrap();
    symlink(
        d.root.join("releases/gone/fleetdeck"),
        d.root.join("current"),
    )
    .unwrap();
    assert!(program::needs_copy(&d.root, &d.exe).unwrap());
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    let copy = target(&d.root, "current").unwrap();
    assert_eq!(std::fs::read(copy).unwrap(), b"build-one");
}

#[test]
fn old_svc_releases_are_removed_and_upd_releases_stay() {
    let d = dirs();
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    for (n, id) in [("two", "01bbb"), ("three", "01ccc")] {
        std::fs::write(&d.exe, format!("build-{n}")).unwrap();
        program::ensure(&d.root, &d.exe, "1.2.3", id).unwrap();
    }
    // The upgrade flow made a release that no link names.
    let upd = d.root.join("releases/1.2.3-upd-01zzz");
    std::fs::create_dir_all(&upd).unwrap();
    std::fs::write(upd.join("fleetdeck"), b"x").unwrap();
    std::fs::write(&d.exe, b"build-four").unwrap();
    program::ensure(&d.root, &d.exe, "1.2.3", "01ddd").unwrap();
    // current is 01ddd, previous is 01ccc; 01aaa and 01bbb are gone.
    assert_eq!(
        releases(&d.root),
        ["1.2.3-svc-01ccc", "1.2.3-svc-01ddd", "1.2.3-upd-01zzz"]
    );
}

#[test]
fn copy_state_is_same_newer_older_or_missing() {
    use crate::service::program::CopyState;
    let d = dirs();
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Missing
    );
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    let copy = target(&d.root, "current").unwrap();
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Same
    );
    // The bytes differ: the time decides. Equal times count as older.
    std::fs::write(&d.exe, b"build-two").unwrap();
    super::set_mtime(&copy, 1000);
    super::set_mtime(&d.exe, 2000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Newer
    );
    super::set_mtime(&d.exe, 1000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Older
    );
    super::set_mtime(&d.exe, 500);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Older
    );
    // A broken link is a missing copy.
    std::fs::remove_file(&copy).unwrap();
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Missing
    );
}

#[test]
fn the_dry_run_line_names_the_copy_only_when_one_would_be_made() {
    let d = dirs();
    let line = program::dry_line(&d.root, &d.exe, "1.2.3", "<ulid>")
        .unwrap()
        .unwrap();
    assert_eq!(
        line,
        format!(
            "$ (copy {} to {} and point {} at it)",
            d.exe.display(),
            d.root.join("releases/1.2.3-svc-<ulid>").display(),
            d.root.join("current").display()
        )
    );
    // Nothing was changed.
    assert!(!d.root.exists());
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    assert_eq!(
        program::dry_line(&d.root, &d.exe, "1.2.3", "<ulid>").unwrap(),
        None
    );
}

/// A fake program that answers `build-info` with the commit time `secs`
/// (`None`: it prints nothing, like an older fleetdeck). `note` makes the
/// bytes differ.
fn fake_build(path: &Path, secs: Option<i64>, note: &str) {
    let version = fleetdeck_core::build::version();
    let body = match secs {
        Some(secs) => format!(
            "printf '%s\\n' '{{\"version\":\"{version}\",\"commit\":\"c{secs}\",\"commit_time\":{secs}}}'"
        ),
        None => "exit 0".to_owned(),
    };
    std::fs::write(path, format!("#!/bin/sh\n# {note}\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_newer_commit_time_wins_even_when_its_file_time_is_older() {
    use crate::service::program::CopyState;
    let d = dirs();
    fake_build(&d.exe, Some(1000), "one");
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    let copy = target(&d.root, "current").unwrap();
    // This program is built from a later commit but its file is older.
    fake_build(&d.exe, Some(2000), "two");
    super::set_mtime(&copy, 2000);
    super::set_mtime(&d.exe, 1000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Newer
    );
    // This program is built from an earlier commit but its file is newer.
    fake_build(&d.exe, Some(500), "three");
    super::set_mtime(&copy, 1000);
    super::set_mtime(&d.exe, 2000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Older
    );
}

#[test]
fn the_same_commit_time_or_a_silent_program_falls_back_to_file_times() {
    use crate::service::program::CopyState;
    let d = dirs();
    fake_build(&d.exe, Some(1000), "one");
    program::ensure(&d.root, &d.exe, "1.2.3", "01aaa").unwrap();
    let copy = target(&d.root, "current").unwrap();
    // The same commit time (local edits): the file time decides.
    fake_build(&d.exe, Some(1000), "local edit");
    super::set_mtime(&copy, 1000);
    super::set_mtime(&d.exe, 2000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Newer
    );
    super::set_mtime(&d.exe, 500);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Older
    );
    // A copy that prints nothing: the file time decides, whatever the commit
    // time of this program.
    fake_build(&copy, None, "old");
    fake_build(&d.exe, Some(500), "new");
    super::set_mtime(&copy, 1000);
    super::set_mtime(&d.exe, 2000);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Newer
    );
    super::set_mtime(&d.exe, 500);
    assert_eq!(
        program::copy_state(&d.root, &d.exe).unwrap(),
        CopyState::Older
    );
}
