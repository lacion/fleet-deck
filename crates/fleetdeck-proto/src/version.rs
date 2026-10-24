//! Protocol versions and feature flags (PLAN §8.2 steps 2 and 5, L16).
//!
//! Behaviour depends on the negotiated feature set, not only on the version
//! number. Old fields stay for old peers; a new behaviour gets a new flag.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The oldest protocol version that this build accepts.
pub const PROTO_MIN: u32 = 1;
/// The newest protocol version that this build speaks.
pub const PROTO_MAX: u32 = 1;

/// Link and board feature flags of this build.
pub mod feature {
    /// Request / Response / Event / Ack / Ping on the control channel.
    pub const CONTROL: &str = "control.v1";
    /// Multiplexed byte streams with credit flow control on the data channel.
    pub const DATA_STREAMS: &str = "data_streams.v1";
    /// Stored accept/reject receipts for re-sent `command_id`s (§8.4).
    pub const COMMAND_RECEIPTS: &str = "command_receipts.v1";
    /// Snapshot batches when event replay is not possible (G4).
    pub const SNAPSHOTS: &str = "snapshots.v1";
    /// `session.revert`: conversation rollback plus workspace restore (PLAN
    /// §6.6, M3 DL-087). A peer without it never gets the method.
    pub const REVERT: &str = "revert.v1";
    /// `session.rename` (M3, DL-095).
    pub const RENAME: &str = "rename.v1";
    /// `session.set_auto_continue` (L19, ADR-007, M3 DL-127).
    pub const AUTO_CONTINUE: &str = "auto_continue.v1";
    /// `session.import`: a driven session that continues an existing native
    /// CLI session (M3 6-12, DL-308).
    pub const IMPORT: &str = "import.v1";
    /// `files.list`, `files.read`, `files.search` and `files.diff`: the
    /// read-only file service (PLAN §6.6, M3 DL-088).
    pub const FILES: &str = "files.v1";
    /// `shell.open`, `shell.resize`, `shell.close`, `shell.list` and
    /// `shell.attach`: the human shell (PLAN §6.8, M3 DL-404).
    pub const SHELL: &str = "shell.v1";
    /// `shell.handover` and `shell.release`: the TUI handover (DL-140).
    pub const SHELL_HANDOVER: &str = "shell.handover.v1";
    /// `session.coordinator_renew` and `session.coordinator_revoke` (the
    /// deck's binding lifecycle commands) and `coordinator.query` and
    /// `coordinator.call` (ADR-006 §6.1 `coordinator_v1`, M3 DL-212,
    /// DL-214).
    pub const COORDINATOR: &str = "coordinator.v1";
    /// ADR-008 fleet control: the new coordinator ops and queries,
    /// `authority` on routed session commands, `coordinator.authority_check`
    /// and `coordinator.policy_now`.
    pub const COORDINATOR_V2: &str = "coordinator.v2";
    /// `workspace.worktrees`: the read-only inventory of the worktrees that
    /// the station created for sessions (DL-142).
    pub const WORKTREES: &str = "workspace.worktrees.v1";
    /// `workspace.preflight`: the Git access check of a clone URL (DL-144).
    pub const PREFLIGHT: &str = "workspace.preflight.v1";
    /// `files.write_temp`: store a pasted image in the station's paste
    /// directory (DL-147).
    pub const WRITE_TEMP: &str = "files.write_temp.v1";
    /// `session.worktree_trash`, `_restore`, `_purge` and `_release`: the
    /// worktree trash (DL-150).
    pub const TRASH: &str = "workspace.trash.v1";
    /// `files.session_diff`: the working tree of a session against its
    /// base commit (R1.2).
    pub const SESSION_DIFF: &str = "files.session_diff.v1";

    /// `session.history`: the earlier conversation of a session, from the
    /// station's snapshot of the provider transcript (DL-189).
    pub const HISTORY: &str = "history.v1";

    /// `session.subagent`: the log of one subagent of a session, from the
    /// provider's own transcript of that subagent (DL-191).
    pub const SUBAGENT: &str = "subagent.v1";

