//! The status of `omarchy.status` (design `omarchy-admin.md` §2.1, §3.2).
//!
//! The install and screensaver facts come only from the JSON of
//! `fleetdeck omarchy status --json`, read whatever the exit code. The
//! provisioning state comes from `provision.rs`.

use fleetdeck_proto::omarchy_admin::{
    OmarchyAction, OmarchyInstall, OmarchyPresence, OmarchyScreensaver, OmarchyStatus,
};
use serde_json::Value;

use super::child::RunResult;
use super::{OmarchyAdmin, provision};

/// The only `blocked` value that the station keeps.
const BLOCKED: &str = "kept_original_differs";

impl OmarchyAdmin {
    /// The present state. `running` is set by the caller.
    pub(super) async fn build_status(&self, running: Option<OmarchyAction>) -> OmarchyStatus {
        let present = crate::desktop::detect::omarchy_root(&self.cfg.omarchy_env);
        let (install, screensaver) = match &present {
            Some(_) => self.install_facts().await,
            None => (None, None),
        };
        OmarchyStatus {
            omarchy: OmarchyPresence {
                present: present.is_some(),
                version: present.and_then(|(_, version)| version),
            },
            install,
            screensaver,
            provision: provision::view(&self.cfg.station_dir, self.last_retry().as_deref()),
            running,
        }
    }

    /// Runs `omarchy status --json` and parses its stdout.
    async fn install_facts(&self) -> (Option<OmarchyInstall>, Option<OmarchyScreensaver>) {
        let out = self
            .run_tail(&["omarchy", "status", "--json"], self.cfg.limits.status)
            .await;
        match out {
            Ok(RunResult::Exit { stdout, .. }) => match parse_status(&stdout) {
                Some((install, screensaver)) => (Some(install), screensaver),
                None => (None, None),
            },
            Ok(_) => (None, None),
            Err(e) => {
                tracing::debug!(error = %e, "omarchy status did not run");
                (None, None)
            }
        }
    }
}

/// Parses the JSON of `omarchy status --json`. A missing key or a wrong type
/// is a failure; an unknown key is ignored.
pub(super) fn parse_status(stdout: &[u8]) -> Option<(OmarchyInstall, Option<OmarchyScreensaver>)> {
    let root: Value = serde_json::from_slice(stdout).ok()?;
    let installed = root.get("installed")?.as_bool()?;
    let plugin_enabled = root.get("plugin_enabled")?.as_bool()?;
    let outdated = root.get("outdated")?.as_bool()?;
    let hyprland_line = root.get("hyprland_line")?.as_str()? == "ok";
    let (mut ok, mut changed, mut missing) = (0_u32, 0_u32, 0_u32);
    for file in root.get("files")?.as_array()? {
        match file.get("state")?.as_str()? {
            "ok" => ok = ok.saturating_add(1),
            "changed" => changed = changed.saturating_add(1),
            "missing" => missing = missing.saturating_add(1),
            _ => {}
        }
    }
    let screensaver = match root.get("screensaver")? {
        Value::Null => None,
        Value::Object(map) => {
            let enabled = map.get("enabled")?.as_bool()?;
            let file_restored = match map.get("file_restored")? {
                Value::Null => None,
                other => Some(other.as_bool()?),
            };
            let blocked = match map.get("blocked")? {
                Value::Null => None,
                other => Some(other.as_str()?).filter(|b| *b == BLOCKED),
            };
            Some(OmarchyScreensaver {
                enabled,
                file_restored,
                blocked: blocked.map(str::to_owned),
            })
        }
        _ => return None,
    };
    Some((
        OmarchyInstall {
            installed,
            files_ok: ok,
            files_changed: changed,
            files_missing: missing,
            plugin_enabled,
            hyprland_line,
            outdated,
        },
        screensaver,
    ))
}
