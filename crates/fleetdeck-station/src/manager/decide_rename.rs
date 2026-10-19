//! `session.rename` (moved out of `decide.rs` for the module size rule; no
//! behavior change).

use fleetdeck_proto::command::CommandResult;
use fleetdeck_proto::{RejectCode, SessionId};

use super::decide::{reject, result};
use crate::store::{DecideError, Decision};

/// `session.rename` (DL-095): a station event and no effect. It changes no
/// provider state, so an ended session or a session in a revert can be
/// renamed. An unchanged title is accepted without an event.
pub(super) fn rename(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    title: Option<String>,
) -> Result<CommandResult, DecideError> {
    let summary = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let title = fleetdeck_proto::command::normalize_title(title.as_deref())
        .map_err(|why| reject(RejectCode::InvalidCommand, why))?;
    if summary.title != title {
        d.append(
            session_id,
            fleetdeck_proto::event::Event::SessionRenamed { title }.into(),
        )?;
    }
    Ok(result(session_id, None))
}