    /// `session.workflows` (the workflow projection of a session, in pages)
    /// and `session.workflow_agent` (the transcript of one workflow agent)
    /// with the `workflow.updated` event (B12, design
    /// `docs/v2/design/workflow-agents.md` §4.2).
    pub const WORKFLOWS: &str = "workflows.v1";

    /// `station.native_sessions` and `station.native_preview`: browse the
    /// native sessions of a station's provider homes (B7).
    pub const NATIVE_SESSIONS: &str = "native_sessions.v1";

    /// `station.analytics`: the hour buckets of the transcript usage and the
    /// Fleet Deck counters of a station (B29).
    pub const ANALYTICS: &str = "analytics.v1";

    /// `session.branches` and `files.branch_diff`: the branches of the
    /// repository of a session and the committed diff of one (DL-191).
    pub const SESSION_BRANCHES: &str = "session_branches.v1";

    /// `session.execution_check` and `session.reconcile_execution`: the
    /// termination check and the owner's reconciliation for resume on the
    /// shim host (DL-182).
    pub const SHIM_RESUME: &str = "shim.resume.v1";

    /// `session.execution_stop_plan` and `session.stop_execution`: the stop
    /// plan and the board-origin command that stop the processes of a lost
    /// run through pidfds (BG-4).
    pub const EXECUTION_STOP: &str = "execution_stop.v1";

    /// The provisioning report of an unpaired key on the Pair channel (BG-3,
    /// `board-gaps.md` section 4). Optional: it is never required for
    /// ordinary pairing.
    pub const PROVISION_REPORT: &str = "provision_report.v1";

    /// The read `station.tools`: the desktop tool facts of the station
    /// (`board-gaps.md` §2.3).
    pub const STATION_TOOLS: &str = "station_tools.v1";

    /// The desktop tools plan and the maintenance channel (`board-gaps.md`
    /// §2.4, §2.9).
    pub const DESKTOP_TOOL_INSTALL: &str = "desktop_tool_install.v1";

    /// `artifacts.publish`, `artifacts.list`, `artifacts.get` and
    /// `artifacts.file`: the artifact service (R12, DL-165).
    pub const ARTIFACTS: &str = "artifacts.v1";

    /// `workspace.precheck`: the shell precheck of a schedule, run on the
    /// station (R5, DL-168).
    pub const PRECHECK: &str = "workspace.precheck.v1";

    /// `integrations.status`, `issues.*` and `prs.*`: GitHub, GitLab, Jira
    /// and Linear through the station (R3, DL-164).
    pub const INTEGRATIONS: &str = "integrations.v1";

    /// `assist.generate`: model help on the station (R10, DL-167).
    pub const ASSIST: &str = "assist.v1";

    /// `accounts.*` and `issues.containers`: the connected GitHub, GitLab,
    /// Jira and Linear accounts of a station (DL-173).
    pub const ACCOUNTS: &str = "accounts.v1";

    /// `settings.get`, `settings.set`, `settings.secret_set` and
    /// `station.restart`: the settings of a station and its restart (DL-172).
    pub const SETTINGS: &str = "settings.v1";

    /// `PairRequest`, `PairPending`, `PairApproved` and `PairResume` on the
    /// pair channel: a station asks a deck to pair without an invitation,
    /// and the owner approves on the board (DL-178, ADR-004 §10).
    pub const PAIR_REQUEST: &str = "pair_request.v1";

    /// `session.shell_command` and `session.release_shell`: a `!` shell
    /// command in a session folder and the owner's release of a folder
    /// (B8, DL-196).
    pub const SHELL_COMMAND: &str = "shell_command.v1";

    /// `session.usage_recount` and the `usage.set` event: the owner's recount
    /// of an old usage count from the provider record (B20, DL-448).
    pub const USAGE_RECOUNT: &str = "usage_recount.v1";

    /// `session.import` with a `transcript` pin: the station verifies the pin
    /// before any launch (B7).
    pub const IMPORT_PIN: &str = "import_pin.v1";

    /// `desktop.info` and `desktop.act`: desktop control on the station (R13
    /// §6, DL-201).
    pub const DESKTOP: &str = "desktop.v1";

