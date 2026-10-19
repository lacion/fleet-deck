//! The `desktop-click` line of `fleetdeck doctor` (R13 P7b §15): `wlrctl`
//! found (an `ok` line that says the compatibility is not verified), missing
//! (a warning) or no Omarchy desktop (no line). The doctor never runs
//! `wlrctl`: the test `wlrctl` would leave a mark if it ran.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fleetdeck_core::ChildEnv;

use super::{InputTool, click_line, click_tool};

fn script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An environment with a `PATH` folder, with `wlrctl` when `wlrctl` is true
/// (it writes `ran` next to the folder when it runs), and an Omarchy folder
/// when `omarchy` is true.
fn env(root: &Path, wlrctl: bool, omarchy: bool) -> ChildEnv {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    if wlrctl {
        script(&bin, "wlrctl", &format!(": > {}/ran", root.display()));
    }
    let home = root.join("omarchy");
    if omarchy {
        std::fs::create_dir_all(home.join("bin")).unwrap();
        script(&home.join("bin"), "omarchy", "exit 0");
    }
    let mut env = ChildEnv::empty();
    env.set("PATH", &bin).unwrap();
    env.set("OMARCHY_PATH", &home).unwrap();
    env
}

#[test]
fn doctor_finds_wlrctl_and_says_that_compatibility_is_not_verified() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path(), true, true);
    assert_eq!(click_tool(&env), InputTool::Found);
    assert_eq!(
        click_line(&env),
        Some(Ok(
            "wlrctl found; compatibility is not verified (live smoke pending)"
        ))
    );
    // Finding the tool never runs it.
    assert!(!dir.path().join("ran").exists());
}

#[test]
fn doctor_warns_when_wlrctl_is_missing_on_an_omarchy_machine() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path(), false, true);
    assert_eq!(click_tool(&env), InputTool::Missing);
    assert_eq!(
        click_line(&env),
        Some(Err(
            "wlrctl not found: agent pointer clicks fail until it is installed"
        ))
    );
}

#[test]
fn doctor_prints_no_click_line_without_an_omarchy_desktop() {
    let dir = tempfile::tempdir().unwrap();
    for wlrctl in [true, false] {
        let env = env(dir.path(), wlrctl, false);
        assert_eq!(click_tool(&env), InputTool::NoDesktop);
        assert_eq!(click_line(&env), None);
    }
    assert!(!dir.path().join("ran").exists());
}
