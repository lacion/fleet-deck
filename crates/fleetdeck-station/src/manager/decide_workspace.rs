//! The setup command check and the callsign pick of a start (moved from
//! `decide.rs` for the module size rule, T1.0).

use fleetdeck_proto::event::RunOrigin;
use fleetdeck_proto::{RejectCode, SessionId};

use super::reject;
use crate::manager::policy::Policy;
use crate::store::{DecideError, Decision};
use crate::workspace::provision;

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