    /// `fleet_git.projects_set`, `fleet_git.push_now`, `SessionCommand::Start`
    /// `fleet_git` and the Fleet Git phase 1 events (ADR-009).
    pub const FLEET_GIT: &str = "fleet_git.v1";

    /// `project.ensure_home`, `project.home_status`,
    /// `fleet_git.artifact_authorize`, `fleet_git.share_verdict`,
    /// `FileRoot::Artifacts` and the artifact events (Fleet Git phase 2,
    /// ADR-009).
    pub const FLEET_GIT_HOMES: &str = "fleet_git.homes.v1";

    /// `project.ensure_home` with `adopt_path` (Fleet Sync).
    pub const FLEET_GIT_HOMES_ADOPT: &str = "fleet_git.homes_adopt.v1";

    /// `project.radar`, `session.land`, `fleet_git.land_status`,
    /// `fleet_git.land_abandon` and the `session.land` event (Fleet Git
    /// phase 3, ADR-009).
    pub const FLEET_GIT_LAND: &str = "fleet_git.land.v1";

    /// `project.branches` and `project.fetch` (Fleet Git phase 4, ADR-009).
    pub const FLEET_GIT_BRANCHES: &str = "fleet_git.branches.v1";

    /// `session.move_out`, `session.move_export`, `session.move_prepare`,
    /// `session.move_in`, `session.move_finish`, `session.move_release`,
    /// `session.move_abort`, `session.move_discard`, `session.move_status`,
    /// `session.move_fail`, `session.move_undo`, the stream purposes and the
    /// events `session.moved`, `session.move_in`, `session.move_undone` (Fleet
    /// Git phase 4, ADR-009).
    pub const FLEET_GIT_MOVE: &str = "fleet_git.move.v1";

    /// `project.secret_scan`, `project.secret_read`, `project.secret_write`
    /// (Fleet Sync FS-3).
    pub const FLEET_GIT_SECRETS: &str = "fleet_git.secrets.v1";

    /// `project.secret_browse` (Fleet Sync).
    pub const FLEET_GIT_SECRETS_BROWSE: &str = "fleet_git.secrets_browse.v1";

    /// `desktop.act` with an `authority` param: the deck authorizes each
    /// desktop action, the station checks it right before the first side
    /// effect (R13 §5E item 1, §5F, §5G).
    pub const DESKTOP_AUTHORITY: &str = "desktop_authority.v1";

    /// `files.display_changes`: protected change statistics and patches for
    /// a wall display (R13 §5D item 13).
    pub const DISPLAY_CHANGES: &str = "display_changes.v1";

    /// `fleet.summary`, `display.directive` and `station.palette`: the fleet
    /// summary, the wall hotkeys and the theme palette of the Omarchy desktop
    /// shell (R13 P4, D1).
    pub const DESKTOP_SHELL: &str = "desktop_shell.v1";

    /// R13 P5a: owner screenshot and its result lookup.
    pub const DESKTOP_CAPTURE: &str = "desktop_capture.v1";

    /// R13 P5a: station artifact deletion.
    pub const ARTIFACTS_DELETE: &str = "artifacts_delete.v1";

    /// R13 P6: the coordinator desktop tools (`coordinator.desktop_call`,
    /// `coordinator.desktop_query`, `desktop.authority_check`,
    /// `desktop.policy_now`, `desktop.coordinator_read`).
    pub const DESKTOP_MCP: &str = "desktop_mcp.v1";

    /// R13 P6: the coordinator screenshot tools (needs `desktop_mcp.v1`).
    pub const CAPTURE_MCP: &str = "capture_mcp.v1";

    /// R13 P5b: the owner recording action, recording status and progress,
    /// active recording discovery, stop, live policy checks and continuation
    /// leases.
    pub const DESKTOP_RECORDING: &str = "desktop_recording.v1";

    /// R13 P5b: the `video` artifact kind, recording provenance and the HTTP
    /// video and `Range` contract.
    pub const ARTIFACTS_VIDEO: &str = "artifacts_video.v1";

    /// R13 P5b-2: the coordinator recording tool `record_monitor`, the grant
    /// fields of a recording allowance and the recording status projection
    /// (needs `desktop_recording.v1` and `capture_mcp.v1`).
    pub const CAPTURE_RECORDING_MCP: &str = "capture_recording_mcp.v1";

