//! Desktop detection (R13 §6.1), at every call: Hyprland gets a new instance
//! signature when it restarts.
//!
//! 1. Hyprland: a directory `$XDG_RUNTIME_DIR/hypr/<signature>/` whose
//!    `.socket.sock` accepts a connect. With more than one live instance the
//!    newest directory wins and the others are logged at debug level.
//! 2. Wayland: the `wl_socket` of that instance in `hyprctl instances -j`.
//! 3. Omarchy: `$OMARCHY_PATH` (else `/usr/share/omarchy`) with an
//!    executable `bin/omarchy`; the version is the `version` file.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::desktop::DesktopKind;
use tokio::net::UnixStream;

use super::env::probe_env;
use super::run::{CALL_TIMEOUT, Runner};

const CONNECT_LIMIT: Duration = Duration::from_secs(1);
const DEFAULT_OMARCHY: &str = "/usr/share/omarchy";

/// The desktop that a call found.
#[derive(Debug, Clone)]
pub struct Found {
    pub kind: DesktopKind,
    pub signature: String,
    pub runtime_dir: PathBuf,
    /// The Wayland socket name of the instance.
    pub wayland: String,
    /// The Omarchy folder, when Omarchy is installed.
    pub omarchy: Option<PathBuf>,
    pub omarchy_version: Option<String>,
}

/// The live Hyprland instances of `runtime_dir`, newest first.
async fn live_instances(runtime_dir: &Path) -> Vec<String> {
    let Ok(mut dir) = tokio::fs::read_dir(runtime_dir.join("hypr")).await else {
        return Vec::new();
    };
    let mut live: Vec<(SystemTime, String)> = Vec::new();
    while let Ok(Some(entry)) = dir.next_entry().await {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(meta) = tokio::fs::metadata(entry.path()).await else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let socket = entry.path().join(".socket.sock");
        let connected = tokio::time::timeout(CONNECT_LIMIT, UnixStream::connect(&socket)).await;
        if !matches!(connected, Ok(Ok(_))) {
            continue;
        }
        live.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), name));
    }
    // The newest first; the name breaks a tie so that the pick is stable.
    live.sort_by(|a, b| b.cmp(a));
    live.into_iter().map(|(_, name)| name).collect()
}

/// The `wl_socket` of the instance `signature` from `hyprctl instances -j`.
async fn wayland_socket(env: &fleetdeck_core::ChildEnv, signature: &str) -> Option<String> {
    let runner = Runner { env, gate: None };
    let args = ["instances".to_owned(), "-j".to_owned()];
    let out = runner.run_ok("hyprctl", &args, CALL_TIMEOUT).await.ok()?;
    let list: serde_json::Value = serde_json::from_str(&out.stdout).ok()?;
    list.as_array()?
        .iter()
        .find(|i| i.get("instance").and_then(|v| v.as_str()) == Some(signature))?
        .get("wl_socket")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The Omarchy folder and its version, when `bin/omarchy` is executable.
fn omarchy(base: &ChildEnv) -> Option<(PathBuf, Option<String>)> {
    let root = base
        .get("OMARCHY_PATH")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_OMARCHY), PathBuf::from);
    let meta = std::fs::metadata(root.join("bin/omarchy")).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    let version = std::fs::read_to_string(root.join("version"))
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    Some((root, version))
}

/// Looks for the desktop of the station's environment `base`. `None` on a
/// system that is not Linux, without `XDG_RUNTIME_DIR`, without a live
/// Hyprland instance, or when `hyprctl` does not name its Wayland socket.
pub async fn detect(base: &ChildEnv) -> Option<Found> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let runtime_dir = PathBuf::from(base.get("XDG_RUNTIME_DIR").filter(|p| !p.is_empty())?);
    let mut live = live_instances(&runtime_dir).await.into_iter();
    let signature = live.next()?;
    for other in live {
        tracing::debug!(instance = %other, "another live Hyprland instance is ignored");
    }
    let probe = probe_env(base, &runtime_dir).ok()?;
    let wayland = wayland_socket(&probe, &signature).await?;
    let omarchy = omarchy(base);
    Some(Found {
        kind: if omarchy.is_some() {
            DesktopKind::Omarchy
        } else {
            DesktopKind::Hyprland
        },
        signature,
        runtime_dir,
        wayland,
        omarchy_version: omarchy.as_ref().and_then(|(_, v)| v.clone()),
        omarchy: omarchy.map(|(root, _)| root),
    })
}
