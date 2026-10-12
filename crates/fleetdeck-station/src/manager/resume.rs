//! `session.resume`: an owner-requested native resume of an ended session
//! (A10, DL-118).
//!
//! A resume starts a new provider process that continues the session's
//! native conversation (Claude `--resume <session_id>`, Codex
//! `thread/resume`). It is not a reattachment: the board shows the new
//! `session.started { resumed: Some(true) }`. The decision transaction
//! admits it only when every fact below holds, and otherwise stores the
//! ordinary rejection receipt (nothing is spawned):
//!
//! - the session exists and has ended (`exited` or `lost`) with no live
//!   process;
//! - its provider instance is still configured, its provider reported the
//!   `resume` capability, and it reported a native resume identity;
//! - every earlier execution is proven ended: no open shim row, and no shim
//!   row lost without termination evidence (ADR-005 §4.6, §6);
//! - no revert of the session waits for reconciliation and no revert holds
//!   the worktree;
//! - the workspace is still inside the allowed roots and the station has a
//!   free session slot.
//!
//! The new process gets the stored mode and model, never a more permissive
//! mode (L8): the provider maps an unknown mode to its most restrictive one.

use fleetdeck_proto::command::{CommandResult, SessionSpec};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, Operation, Reject, RejectCode, SessionId};

use super::decide::{reject, result};
use super::policy::{Policy, WorkspaceError};
use crate::store::{DecideError, Decision, EffectPayload, InputRecord};

/// Decides `session.resume` for `session_id` (see the module documentation).
///
/// # Errors
/// A stored rejection, or a database error.
pub fn decide_resume(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
) -> Result<CommandResult, DecideError> {
    admit_resume(d, policy, session_id, None, None)
}

/// [`decide_resume`] with the first input of the new process and its
/// dispatch deadline: an auto-continue intent (ADR-007 §2.3, DL-127).
///
/// # Errors
/// A stored rejection, or a database error.
pub fn admit_resume(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    prompt: Option<InputRecord>,
    continue_by: Option<i64>,
) -> Result<CommandResult, DecideError> {
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if !matches!(s.state, SessionState::Exited | SessionState::Lost)
        || d.live_process(session_id)?.is_some()
    {
        return Err(reject(
            RejectCode::InvalidState,
            "only an ended session can be resumed",
        ));
    }
    let instance = s.provider_instance.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session never started a provider",
        )
    })?;
    if policy.instance(&instance).is_none() {
        return Err(reject(
            RejectCode::UnknownProvider,
            format!("`{instance}` is not a provider instance of this station"),
        ));
    }
    let caps = s.capabilities.as_ref().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the provider never reported its capabilities",
        )
    })?;
    caps.check(Operation::Resume).map_err(Reject::from)?;
    let native = d.native_session_id(session_id)?.ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the provider never reported a native session id to resume",
        )
    })?;
    if let Some(why) = d.unended_execution(session_id)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!("not_resumable: the previous execution may still run ({why})"),
        ));
    }
    if s.revert.as_ref().is_some_and(|r| r.blocks_input()) {
        return Err(reject(
            RejectCode::InvalidState,
            "a revert of this session needs reconciliation first",
        ));
    }
    let stored_cwd = s
        .cwd
        .clone()
        .ok_or_else(|| reject(RejectCode::InvalidState, "the session has no workspace"))?;
    let cwd = policy.workspace(&stored_cwd).map_err(|e| match e {
        WorkspaceError::Missing(_) => reject(RejectCode::InvalidState, e.to_string()),
        WorkspaceError::Outside(_) => reject(RejectCode::Forbidden, e.to_string()),
    })?;
    let cwd = cwd.to_string_lossy().into_owned();
    if let Some(other) = d.open_revert_over(&cwd)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a revert of session `{}` holds this worktree",
                other.revert.session_id
            ),
        ));
    }
    if d.active_sessions()? >= u64::from(policy.max_sessions()) {
        return Err(reject(
            RejectCode::Forbidden,
            format!(
                "this station already runs its maximum of {} sessions",
                policy.max_sessions()
            ),
        ));
    }
    let spec = SessionSpec {
        provider_instance: instance,
        cwd,
        mode: s.mode.clone(),
        model: s.model.clone(),
        title: s.title.clone(),
        coordinator: None,
    };
    let input_id = prompt.as_ref().map(|p| p.input_id.clone());
    d.enqueue(
        session_id,
        EffectPayload::Resume {
            native_session_id: native,
            spec,
            prompt,
            continue_by,
        },
    )?;
    // The session is starting again from this commit: it counts against
    // `max_sessions`, and inputs are admitted once the new process is
    // live (a Claude CLI reports `session.started` only after its first
    // input). A resume that gets no process ends it (`start_failed`).
    d.append(
        session_id,
        Event::SessionStateChanged {
            state: SessionState::Starting,
            detail: Some("resume requested".to_owned()),
        }
        .into(),
    )?;
    Ok(result(session_id, input_id))
}
