//! The setup command check and the callsign pick of a start (moved from
//! `decide.rs` for the module size rule, T1.0).

use std::path::{Path, PathBuf};

use fleetdeck_proto::command::WorkspaceRequest;
use fleetdeck_proto::event::RunOrigin;
use fleetdeck_proto::fleet_git::FleetGitStart;
use fleetdeck_proto::{RejectCode, SessionId};

use super::reject;
use crate::manager::policy::Policy;
use crate::store::{DecideError, Decision, WorkspacePlan};
use crate::workspace::provision;

#[path = "decide_fleet_git.rs"]
pub(in crate::manager) mod fleet_git;

/// The longest setup command, in characters (DL-148).
const MAX_SETUP_CMD_CHARS: usize = 4096;

/// The refusals of a setup command, in this order: the station policy, a
/// coordinator origin, then the text (DL-148).
pub(super) fn check_setup_cmd(
    policy: &Policy,
    origin: RunOrigin,
    cmd: &str,
) -> Result<(), DecideError> {
    if !policy.allow_setup_cmd() {
        return Err(reject(
            RejectCode::Forbidden,
            "setup commands are off on this station (allow_setup_cmd = false)",
        ));
    }
    if origin == RunOrigin::Coordinator {
        return Err(reject(
            RejectCode::Forbidden,
            "a coordinator start cannot carry a setup command",
        ));
    }
    if cmd.trim().is_empty() || cmd.chars().count() > MAX_SETUP_CMD_CHARS || cmd.contains('\0') {
        return Err(reject(
            RejectCode::InvalidCommand,
            "the setup command must be 1 to 4096 characters without NUL",
        ));
    }
    Ok(())
}

/// The first free callsign of the station for a new session (DL-200).
pub(super) fn new_callsign(
    d: &Decision<'_>,
    session_id: &SessionId,
    start: u64,
    ticket: Option<&str>,
) -> Result<String, DecideError> {
    for candidate in provision::callsign_candidates(session_id.as_str(), start, ticket) {
        if !d.callsign_taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(reject(RejectCode::InvalidState, "no free callsign"))
}

/// The worktree plan of a worktree start, and the folder of the new worktree
/// (moved from `decide.rs`, T1.3). A synced start (`fleet_git`, FG-1) gets the
/// branch `fd/<slug>/<callsign>` and the fetch of the plan; `None` when the
/// request is not a worktree.
pub(super) fn worktree_block(
    repo: &Path,
    callsign: &str,
    request: Option<&WorkspaceRequest>,
    fleet: Option<&FleetGitStart>,
    inside: impl Fn(&Path) -> bool,
) -> Result<Option<(WorkspacePlan, PathBuf)>, provision::Refused> {
    let Some(WorkspaceRequest::Worktree { branch, base }) = request else {
        return Ok(None);
    };
    let (plan, path) =
        provision::worktree_plan(repo, callsign, branch.as_deref(), base.as_deref(), inside)?;
    let plan = match fleet {
        Some(fg) => fleet_git::sync_plan(fg, callsign, plan, branch.is_some()),
        None => plan,
    };
    Ok(Some((plan, path)))
}
