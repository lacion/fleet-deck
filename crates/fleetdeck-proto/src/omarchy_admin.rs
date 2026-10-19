//! Omarchy management on the wire (design `omarchy-admin.md` §2): the params
//! and answers of the link methods `omarchy.status` and `omarchy.act`
//! (feature `omarchy_admin.v1`, deck → station, scope `admin`, not
//! receipt-mutating).
//!
//! `omarchy.status` takes no params (`null` or `{}`). `Option` fields
//! serialize as `null`; they are never skipped.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Wire error codes and `failed` codes of the Omarchy methods.
pub mod code {
    /// The request needs the owner's elevation (`elevated: false`).
    pub const ELEVATION_REQUIRED: &str = "elevation_required";
    /// Another Omarchy action runs on the station.
    pub const BUSY: &str = "busy";
    /// Omarchy is not present on the station.
    pub const OMARCHY_MISSING: &str = "omarchy_missing";
    /// A provisioning retry is not possible in the state of the station.
    pub const NOT_PROVISIONED: &str = "not_provisioned";
    /// The params are not valid.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// The installed integration is older than this build.
    pub const INTEGRATION_OUTDATED: &str = "integration_outdated";
    /// The installer refused (exit 1).
    pub const INSTALL_REFUSED: &str = "install_refused";
    /// The uninstall left something behind (exit 1).
    pub const UNINSTALL_INCOMPLETE: &str = "uninstall_incomplete";
    /// The screensaver command refused (exit 1).
    pub const SCREENSAVER_REFUSED: &str = "screensaver_refused";
    /// The child ran past its time limit.
    pub const TIMEOUT: &str = "timeout";
    /// The child wrote more output than the station reads.
    pub const OUTPUT_TOO_LARGE: &str = "output_too_large";
    /// Any other failure.
    pub const FAILED: &str = "failed";
}

/// The exit code of an Omarchy CLI command that waited 10 s for the Omarchy
/// lock and did not get it (DL-243 item b, `board-gaps.md` §3.8). The station
/// maps it to [`code::BUSY`]; exit 1 keeps the refusal meaning.
pub const LOCK_BUSY_EXIT: u8 = 3;

/// One action of `omarchy.act`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum OmarchyAction {
    Install,
    Uninstall,
    ScreensaverOn,
    ScreensaverOff,
    ScreensaverRebaseline,
    ProvisionRetry,
}

/// `omarchy.act` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyActParams {
    pub action: OmarchyAction,
    pub elevated: bool,
}

/// Whether Omarchy is present on the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyPresence {
    pub present: bool,
    pub version: Option<String>,
}

/// The install facts of `fleetdeck omarchy status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyInstall {
    pub installed: bool,
    pub files_ok: u32,
    pub files_changed: u32,
    pub files_missing: u32,
    pub plugin_enabled: bool,
    pub hyprland_line: bool,
    pub outdated: bool,
}

/// The screensaver facts of `fleetdeck omarchy status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyScreensaver {
    pub enabled: bool,
    pub file_restored: Option<bool>,
    pub blocked: Option<String>,
}

/// The provisioning state of the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct ProvisionView {
    pub phase: Option<String>,
    pub error: Option<String>,
    pub bootstrap_running: bool,
    pub last_retry: Option<String>,
}

/// The answer to `omarchy.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyStatus {
    pub omarchy: OmarchyPresence,
    pub install: Option<OmarchyInstall>,
    pub screensaver: Option<OmarchyScreensaver>,
    pub provision: Option<ProvisionView>,
    pub running: Option<OmarchyAction>,
}

/// How an action ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum ActOutcome {
    Done,
    Failed,
    /// The action runs on; the status tells when it ends.
    Started,
}

