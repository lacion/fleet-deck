//! `fleetdeck doctor` and desktop input (R13 P7 §8.1): the `desktop-input`
//! line is `ok` when `wtype` is on the `PATH` of an Omarchy machine, a `warn`
//! (never a `fail`, the exit code stays 0) when it is not, and absent without
//! an Omarchy desktop.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fleetdeck_testkit::TempNode;

fn git_dir() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))
        .find(|dir| dir.join("git").is_file())
        .expect("git is on PATH")
}

fn script(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs `doctor` with a fake `claude` and `codex`, `wtype` when `wtype` is
/// true, and an Omarchy folder when `omarchy` is true.
fn doctor(node: &TempNode, wtype: bool, omarchy: bool) -> (Output, String) {
    let bin = node.root().join("bin");
    script(&bin, "claude", "echo '2.1.289 (fake)'");
    script(&bin, "codex", "echo 'codex-cli 0.160.0'");
    if wtype {
        script(&bin, "wtype", "exit 0");
    }
    let home = node.root().join("omarchy");
    if omarchy {
        script(&home.join("bin"), "omarchy", "exit 0");
    }
    let path = std::env::join_paths([bin, git_dir()]).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", node.home())
        .env("OMARCHY_PATH", &home)
        .env("RUST_LOG", "warn")
        .arg("--data-dir")
        .arg(node.data_dir())
        .arg("doctor")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    (out, text)
}

#[test]
fn doctor_reports_wtype_found_on_an_omarchy_machine() {
    let node = TempNode::new("doctor-input-found").unwrap();
    let (out, text) = doctor(&node, true, true);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("ok   desktop-input: wtype found"), "{text}");
}

#[test]
fn doctor_warns_but_does_not_fail_when_wtype_is_missing() {
    let node = TempNode::new("doctor-input-missing").unwrap();
    let (out, text) = doctor(&node, false, true);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("warn desktop-input: wtype not found"),
        "{text}"
    );
    assert!(!text.contains("fail desktop-input"), "{text}");
}

#[test]
fn doctor_has_no_input_line_without_an_omarchy_desktop() {
    let node = TempNode::new("doctor-input-none").unwrap();
    let (out, text) = doctor(&node, false, false);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(!text.contains("desktop-input"), "{text}");
}
