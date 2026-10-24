//! The `desktop-input` line of `fleetdeck doctor` (R13 P7 §8.1): `wtype`
//! found, missing (a warning) or no Omarchy desktop (no line).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fleetdeck_core::ChildEnv;

use super::{InputTool, input_tool};

fn script(dir: &Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// An environment with a `PATH` folder, with `wtype` when `wtype` is true,
/// and an Omarchy folder when `omarchy` is true.
fn env(root: &Path, wtype: bool, omarchy: bool) -> ChildEnv {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    if wtype {
        script(&bin, "wtype");
    }
    let home = root.join("omarchy");
    if omarchy {
        std::fs::create_dir_all(home.join("bin")).unwrap();
        script(&home.join("bin"), "omarchy");
    }
    let mut env = ChildEnv::empty();
    env.set("PATH", &bin).unwrap();
    env.set("OMARCHY_PATH", &home).unwrap();
    env
}

#[test]
fn doctor_finds_wtype_on_an_omarchy_machine() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(input_tool(&env(dir.path(), true, true)), InputTool::Found);
}

#[test]
fn doctor_warns_when_wtype_is_missing_on_an_omarchy_machine() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        input_tool(&env(dir.path(), false, true)),
        InputTool::Missing
    );
}

#[test]
fn doctor_prints_no_input_line_without_an_omarchy_desktop() {
    let dir = tempfile::tempdir().unwrap();
    for wtype in [true, false] {
        assert_eq!(
            input_tool(&env(dir.path(), wtype, false)),
            InputTool::NoDesktop
        );
    }
}