/// The answer to `omarchy.act`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct OmarchyActResult {
    pub outcome: ActOutcome,
    pub code: Option<String>,
    pub message: Option<String>,
    pub status: OmarchyStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round<T>(value: &T, expected: serde_json::Value)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let text = serde_json::to_value(value).unwrap();
        assert_eq!(text, expected);
        let back: T = serde_json::from_value(expected).unwrap();
        assert_eq!(&back, value);
    }

    #[test]
    fn actions_use_snake_case() {
        for (action, name) in [
            (OmarchyAction::Install, "install"),
            (OmarchyAction::Uninstall, "uninstall"),
            (OmarchyAction::ScreensaverOn, "screensaver_on"),
            (OmarchyAction::ScreensaverOff, "screensaver_off"),
            (
                OmarchyAction::ScreensaverRebaseline,
                "screensaver_rebaseline",
            ),
            (OmarchyAction::ProvisionRetry, "provision_retry"),
        ] {
            round(&action, json!(name));
        }
    }

    #[test]
    fn outcomes_use_snake_case() {
        round(&ActOutcome::Done, json!("done"));
        round(&ActOutcome::Failed, json!("failed"));
        round(&ActOutcome::Started, json!("started"));
    }

    #[test]
    fn act_params_reject_unknown_keys_and_unknown_actions() {
        let ok: OmarchyActParams =
            serde_json::from_value(json!({"action": "install", "elevated": true})).unwrap();
        assert_eq!(ok.action, OmarchyAction::Install);
        assert!(ok.elevated);
        assert!(
            serde_json::from_value::<OmarchyActParams>(
                json!({"action": "install", "elevated": true, "extra": 1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<OmarchyActParams>(
                json!({"action": "reboot", "elevated": true})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<OmarchyActParams>(json!({"action": "install"})).is_err());
    }

    #[test]
    fn options_serialize_as_null() {
        round(
            &OmarchyPresence {
                present: false,
                version: None,
            },
            json!({"present": false, "version": null}),
        );
        round(
            &OmarchyScreensaver {
                enabled: true,
                file_restored: None,
                blocked: None,
            },
            json!({"enabled": true, "file_restored": null, "blocked": null}),
        );
        round(
            &ProvisionView {
                phase: None,
                error: Some("unknown_error".into()),
                bootstrap_running: true,
                last_retry: None,
            },
            json!({"phase": null, "error": "unknown_error", "bootstrap_running": true,
                   "last_retry": null}),
        );
    }

    #[test]
    fn install_serializes_every_field() {
        round(
            &OmarchyInstall {
                installed: true,
                files_ok: 3,
                files_changed: 1,
                files_missing: 2,
                plugin_enabled: true,
                hyprland_line: false,
                outdated: true,
            },
            json!({"installed": true, "files_ok": 3, "files_changed": 1, "files_missing": 2,
                   "plugin_enabled": true, "hyprland_line": false, "outdated": true}),
        );
    }

    #[test]
    fn status_and_result_serialize_with_nulls() {
        let status = OmarchyStatus {
            omarchy: OmarchyPresence {
                present: true,
                version: Some("3.1".into()),
            },
            install: None,
            screensaver: None,
            provision: None,
            running: Some(OmarchyAction::ProvisionRetry),
        };
        let status_json = json!({
            "omarchy": {"present": true, "version": "3.1"},
            "install": null, "screensaver": null, "provision": null,
            "running": "provision_retry"
        });
        round(&status, status_json.clone());
        round(
            &OmarchyActResult {
                outcome: ActOutcome::Failed,
                code: Some(code::INSTALL_REFUSED.into()),
                message: None,
                status,
            },
            json!({"outcome": "failed", "code": "install_refused", "message": null,
                   "status": status_json}),
        );
    }

    #[test]
    fn the_codes_are_fixed() {
        assert_eq!(code::ELEVATION_REQUIRED, "elevation_required");
        assert_eq!(code::BUSY, "busy");
        assert_eq!(code::OMARCHY_MISSING, "omarchy_missing");
        assert_eq!(code::NOT_PROVISIONED, "not_provisioned");
        assert_eq!(code::INVALID_REQUEST, "invalid_request");
        assert_eq!(code::INTEGRATION_OUTDATED, "integration_outdated");
        assert_eq!(code::INSTALL_REFUSED, "install_refused");
        assert_eq!(code::UNINSTALL_INCOMPLETE, "uninstall_incomplete");
        assert_eq!(code::SCREENSAVER_REFUSED, "screensaver_refused");
        assert_eq!(code::TIMEOUT, "timeout");
        assert_eq!(code::OUTPUT_TOO_LARGE, "output_too_large");
        assert_eq!(code::FAILED, "failed");
    }
}
