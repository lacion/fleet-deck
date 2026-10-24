//! An input for a hibernated session (NV-27): it resumes the session, and
//! the restart freeze refuses it like a resume (split from `decide.rs` for
//! the module size rule).

use super::*;

/// True when an input from `origin` resumes the session: it is exited and
/// hibernated, and no rule keeps the input from resuming it.
fn resumes_hibernated(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    origin: RunOrigin,
) -> Result<bool, DecideError> {
    // ADR-008 §5.2: no implicit continuation by a coordinator.
    if origin == RunOrigin::Coordinator || d.authority().is_some() {
        return Ok(false);
    }
    let Some(summary) = d.summary(session_id)? else {
        return Ok(false);
    };
    Ok(summary.state == SessionState::Exited && summary.hibernated)
}

/// An input that would resume a hibernated session starts a process, so
/// the restart freeze refuses it like a resume (NV-27).
pub(super) fn refuse_resume_while_restarting(
    d: &mut Decision<'_>,
    restarting: bool,
    session_id: &SessionId,
    origin: RunOrigin,
) -> Result<(), DecideError> {
    // (FG-8) A land holds its session: no input. (FG-9) The first input of a moved session.
    super::super::land::move_in::check_input(d, session_id)?;
    if restarting && resumes_hibernated(d, session_id, origin)? {
        return Err(reject(
            RejectCode::InvalidState,
            super::super::restart_gate::RESTARTING,
        ));
    }
    Ok(())
}

/// An input for a hibernated session resumes it with that input as the first
/// input of the new process. `None`: the session is not hibernated (or does
/// not exist), and the normal input checks apply.
pub(super) fn resume_hibernated(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    origin: RunOrigin,
    input: &fleetdeck_proto::command::UserInput,
) -> Result<Option<CommandResult>, DecideError> {
    if !resumes_hibernated(d, session_id, origin)? {
        return Ok(None);
    }
    let record = super::super::shell_context::record(d, session_id, origin, input.clone())?;
    super::super::resume::admit_resume(d, policy, session_id, Some(record), None).map(Some)
}
