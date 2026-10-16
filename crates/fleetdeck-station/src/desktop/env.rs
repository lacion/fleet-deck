//! The environment of desktop children (R13 §6.1): the station's own
//! environment, plus the three Hyprland variables and, on Omarchy, a `PATH`
//! that starts with `$OMARCHY_PATH/bin`. Only desktop children get it;
//! provider sessions never do.

use std::ffi::OsString;

use fleetdeck_core::ChildEnv;

use super::detect::Found;
use super::{Fail, code};

fn failed(e: impl std::fmt::Display) -> Fail {
    Fail::new(
        code::FAILED,
        format!("the desktop environment is invalid: {e}"),
    )
}

/// The environment of a desktop child of `found`, built on `base`.
///
/// # Errors
/// `failed` when a value cannot be an environment value.
pub fn desktop_env(base: &ChildEnv, found: &Found) -> Result<ChildEnv, Fail> {
    let mut env = base.clone();
    env.set("XDG_RUNTIME_DIR", &found.runtime_dir)
        .map_err(failed)?;
    env.set("WAYLAND_DISPLAY", &found.wayland).map_err(failed)?;
    env.set("HYPRLAND_INSTANCE_SIGNATURE", &found.signature)
        .map_err(failed)?;
    if let Some(root) = &found.omarchy {
        let mut path = OsString::from(root.join("bin"));
        if let Some(old) = base.get("PATH") {
            path.push(":");
            path.push(old);
        }
        env.set("PATH", path).map_err(failed)?;
    }
    Ok(env)
}

/// The environment for the detection itself: the base and the runtime dir.
///
/// # Errors
/// `failed` when the directory cannot be an environment value.
pub fn probe_env(base: &ChildEnv, runtime_dir: &std::path::Path) -> Result<ChildEnv, Fail> {
    let mut env = base.clone();
    env.set("XDG_RUNTIME_DIR", runtime_dir).map_err(failed)?;
    Ok(env)
}