    /// Omarchy management from the board: `omarchy.status`, `omarchy.act`
    /// (design `omarchy-admin.md`).
    pub const OMARCHY_ADMIN: &str = "omarchy_admin.v1";

    /// The installer repair of Omarchy (`board-gaps.md` section 3): the
    /// reads `omarchy.install_plan`, `omarchy.repair_status` and
    /// `omarchy.restore_plan`, and the `omarchy.act` actions
    /// `install_replace`, `install_resume` and `restore`.
    pub const OMARCHY_REPAIR: &str = "omarchy_repair.v1";

    /// R13 P7: the coordinator input tools `key_press`, `type_text` and
    /// `pointer_move`, the input grant fields, the input target actions, the
    /// `input` policy purpose and `DesktopInfo.host_addresses` (needs
    /// `desktop_mcp.v1` and `capture_mcp.v1`). It means support for the
    /// protocol: it does not attest that `wtype` is installed.
    pub const DESKTOP_INPUT_MCP: &str = "desktop_input_mcp.v1";

    /// R13 P7b: the coordinator tool `pointer_click`, the click grant field,
    /// the click target actions and the `input_click` policy purpose (needs
    /// `desktop_input_mcp.v1`). It means support for the protocol: it does
    /// not attest that `wlrctl` is installed.
    pub const DESKTOP_CLICK_MCP: &str = "desktop_click_mcp.v1";

    /// R13 P7b: the `any_window` input scope, the coordinator tool
    /// `window_list`, the `input_windows` protected read, the broader target
    /// actions and the `input_any` policy purposes (needs
    /// `desktop_input_mcp.v1`).
    pub const DESKTOP_INPUT_ANY: &str = "desktop_input_any.v1";

    /// `fleet.view`: the fleet view that every session reads, with or without
    /// a coordinator grant (MCP P3, design `docs/v2/design/mcp-p3.md` §2).
    pub const FLEET_VIEW: &str = "fleet_view.v1";

    /// `worker_grant.v1`: the station accepts a `CoordinatorSpec` with
    /// `kind: worker` from the paired deck (MCP P4a, design
    /// `docs/v2/design/mcp-p4a.md` §6.1).
    pub const WORKER_GRANT: &str = "worker_grant.v1";
    /// `session.coordinator_set`: live changes of a session's grant and the
    /// per-session grant record (MCP P4b §9).
    pub const COORDINATOR_LIVE: &str = "coordinator_live.v1";
    /// `session.set_effort`, `SessionSpec.effort` and the effort events
    /// `effort.set_result` and `effort.chosen` (B18 Phase 2, design
    /// `docs/v2/design/effort.md` §9).
    pub const SESSION_EFFORT: &str = "session_effort.v1";

    /// B17 §3.1: the wall control relay and the owner remote (the routes
    /// `POST /api/displays/{id}/control`, `POST .../remote/navigate`, `GET
    /// .../remote`, and the `wall_control` frame of a display socket). A
    /// board feature: no station and no link method uses it.
    pub const WALL_CONTROL: &str = "wall_control.v1";

    /// B17 §3.2: the wall pages allow-list (the routes `GET|POST
    /// /api/wall-pages`, `POST /api/wall-pages/{id}/delete`) and, with B2,
    /// the `page` directive. A board feature: no station and no link method
    /// uses it.
    pub const WALL_PAGES: &str = "wall_pages.v1";

    /// MCP Apps (B15): the link methods `session.app_meta`,
    /// `session.app_document`, `session.app_call`, `session.app_calls` and
    /// `session.app_release`, and the `tool.app` event.
    pub const MCP_APPS: &str = "mcp_apps.v1";

    /// B17 §3.4: wall apps (the `app` flag of an `artifact` directive, the
    /// setting `allow_apps` and the route `GET /api/displays/self/app-feed/
    /// ...`). A board feature: no station and no link method uses it. The
    /// wall declares it in the header or in the query `features=` of the
    /// artifact request.
    pub const WALL_APPS: &str = "wall_apps.v1";

