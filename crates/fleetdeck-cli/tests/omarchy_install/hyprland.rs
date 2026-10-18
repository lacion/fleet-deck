//! The managed Hyprland line of the installer (P4-7, P4-13).

use std::os::unix::fs::symlink;

use serde_json::Value;

use crate::rig::{Rig, err, managed_line, out};

#[test]
fn omarchy_hyprland_line_is_appended_once() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    assert_eq!(rig.install().status.code(), Some(0));
    let line = managed_line(&rig);
    assert_eq!(rig.hypr_text(), format!("-- user\n{line}\n"));
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.hypr_text(), format!("-- user\n{line}\n"));
    let record = rig.record();
    assert_eq!(record["hyprland_line"]["added"], true);
    assert_eq!(
        record["hyprland_line"]["before_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_ne!(
        record["hyprland_line"]["before_sha256"],
        record["hyprland_line"]["after_sha256"]
    );
}

#[test]
fn omarchy_hyprland_file_without_a_final_newline() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user");
    assert_eq!(rig.install().status.code(), Some(0));
    let line = managed_line(&rig);
    assert_eq!(rig.hypr_text(), format!("-- user\n{line}\n"));
    assert_eq!(rig.record()["hyprland_line"]["newline_added"], true);
    assert_eq!(rig.run(&["omarchy", "uninstall"]).status.code(), Some(0));
    assert_eq!(rig.hypr_text(), "-- user");
}

#[test]
fn omarchy_hyprland_symlinked_file_is_not_touched_and_the_line_is_printed() {
    let mut rig = Rig::new();
    rig.station(false);
    let real = rig.root().join("dotfiles-hyprland.lua");
    std::fs::write(&real, "-- theirs\n").unwrap();
    std::fs::create_dir_all(rig.cfg().join("hypr")).unwrap();
    symlink(&real, rig.cfg().join("hypr/hyprland.lua")).unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "-- theirs\n");
    assert!(out(&o).contains(&managed_line(&rig)), "{}", out(&o));
    assert_eq!(rig.record()["hyprland_line"]["added"], false);
    let status = rig.run(&["omarchy", "status"]);
    assert_eq!(status.status.code(), Some(0), "{}", out(&status));
    assert!(
        out(&status).contains("hyprland: line missing"),
        "{}",
        out(&status)
    );
}

#[test]
fn omarchy_hyprland_missing_file_is_not_created() {
    let mut rig = Rig::new();
    rig.station(false);
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!rig.cfg().join("hypr").exists());
    assert!(out(&o).contains(&managed_line(&rig)), "{}", out(&o));
    assert_eq!(rig.record()["hyprland_line"]["added"], false);
}

#[test]
fn omarchy_hyprland_uninstall_removes_exactly_the_line() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("a = 1\nb = 2\n");
    assert_eq!(rig.install().status.code(), Some(0));
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(rig.hypr_text(), "a = 1\nb = 2\n");
}

#[test]
fn omarchy_hyprland_uninstall_after_a_user_edit_keeps_the_edit() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("a = 1\n");
    assert_eq!(rig.install().status.code(), Some(0));
    let line = managed_line(&rig);
    std::fs::write(
        rig.cfg().join("hypr/hyprland.lua"),
        format!("a = 1\n{line}\nmine = 3\n"),
    )
    .unwrap();
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(rig.hypr_text(), "a = 1\nmine = 3\n");
    assert!(
        out(&o).contains(&format!(
            "kept your other changes: {}",
            rig.cfg().join("hypr/hyprland.lua").display()
        )),
        "{}",
        out(&o)
    );
}

#[test]
fn omarchy_hyprland_status_reports_a_missing_line_without_a_failing_exit() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("a = 1\n");
    assert_eq!(rig.install().status.code(), Some(0));
    let ok = rig.run(&["omarchy", "status"]);
    assert!(out(&ok).contains("hyprland: line ok"), "{}", out(&ok));
    rig.hypr("a = 1\n");
    let o = rig.run(&["omarchy", "status"]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert!(out(&o).contains("hyprland: line missing"), "{}", out(&o));
    let json = rig.run(&["omarchy", "status", "--json"]);
    let v: Value = serde_json::from_str(out(&json).trim()).unwrap();
    assert_eq!(v["hyprland_line"], "missing");
}
