//! The effort part of the decisions (B18 Phase 2, design
//! `docs/v2/design/effort.md` §9.2 item 2; moved out of `decide.rs` for the
//! module size rule).

use fleetdeck_proto::command::CommandResult;
use fleetdeck_proto::effort::valid_effort;
use fleetdeck_proto::event::Event;
use fleetdeck_proto::{Operation, RejectCode, SessionId};

use super::{live_session, plain_effect, reject};
use crate::store::{DecideError, Decision, EffectPayload};

/// The text of a refused level.
const INVALID: &str = "use 1-16 of a-z, 0-9, `-`, `_`";

/// `valid_effort` of the effort of a start or an import spec.
pub(super) fn check(effort: Option<&str>) -> Result<(), DecideError> {
    match effort {
        Some(e) if !valid_effort(e) => Err(reject(RejectCode::InvalidCommand, INVALID)),
        _ => Ok(()),
    }
}

/// The owner's choice of a new session: `effort.chosen` in the start
/// decision. A redelivered start adds nothing (it is not decided again).
pub(super) fn record(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    effort: Option<&str>,
) -> Result<(), DecideError> {
    if let Some(effort) = effort {
        d.append(
            session_id,
            Event::EffortChosen {
                effort: effort.to_owned(),
            }
            .into(),
        )?;
    }
    Ok(())
}

/// `session.set_effort`: the effect settles on the provider's
/// `effort.set_result` with its effect id.
pub(super) fn set(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    effort: String,
) -> Result<CommandResult, DecideError> {
    check(Some(&effort))?;
    live_session(d, session_id, Operation::SetEffort)?;
    plain_effect(d, session_id, EffectPayload::SetEffort { effort })
}
