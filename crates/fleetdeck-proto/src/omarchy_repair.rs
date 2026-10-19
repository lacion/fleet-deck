//! The installer repair of Omarchy (`board-gaps.md` §3, BG-2): the asset IDs,
//! the codes and the JSON shapes of the CLI commands `omarchy install-plan`,
//! `install --replace-conflicts --plan-stdin`, `install --resume`,
//! `repair-status`, `restore-plan` and `restore --plan-stdin`. Later they
//! are the wire answers of the station.
//!
//! `Option` fields serialize as `null`; they are never skipped.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The codes of a refused or stopped repair.
pub mod code {
    /// A premise of the reviewed plan changed.
    pub const PLAN_CHANGED: &str = "plan_changed";
    /// A path cannot be replaced safely.
    pub const UNSAFE_PATH: &str = "unsafe_path";
    /// A copy of a file could not be kept.
    pub const BACKUP_FAILED: &str = "backup_failed";
    /// The repair stopped after it changed some files.
    pub const REPAIR_INCOMPLETE: &str = "repair_incomplete";
    /// A current file changed after the repair.
    pub const RESTORE_CONFLICT: &str = "restore_conflict";
    /// The repair was started by another build.
    pub const BUILD_MISMATCH: &str = "build_mismatch";
}

/// One installed file that a repair can replace. The menu file has no ID: it
/// is never replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum OmarchyAssetId {
    FleetPluginManifest,
    FleetPluginWidget,
    FleetPluginPanel,
    FleetPluginService,
    FleetPluginModel,
    FleetPluginI18n,
    HookPostBoot,
    HookPostUpdate,
    HookThemeSet,
    ThemeTemplate,
    /// The `hypr.lua` asset.
    FleetBindings,
}

impl OmarchyAssetId {
    /// The wire name (the serde name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FleetPluginManifest => "fleet_plugin_manifest",
            Self::FleetPluginWidget => "fleet_plugin_widget",
            Self::FleetPluginPanel => "fleet_plugin_panel",
            Self::FleetPluginService => "fleet_plugin_service",
            Self::FleetPluginModel => "fleet_plugin_model",
            Self::FleetPluginI18n => "fleet_plugin_i18n",
            Self::HookPostBoot => "hook_post_boot",
            Self::HookPostUpdate => "hook_post_update",
            Self::HookThemeSet => "hook_theme_set",
            Self::ThemeTemplate => "theme_template",
            Self::FleetBindings => "fleet_bindings",
        }
    }
}

/// The folder that a path is relative to, below the config home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export_to = "omarchy/")]
pub enum AssetBase {
    Omarchy,
    Fleetdeck,
}

/// Why a file is in the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum PlanClassification {
    /// An unrecorded file differs from the asset.
    UnrecordedConflict,
    /// A recorded file differs from its recorded hash and from the asset.
    RecordedEdit,
}

/// Why a file cannot be replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum PlanBlock {
    UnsafePath,
}

/// One file of the install plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct PlanFile {
    pub asset_id: OmarchyAssetId,
    /// The absolute path.
    pub path: String,
    pub classification: PlanClassification,
    pub base: AssetBase,
    /// The path relative to the base, as the install record stores it.
    pub rel: String,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    /// The permission bits (`st_mode & 0o7777`).
    pub mode: Option<u32>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub replaceable: bool,
    pub blocked: Option<PlanBlock>,
}

/// The facts of `fleetdeck omarchy install-plan --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct InstallPlanFacts {
    pub build_hash: String,
    pub record_sha256: Option<String>,
    pub files: Vec<PlanFile>,
}

/// The stdin of `install --replace-conflicts --plan-stdin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "omarchy/")]
pub struct InstallPlanInput {
    pub facts: InstallPlanFacts,
    pub replace_recorded_edits: Vec<OmarchyAssetId>,
}

/// What a command did to one file (the state after the run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum RepairFileState {
    Unchanged,
    BackedUp,
    Replaced,
    Restored,
}

/// One file of a [`RepairResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RepairFile {
    pub asset_id: OmarchyAssetId,
    /// The absolute path of the target.
    pub path: String,
    /// The absolute path of the kept copy; `null` when there is none.
    pub backup_path: Option<String>,
    pub state: RepairFileState,
}

/// The stdout of `install --replace-conflicts`, `install --resume` and
/// `restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RepairResult {
    /// A code of [`code`]; `null` when the command did what it was asked.
    pub code: Option<String>,
    /// `null` before a journal exists.
    pub repair_id: Option<String>,
    pub files: Vec<RepairFile>,
}

/// What the files on disk say about one file of a repair (the decision
/// table of `board-gaps.md` §3.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum ObservedState {
    NothingMoved,
    BackedUp,
    Published,
    Restored,
    Conflict,
    Incomplete,
}

/// The state of one journal, from its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum JournalState {
    Complete,
    Restored,
    Incomplete,
}

/// One file of a [`RepairJournalView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RepairStatusFile {
    pub asset_id: OmarchyAssetId,
    pub state: ObservedState,
    /// The absolute path of the kept copy; `null` when it is not on disk.
    pub backup_path: Option<String>,
}

