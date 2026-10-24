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
//!   row lost without termination evidence (ADR-005 §4.6, §6), and no other
//!   session or launch holds the same native conversation (DL-182 §3);
//! - on the shim host the platform can resume there (Linux, DL-182);
//! - no revert of the session waits for reconciliation and no revert holds
//!   the worktree;
//! - the workspace is still inside the allowed roots and the station has a
//!   free session slot;
//! - no terminal holds the session or its native session (DL-140).
//!
//! The new process gets the stored mode and model, never a more permissive
//! mode (L8): the provider maps an unknown mode to its most restrictive one.

use fleetdeck_proto::command::{CommandResult, SessionSpec};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{
    Event, Operation, ProviderInstanceId, Reject, RejectCode, SessionId, SessionSummary,
};

use super::decide::{reject, result};
use super::policy::{Policy, WorkspaceError};
use crate::store::resumes::Purpose;
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

/// The facts of a session that a new process may continue (`session.resume`
/// and `shell.handover`, DL-140).
#[derive(Debug, Clone)]
pub(crate) struct Resumable {
    pub(crate) summary: SessionSummary,
    /// The native resume identity (Claude `session_id`, Codex `thread_id`).
    pub(crate) native: String,
    pub(crate) instance: ProviderInstanceId,
    /// The canonical workspace of the session.
    pub(crate) cwd: String,
}

/// The checks that every continuation of an ended session shares: the
/// session exists and has ended with no live process, its provider instance
/// is configured, it can resume and has a native id, no earlier execution
/// may still run, no revert blocks it, and its workspace is inside the
/// roots and free of reverts.
///
/// # Errors
/// A stored rejection, or a database error.
pub(crate) fn resumable(
    d: &Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
) -> Result<Resumable, DecideError> {
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
    if let Some(why) = policy.shim_host_refusal() {
        return Err(reject(RejectCode::Unsupported, why));
    }
    if let Some(cwd) = &s.cwd {
        super::trash::custody(d, cwd)?;
    }
    let instance = s.provider_instance.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session never started a provider",
        )
    })?;
    let Some(kind) = policy.instance(&instance) else {
        return Err(reject(
            RejectCode::UnknownProvider,
            format!("`{instance}` is not a provider instance of this station"),
        ));
    };
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
    // One native conversation, one execution: across every session and
    // provider instance of the station (DL-182 §3).
    if let Some(why) = d.unended_native(kind, &native, Some(session_id), Purpose::Resume)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!("not_resumable: another execution of this conversation may still run ({why})"),
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
    // Before the existence check: a folder in the trash is missing at its
    // original place, and the owner needs the reason (DL-150).
    super::trash::custody(d, &stored_cwd)?;
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
    super::trash::custody(d, &cwd)?;
    Ok(Resumable {
        summary: s,
        native,
        instance,
        cwd,
    })
}

/// The shim-host rules of an import (DL-182): the platform can resume
/// there, and no other session or launch holds the native conversation.
///
/// # Errors
/// A stored rejection, or a database error.
pub(crate) fn import_exclusion(
    d: &Decision<'_>,
    policy: &Policy,
    kind: fleetdeck_proto::event::ProviderKind,
    native: &str,
) -> Result<(), DecideError> {
    if let Some(why) = policy.shim_host_refusal() {
        return Err(reject(RejectCode::Unsupported, why));
    }
    if let Some(other) = d.native_session_in_use(native)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!("session `{other}` already drives this native session"),
        ));
    }
    if let Some(why) = d.unended_native(kind, native, None, Purpose::Import)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!("not_resumable: another execution of this conversation may still run ({why})"),
        ));
    }
    Ok(())
}

/// The terminal exclusion of a resume and of a handover (DL-140): the
/// session itself is not held by a terminal, and no other session holds the
/// same native session.
///
/// # Errors
/// A stored rejection, or a database error.
pub(crate) fn not_held(d: &Decision<'_>, r: &Resumable) -> Result<(), DecideError> {
    // (FG-8) A land holds its session.
    crate::store::session_holds::check_free(d.connection(), &r.summary.session_id)?;
    if let Some(hold) = &r.summary.terminal {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a terminal holds this session (shell `{}`); return it to board control first",
                hold.shell_id
            ),
        ));
    }
    if let Some(other) = d.native_session_held(&r.native)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!("session `{other}` holds this native session in a terminal"),
        ));
    }
    Ok(())
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
    let r = resumable(d, policy, session_id)?;
    not_held(d, &r)?;
    if d.active_sessions()? >= u64::from(policy.max_sessions()) {
        return Err(reject(
            RejectCode::Forbidden,
            format!(
                "this station already runs its maximum of {} sessions",
                policy.max_sessions()
            ),
        ));
    }
    // The owner's choice, never the observed value (design §9.2 item 5).
    let spec = SessionSpec {
        effort: r.summary.effort_choice.clone(),
        provider_instance: r.instance,
        cwd: r.cwd,
        mode: r.summary.mode.clone(),
        model: r.summary.model.clone(),
        title: r.summary.title.clone(),
        coordinator: None,
        launch: None,
        extra_env: None,
    };
    let input_id = prompt.as_ref().map(|p| p.input_id.clone());
    d.enqueue(
        session_id,
        EffectPayload::Resume {
            native_session_id: r.native,
            spec,
            prompt,
            continue_by,
            authority: None,
            import: false,
            transcript: None,
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