    /// Every feature that this build offers.
    pub const ALL: &[&str] = &[
        CONTROL,
        DATA_STREAMS,
        COMMAND_RECEIPTS,
        SNAPSHOTS,
        REVERT,
        RENAME,
        FILES,
        SHELL,
        SHELL_HANDOVER,
        COORDINATOR,
        COORDINATOR_V2,
        AUTO_CONTINUE,
        IMPORT,
        WORKTREES,
        PREFLIGHT,
        WRITE_TEMP,
        TRASH,
        SESSION_DIFF,
        HISTORY,
        SUBAGENT,
        NATIVE_SESSIONS,
        ANALYTICS,
        IMPORT_PIN,
        SESSION_BRANCHES,
        ARTIFACTS,
        PRECHECK,
        INTEGRATIONS,
        ASSIST,
        ACCOUNTS,
        SETTINGS,
        SHIM_RESUME,
        PAIR_REQUEST,
        SHELL_COMMAND,
        DESKTOP,
        FLEET_GIT,
        FLEET_GIT_HOMES,
        FLEET_GIT_HOMES_ADOPT,
        FLEET_GIT_LAND,
        FLEET_GIT_BRANCHES,
        FLEET_GIT_MOVE,
        FLEET_GIT_SECRETS,
        FLEET_GIT_SECRETS_BROWSE,
        DESKTOP_AUTHORITY,
        DISPLAY_CHANGES,
        DESKTOP_SHELL,
        DESKTOP_CAPTURE,
        ARTIFACTS_DELETE,
        DESKTOP_MCP,
        CAPTURE_MCP,
        DESKTOP_RECORDING,
        ARTIFACTS_VIDEO,
        CAPTURE_RECORDING_MCP,
        OMARCHY_ADMIN,
        OMARCHY_REPAIR,
        DESKTOP_INPUT_MCP,
        DESKTOP_CLICK_MCP,
        DESKTOP_INPUT_ANY,
        FLEET_VIEW,
        WORKER_GRANT,
        COORDINATOR_LIVE,
        EXECUTION_STOP,
        PROVISION_REPORT,
        STATION_TOOLS,
        DESKTOP_TOOL_INSTALL,
        WORKFLOWS,
        WALL_CONTROL,
        WALL_PAGES,
        USAGE_RECOUNT,
        SESSION_EFFORT,
        MCP_APPS,
        WALL_APPS,
    ];
    /// Features without which a link is refused.
    pub const REQUIRED: &[&str] = &[CONTROL];
}

/// What one side offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct VersionOffer {
    pub proto_min: u32,
    pub proto_max: u32,
    pub features: Vec<String>,
}

impl VersionOffer {
    /// The offer of this build.
    #[must_use]
    pub fn this_build() -> Self {
        Self {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            features: feature::ALL.iter().map(|f| (*f).to_owned()).collect(),
        }
    }
}

/// The result of a successful negotiation; both sides compute the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct Negotiated {
    pub version: u32,
    /// Sorted intersection of both feature lists.
    pub features: Vec<String>,
}

impl Negotiated {
    /// True if both sides offered `feature`.
    #[must_use]
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

/// Why a negotiation failed. Both sides report the same reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiationError {
    #[error(
        "no common protocol version (local {local_min}..={local_max}, peer {peer_min}..={peer_max})"
    )]
    NoCommonVersion {
        local_min: u32,
        local_max: u32,
        peer_min: u32,
        peer_max: u32,
    },
    #[error("an offer has proto_min {min} above proto_max {max}")]
    InvalidRange { min: u32, max: u32 },
    #[error("required feature `{0}` is not offered by both sides")]
    MissingRequiredFeature(String),
}

impl NegotiationError {
    /// Stable reason code for `close` frames.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoCommonVersion { .. } | Self::InvalidRange { .. } => "no_common_version",
            Self::MissingRequiredFeature(_) => "missing_required_feature",
        }
    }
}

