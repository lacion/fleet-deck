//! The trusted release prerequisite of the bootstrap (design
//! `omarchy-p8.md` 5.2 and 5.4, D24): the manifest of the installer image
//! and the installed hook files must match the running build.
//!
//! The paths are constants. There is no override (D23): a user-controlled
//! path would weaken the check.

use std::path::Path;

use serde::Deserialize;

use super::effects::{Build, Effects, RootFile, Terminal};
use super::sha256_hex;

/// The manifest of the trusted installer image.
pub const MANIFEST_PATH: &str = "/etc/fleetdeck-provision-release.json";
/// The first-login hook.
pub const HOOK_PATH: &str = "/usr/lib/fleetdeck/provision-first-login";
/// The autostart entry of the hook.
pub const DESKTOP_PATH: &str = "/etc/xdg/autostart/fleetdeck-provision.desktop";
/// The manifest is at most this large.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    pub provision_first_login_sha256: String,
    pub autostart_desktop_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub release_version: String,
    pub commit: String,
    pub package_sha256: String,
    pub binary_sha256: String,
    pub hooks: Hooks,
}

/// Checks the manifest and the hooks against the running build.
///
/// # Errors
/// - `release_untrusted`: a manifest that is a symlink, not owned by root,
///   not mode 0444, in a parent that is not root-owned or that group or
///   others can write, over 64 KiB, not valid, or not version 1; a file that
///   cannot be read; a hook that is a symlink, not root-owned, writable by
///   group or others, or whose hash differs.
/// - `build_mismatch`: the running build is not a release build, its version
///   or commit differs from the manifest, or the binary hash differs (or
///   cannot be read).
pub fn check(effects: &dyn Effects, build: &Build) -> Result<Manifest, Terminal> {
    let untrusted = Terminal::RELEASE_UNTRUSTED;
    let file = effects
        .root_file(Path::new(MANIFEST_PATH))
        .map_err(|_| untrusted)?;
    if file.symlink
        || file.uid != 0
        || file.mode & 0o7777 != 0o444
        || file.parent_uid != 0
        || file.parent_mode & 0o022 != 0
        || file.bytes.len() > MAX_MANIFEST_BYTES
    {
        return Err(untrusted);
    }
    let manifest: Manifest = serde_json::from_slice(&file.bytes).map_err(|_| untrusted)?;
    if manifest.version != 1 {
        return Err(untrusted);
    }
    if !build.release
        || manifest.release_version != build.version
        || manifest.commit != build.commit
    {
        return Err(Terminal::BUILD_MISMATCH);
    }
    let own = effects
        .own_binary_sha256()
        .map_err(|_| Terminal::BUILD_MISMATCH)?;
    if own != manifest.binary_sha256 {
        return Err(Terminal::BUILD_MISMATCH);
    }
    hook_matches(
        effects,
        HOOK_PATH,
        &manifest.hooks.provision_first_login_sha256,
    )?;
    hook_matches(
        effects,
        DESKTOP_PATH,
        &manifest.hooks.autostart_desktop_sha256,
    )?;
    Ok(manifest)
}

fn hook_matches(effects: &dyn Effects, path: &str, expected: &str) -> Result<(), Terminal> {
    let untrusted = Terminal::RELEASE_UNTRUSTED;
    let file: RootFile = effects.root_file(Path::new(path)).map_err(|_| untrusted)?;
    if file.symlink
        || file.uid != 0
        || file.mode & 0o022 != 0
        || sha256_hex(&file.bytes) != expected
    {
        return Err(untrusted);
    }
    Ok(())
}