/// One journal in `repair-status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RepairJournalView {
    pub repair_id: String,
    /// Unix milliseconds.
    pub created_at: i64,
    pub state: JournalState,
    pub files: Vec<RepairStatusFile>,
}

/// The stdout of `fleetdeck omarchy repair-status --json`: the last 8
/// journals, the newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RepairStatusView {
    pub journals: Vec<RepairJournalView>,
}

/// What a restore does to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "omarchy/")]
pub enum RestoreAction {
    /// Copy the kept copy to the absent target (no replace).
    CopyBackup,
    /// Move the target to a new kept name, then copy the kept copy to it.
    MoveAsideThenCopyBackup,
    /// Remove the published target (the file did not exist before).
    RemovePublished,
    Nothing,
    StopRestoreConflict,
    StopRepairIncomplete,
}

/// One file of a restore plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RestorePlanFile {
    pub asset_id: OmarchyAssetId,
    /// The absolute path of the target.
    pub path: String,
    pub target_sha256: Option<String>,
    /// The absolute path of the kept copy.
    pub backup_path: String,
    pub backup_sha256: Option<String>,
    pub action: RestoreAction,
}

/// The facts of `fleetdeck omarchy restore-plan --repair <id> --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "omarchy/")]
pub struct RestorePlanFacts {
    pub repair_id: String,
    pub files: Vec<RestorePlanFile>,
}

/// The stdin of `restore --plan-stdin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "omarchy/")]
pub struct RestorePlanInput {
    pub facts: RestorePlanFacts,
    pub asset_ids: Vec<OmarchyAssetId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [OmarchyAssetId; 11] = [
        OmarchyAssetId::FleetPluginManifest,
        OmarchyAssetId::FleetPluginWidget,
        OmarchyAssetId::FleetPluginPanel,
        OmarchyAssetId::FleetPluginService,
        OmarchyAssetId::FleetPluginModel,
        OmarchyAssetId::FleetPluginI18n,
        OmarchyAssetId::HookPostBoot,
        OmarchyAssetId::HookPostUpdate,
        OmarchyAssetId::HookThemeSet,
        OmarchyAssetId::ThemeTemplate,
        OmarchyAssetId::FleetBindings,
    ];

    #[test]
    fn the_asset_names_match_the_serde_names() {
        for id in ALL {
            assert_eq!(serde_json::to_value(id).unwrap(), json!(id.as_str()));
        }
        assert_eq!(ALL[0].as_str(), "fleet_plugin_manifest");
        assert_eq!(ALL[10].as_str(), "fleet_bindings");
    }

    #[test]
    fn the_codes_are_fixed() {
        assert_eq!(code::PLAN_CHANGED, "plan_changed");
        assert_eq!(code::UNSAFE_PATH, "unsafe_path");
        assert_eq!(code::BACKUP_FAILED, "backup_failed");
        assert_eq!(code::REPAIR_INCOMPLETE, "repair_incomplete");
        assert_eq!(code::RESTORE_CONFLICT, "restore_conflict");
        assert_eq!(code::BUILD_MISMATCH, "build_mismatch");
    }

    #[test]
    fn a_result_serializes_with_nulls() {
        let result = RepairResult {
            code: None,
            repair_id: None,
            files: vec![RepairFile {
                asset_id: OmarchyAssetId::HookThemeSet,
                path: "/home/o/hook".into(),
                backup_path: None,
                state: RepairFileState::Unchanged,
            }],
        };
        let value = json!({
            "code": null, "repair_id": null,
            "files": [{"asset_id": "hook_theme_set", "path": "/home/o/hook",
                       "backup_path": null, "state": "unchanged"}]
        });
        assert_eq!(serde_json::to_value(&result).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<RepairResult>(value).unwrap(),
            result
        );
    }

    #[test]
    fn the_inputs_reject_unknown_keys() {
        let facts = json!({"build_hash": "ab", "record_sha256": null, "files": []});
        let ok = json!({"facts": facts, "replace_recorded_edits": ["fleet_bindings"]});
        let input: InstallPlanInput = serde_json::from_value(ok).unwrap();
        assert_eq!(
            input.replace_recorded_edits,
            vec![OmarchyAssetId::FleetBindings]
        );
        let extra = json!({"facts": facts, "replace_recorded_edits": [], "x": 1});
        assert!(serde_json::from_value::<InstallPlanInput>(extra).is_err());
        let unknown = json!({"facts": facts, "replace_recorded_edits": ["menu"]});
        assert!(serde_json::from_value::<InstallPlanInput>(unknown).is_err());
    }

    #[test]
    fn the_enums_use_snake_case() {
        assert_eq!(
            serde_json::to_value(RestoreAction::MoveAsideThenCopyBackup).unwrap(),
            json!("move_aside_then_copy_backup")
        );
        assert_eq!(
            serde_json::to_value(PlanClassification::UnrecordedConflict).unwrap(),
            json!("unrecorded_conflict")
        );
        assert_eq!(
            serde_json::to_value(ObservedState::NothingMoved).unwrap(),
            json!("nothing_moved")
        );
        assert_eq!(
            serde_json::to_value(AssetBase::Fleetdeck).unwrap(),
            json!("fleetdeck")
        );
    }
}