/// Picks the highest common version and the common features (§8.2 step 5).
///
/// The function is symmetric: `negotiate(a, b) == negotiate(b, a)`.
pub fn negotiate(
    local: &VersionOffer,
    peer: &VersionOffer,
) -> Result<Negotiated, NegotiationError> {
    // Check in a fixed order so that both sides report the same error.
    let mut ranges = [
        (local.proto_min, local.proto_max),
        (peer.proto_min, peer.proto_max),
    ];
    ranges.sort_unstable();
    for offer in ranges.map(|(proto_min, proto_max)| VersionOffer {
        proto_min,
        proto_max,
        features: Vec::new(),
    }) {
        if offer.proto_min > offer.proto_max {
            return Err(NegotiationError::InvalidRange {
                min: offer.proto_min,
                max: offer.proto_max,
            });
        }
    }
    let low = local.proto_min.max(peer.proto_min);
    let high = local.proto_max.min(peer.proto_max);
    if low > high {
        // Report the ranges in a fixed order so both sides print the same text.
        let (a, b) = if (local.proto_min, local.proto_max) <= (peer.proto_min, peer.proto_max) {
            (local, peer)
        } else {
            (peer, local)
        };
        return Err(NegotiationError::NoCommonVersion {
            local_min: a.proto_min,
            local_max: a.proto_max,
            peer_min: b.proto_min,
            peer_max: b.proto_max,
        });
    }
    let ours: BTreeSet<&str> = local.features.iter().map(String::as_str).collect();
    let theirs: BTreeSet<&str> = peer.features.iter().map(String::as_str).collect();
    let common: Vec<String> = ours
        .intersection(&theirs)
        .map(|f| (*f).to_owned())
        .collect();
    for required in feature::REQUIRED {
        if !common.iter().any(|f| f == required) {
            return Err(NegotiationError::MissingRequiredFeature(
                (*required).to_owned(),
            ));
        }
    }
    Ok(Negotiated {
        version: high,
        features: common,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn offer(min: u32, max: u32, features: &[&str]) -> VersionOffer {
        VersionOffer {
            proto_min: min,
            proto_max: max,
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    #[test]
    fn the_p7b_features_have_their_names_and_follow_the_input_feature_in_all() {
        assert_eq!(feature::DESKTOP_CLICK_MCP, "desktop_click_mcp.v1");
        assert_eq!(feature::DESKTOP_INPUT_ANY, "desktop_input_any.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(
            at(feature::DESKTOP_CLICK_MCP),
            at(feature::DESKTOP_INPUT_MCP) + 1
        );
        assert_eq!(
            at(feature::DESKTOP_INPUT_ANY),
            at(feature::DESKTOP_INPUT_MCP) + 2
        );
        let this = VersionOffer::this_build();
        assert!(this.features.iter().any(|f| f == "desktop_click_mcp.v1"));
        assert!(this.features.iter().any(|f| f == "desktop_input_any.v1"));
    }

    #[test]
    fn the_desktop_tool_features_have_their_names_and_follow_provision_report_in_all() {
        assert_eq!(feature::STATION_TOOLS, "station_tools.v1");
        assert_eq!(feature::DESKTOP_TOOL_INSTALL, "desktop_tool_install.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(
            at(feature::STATION_TOOLS),
            at(feature::PROVISION_REPORT) + 1
        );
        assert_eq!(
            at(feature::DESKTOP_TOOL_INSTALL),
            at(feature::PROVISION_REPORT) + 2
        );
        // Later features (for example `workflows.v1`, `wall_control.v1`) follow in `ALL`.
    }

    #[test]
    fn the_workflows_feature_has_its_name_and_is_offered() {
        assert_eq!(feature::WORKFLOWS, "workflows.v1");
        assert!(feature::ALL.contains(&feature::WORKFLOWS));
        assert!(
            VersionOffer::this_build()
                .features
                .iter()
                .any(|f| f == "workflows.v1")
        );
    }

    #[test]
    fn the_session_effort_feature_follows_the_usage_recount_in_all() {
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::USAGE_RECOUNT) + 1, at(feature::SESSION_EFFORT));
    }

    #[test]
    fn the_usage_recount_feature_has_its_name_is_offered_and_not_required() {
        assert_eq!(feature::USAGE_RECOUNT, "usage_recount.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::USAGE_RECOUNT), at(feature::WALL_PAGES) + 1);
        assert!(!feature::REQUIRED.contains(&feature::USAGE_RECOUNT));
    }

    #[test]
    fn the_session_effort_feature_has_its_name_and_is_offered() {
        assert_eq!(feature::SESSION_EFFORT, "session_effort.v1");
        assert!(
            VersionOffer::this_build()
                .features
                .iter()
                .any(|f| f == "session_effort.v1")
        );
    }

    #[test]
    fn the_mcp_apps_feature_has_its_name_and_follows_the_session_effort_in_all() {
        assert_eq!(feature::MCP_APPS, "mcp_apps.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::SESSION_EFFORT) + 1, at(feature::MCP_APPS));
        assert!(
            VersionOffer::this_build()
                .features
                .iter()
                .any(|f| f == "mcp_apps.v1")
        );
    }

    #[test]
    fn the_wall_apps_feature_has_its_name_follows_the_mcp_apps_and_is_the_last_in_all() {
        assert_eq!(feature::WALL_APPS, "wall_apps.v1");
        assert_eq!(feature::ALL.last(), Some(&feature::WALL_APPS));
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::MCP_APPS) + 1, at(feature::WALL_APPS));
        assert!(!feature::REQUIRED.contains(&feature::WALL_APPS));
    }

    #[test]
    fn the_worker_grant_feature_has_its_name_and_follows_the_fleet_view_in_all() {
        assert_eq!(feature::WORKER_GRANT, "worker_grant.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::WORKER_GRANT), at(feature::FLEET_VIEW) + 1);
        assert!(
            VersionOffer::this_build()
                .features
                .iter()
                .any(|f| f == "worker_grant.v1")
        );
    }

    #[test]
    fn the_coordinator_live_feature_has_its_name_and_follows_the_worker_grant_in_all() {
        assert_eq!(feature::COORDINATOR_LIVE, "coordinator_live.v1");
        let at = |f: &str| feature::ALL.iter().position(|x| *x == f).unwrap();
        assert_eq!(at(feature::COORDINATOR_LIVE), at(feature::WORKER_GRANT) + 1);
        assert!(
            VersionOffer::this_build()
                .features
                .iter()
                .any(|f| f == "coordinator_live.v1")
        );
    }

    #[test]
    fn the_highest_common_version_and_the_common_features_win() {
        let a = offer(1, 3, &["control.v1", "snapshots.v1", "x.v1"]);
        let b = offer(2, 5, &["snapshots.v1", "control.v1"]);
        let n = negotiate(&a, &b).unwrap();
        assert_eq!(n.version, 3);
        assert_eq!(n.features, vec!["control.v1", "snapshots.v1"]);
        assert!(n.has("snapshots.v1"));
        assert!(!n.has("x.v1"));
    }

    #[test]
    fn no_common_version_fails_with_the_same_text_on_both_sides() {
        let a = offer(1, 1, &["control.v1"]);
        let b = offer(2, 2, &["control.v1"]);
        let ab = negotiate(&a, &b).unwrap_err();
        let ba = negotiate(&b, &a).unwrap_err();
        assert_eq!(ab, ba);
        assert_eq!(ab.reason(), "no_common_version");
    }

    #[test]
    fn a_missing_required_feature_fails() {
        let err = negotiate(&offer(1, 1, &["control.v1"]), &offer(1, 1, &[])).unwrap_err();
        assert_eq!(err.reason(), "missing_required_feature");
    }

    #[test]
    fn this_build_negotiates_with_itself() {
        let me = VersionOffer::this_build();
        let n = negotiate(&me, &me).unwrap();
        assert_eq!(n.version, PROTO_MAX);
        assert_eq!(n.features.len(), feature::ALL.len());
    }

    proptest! {
        #[test]
        fn negotiation_is_symmetric(
            a in (0u32..6, 0u32..6), b in (0u32..6, 0u32..6),
            fa in proptest::collection::vec(0usize..4, 0..4),
            fb in proptest::collection::vec(0usize..4, 0..4),
        ) {
            let pick = |ix: &Vec<usize>| ix.iter().map(|i| feature::ALL[*i]).collect::<Vec<_>>();
            let x = offer(a.0, a.1, &pick(&fa));
            let y = offer(b.0, b.1, &pick(&fb));
            prop_assert_eq!(negotiate(&x, &y), negotiate(&y, &x));
        }
    }
}
