//! Tests of the managed Hyprland line (R13 P4 task C, decisions P4-7 and
//! P4-13).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::symlink;
use std::path::Path;

use super::fsx::Home;
use super::hyprland::{FILE, Removal, install, line_for, present, uninstall};
use super::record::{HyprlandLine, sha256_hex};

const HYPR: &str = "/home/u/.config/fleetdeck/omarchy/hypr.lua";

fn rig(initial: Option<&str>) -> (tempfile::TempDir, Home, String) {
    let dir = tempfile::Builder::new().prefix("hypr").tempdir().unwrap();
    if let Some(text) = initial {
        std::fs::create_dir_all(dir.path().join("hypr")).unwrap();
        std::fs::write(dir.path().join(FILE), text).unwrap();
    }
    let home = Home::open(dir.path()).unwrap();
    let line = line_for(Path::new(HYPR)).unwrap();
    (dir, home, line)
}

fn text(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join(FILE)).unwrap()
}

#[test]
fn the_line_is_the_pcall_dofile_call_with_the_marker() {
    assert_eq!(
        line_for(Path::new(HYPR)).unwrap(),
        "pcall(dofile, \"/home/u/.config/fleetdeck/omarchy/hypr.lua\") -- fleetdeck: managed line"
    );
}

#[test]
fn the_path_is_escaped_as_a_lua_string_and_unsafe_paths_give_none() {
    assert_eq!(
        line_for(Path::new("/a\\b\"c/hypr.lua")).unwrap(),
        "pcall(dofile, \"/a\\\\b\\\"c/hypr.lua\") -- fleetdeck: managed line"
    );
    assert!(line_for(Path::new("/a\nb/hypr.lua")).is_none());
    assert!(line_for(Path::new("/a\tb/hypr.lua")).is_none());
}

#[test]
fn the_line_is_appended_once_and_a_second_install_adds_nothing() {
    let (dir, home, line) = rig(Some("o.bind(\"x\")\n"));
    let first = install(&home, &line, &HyprlandLine::default());
    assert!(first.record.added && !first.print_line && !first.record.newline_added);
    assert_eq!(text(&dir), format!("o.bind(\"x\")\n{line}\n"));
    assert_eq!(
        first.record.before_sha256.as_deref(),
        Some(sha256_hex(b"o.bind(\"x\")\n").as_str())
    );
    assert_eq!(
        first.record.after_sha256.as_deref(),
        Some(sha256_hex(text(&dir).as_bytes()).as_str())
    );
    let before = text(&dir);
    let second = install(&home, &line, &first.record);
    assert_eq!(text(&dir), before);
    // The rerun keeps the old object unchanged (P4-13).
    assert_eq!(second.record, first.record);
    assert!(present(&home, &line));
}

#[test]
fn a_line_that_was_there_without_a_record_gets_both_hashes_of_the_file() {
    let (dir, home, line) = rig(None);
    std::fs::create_dir_all(dir.path().join("hypr")).unwrap();
    std::fs::write(dir.path().join(FILE), format!("{line}\n")).unwrap();
    let done = install(&home, &line, &HyprlandLine::default());
    let hash = sha256_hex(format!("{line}\n").as_bytes());
    assert!(done.record.added);
    assert_eq!(done.record.before_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(done.record.after_sha256.as_deref(), Some(hash.as_str()));
    assert!(!done.record.newline_added);
    assert_eq!(text(&dir), format!("{line}\n"));
}

#[test]
fn a_file_without_a_final_newline_gets_one_and_uninstall_takes_it_back() {
    let (dir, home, line) = rig(Some("last line"));
    let done = install(&home, &line, &HyprlandLine::default());
    assert!(done.record.newline_added);
    assert_eq!(text(&dir), format!("last line\n{line}\n"));
    assert_eq!(uninstall(&home, &line, &done.record), Removal::Removed);
    assert_eq!(text(&dir), "last line");
    assert_eq!(
        sha256_hex(text(&dir).as_bytes()),
        done.record.before_sha256.clone().unwrap()
    );
}

#[test]
fn uninstall_removes_exactly_the_line_and_restores_the_file() {
    let (dir, home, line) = rig(Some("a\nb\n"));
    let done = install(&home, &line, &HyprlandLine::default());
    assert_eq!(uninstall(&home, &line, &done.record), Removal::Removed);
    assert_eq!(text(&dir), "a\nb\n");
    assert!(!present(&home, &line));
    // A second uninstall finds nothing.
    assert_eq!(uninstall(&home, &line, &done.record), Removal::Nothing);
}

#[test]
fn uninstall_after_a_user_edit_keeps_the_edit_and_removes_only_the_line() {
    let (dir, home, line) = rig(Some("a\n"));
    let done = install(&home, &line, &HyprlandLine::default());
    let edited = text(&dir).replace("a\n", "a\nmine = 1\n") + "tail = 2\n";
    std::fs::write(dir.path().join(FILE), &edited).unwrap();
    assert_eq!(uninstall(&home, &line, &done.record), Removal::KeptOthers);
    assert_eq!(text(&dir), "a\nmine = 1\ntail = 2\n");
}

#[test]
fn a_missing_hyprland_file_is_not_created_and_the_line_is_printed() {
    let (dir, home, line) = rig(None);
    let done = install(&home, &line, &HyprlandLine::default());
    assert!(!done.record.added && done.print_line);
    assert!(!dir.path().join("hypr").exists());
}

#[test]
fn a_symlinked_hyprland_file_is_not_touched_and_the_line_is_printed() {
    let (dir, home, line) = rig(None);
    std::fs::create_dir_all(dir.path().join("hypr")).unwrap();
    let real = dir.path().join("real.lua");
    std::fs::write(&real, "keep\n").unwrap();
    symlink(&real, dir.path().join(FILE)).unwrap();
    let done = install(&home, &line, &HyprlandLine::default());
    assert!(!done.record.added && done.print_line);
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "keep\n");
    assert!(
        std::fs::symlink_metadata(dir.path().join(FILE))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!present(&home, &line));
}

#[test]
fn a_symlinked_hypr_directory_is_not_followed() {
    let (dir, home, line) = rig(None);
    let real = dir.path().join("dotfiles");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("hyprland.lua"), "keep\n").unwrap();
    symlink(&real, dir.path().join("hypr")).unwrap();
    let done = install(&home, &line, &HyprlandLine::default());
    assert!(!done.record.added && done.print_line);
    assert_eq!(
        std::fs::read_to_string(real.join("hyprland.lua")).unwrap(),
        "keep\n"
    );
}

#[test]
fn the_file_mode_is_kept() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, home, line) = rig(Some("a\n"));
    std::fs::set_permissions(
        dir.path().join(FILE),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    install(&home, &line, &HyprlandLine::default());
    let mode = std::fs::metadata(dir.path().join(FILE))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640);
}
