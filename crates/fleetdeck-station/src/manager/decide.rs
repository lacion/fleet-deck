//! The decision step of each session command (PLAN §6.4): checks against
//! the station policy and the session's projection, then the effects, all
//! in the command's transaction. Nothing here touches a provider.

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand, SessionSpec};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin, SessionState};
use fleetdeck_proto::{InputId, Operation, Reject, RejectCode, SessionId, SessionSummary};

use super::policy::{Policy, WorkspaceError};
use crate::store::{
    AnswerClaim, DecideError, Decision, EffectPayload, InputRecord, NewSession, RequestState,
};

/// The run origin of an input from `origin` (PLAN §6.3).
#[must_use]
pub fn run_origin(origin: CommandOrigin) -> RunOrigin {
    match origin {
        CommandOrigin::Board => RunOrigin::Board,
        CommandOrigin::Coordinator => RunOrigin::Coordinator,
        CommandOrigin::Mail => RunOrigin::Mail,
        CommandOrigin::Local => RunOrigin::Local,
    }
}

/// Who answered an approval, from the command's origin.
#[must_use]
pub fn resolved_by(origin: CommandOrigin) -> ResolvedBy {
    match origin {
        CommandOrigin::Board | CommandOrigin::Mail => ResolvedBy::Board,
        CommandOrigin::Coordinator => ResolvedBy::Coordinator,
        CommandOrigin::Local => ResolvedBy::Local,
    }
}

fn reject(code: RejectCode, message: impl Into<String>) -> DecideError {
    DecideError::Reject(Reject::new(code, message))
}

/// Decides one command. A rejection is stored as the command's receipt.
///
/// # Errors
/// [`DecideError::Reject`] for a refused command, [`DecideError::Store`] for
/// a database failure (nothing is stored).
pub fn decide(
    d: &mut Decision<'_>,
    policy: &Policy,
    origin: CommandOrigin,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    let origin_run = run_origin(origin);
    match command {
        SessionCommand::Start { spec, prompt } => start(d, policy, origin_run, spec, prompt),
        SessionCommand::Resume { .. } | SessionCommand::Fork { .. } => Err(reject(
            RejectCode::Unsupported,
            "resume and fork are not part of this station build yet (DL-037)",
        )),
        SessionCommand::Prompt { session_id, input } => {
            let s = live_session(d, &session_id, Operation::Prompt)?;
            input_effect(d, &s, origin_run, input, |input| EffectPayload::Prompt {
                input,
            })
        }
        SessionCommand::Steer { session_id, input } => {
            let s = live_session(d, &session_id, Operation::Steer)?;
            input_effect(d, &s, origin_run, input, |input| EffectPayload::Steer {
                input,
            })
        }
        SessionCommand::FollowUp { session_id, input } => {
            let s = live_session(d, &session_id, Operation::FollowUp)?;
            input_effect(d, &s, origin_run, input, |input| EffectPayload::FollowUp {
                input,
            })
        }
        SessionCommand::Interrupt { session_id } => {
            let s = live_session(d, &session_id, Operation::Interrupt)?;
            if s.current_turn.is_none()
                && s.current_run.is_none()
                && s.state != SessionState::Starting
            {
                return Err(reject(RejectCode::InvalidState, "no turn runs"));
            }
            plain_effect(d, &session_id, EffectPayload::Interrupt)
        }
        SessionCommand::Answer {
            session_id,
            request_id,
            decision,
        } => {
            let op = if decision.updated_input.is_some() {
                Operation::EditInput
            } else {
                Operation::Answer
            };
            let request = d
                .request(&request_id)?
                .filter(|r| r.session_id == session_id);
            // A closed request answers for itself, also after its session
            // ended: a repeated answer gets the first outcome (L3), a stale
            // one `request_closed`. Only an open request needs a live session.
            if request
                .as_ref()
                .is_none_or(|r| r.state == RequestState::Open)
            {
                live_session(d, &session_id, op)?;
            }
            let request =
                request.ok_or_else(|| reject(RejectCode::UnknownRequest, "no such request"))?;
            let by = resolved_by(origin);
            match d.claim_answer(&request.request_id, &decision.option_id, by)? {
                AnswerClaim::First => {
                    let payload = EffectPayload::Answer {
                        request_id,
                        decision,
                        by,
                    };
                    plain_effect(d, &session_id, payload)
                }
                // L3: a second answer gets the first answer's outcome and
                // sends nothing.
                AnswerClaim::Repeated(_) => Ok(result(&session_id, None)),
                AnswerClaim::Closed(state) => Err(reject(
                    RejectCode::RequestClosed,
                    match state {
                        RequestState::Expired => "the request expired",
                        _ => "the request is closed",
                    },
                )),
                AnswerClaim::UnknownRequest => {
                    Err(reject(RejectCode::UnknownRequest, "no such request"))
                }
                AnswerClaim::UnknownOption => Err(reject(
                    RejectCode::InvalidCommand,
                    format!("`{}` is not an option of this request", decision.option_id),
                )),
            }
        }
        SessionCommand::SetMode { session_id, mode } => {
            let s = live_session(d, &session_id, Operation::SetMode)?;
            if let Some(caps) = &s.capabilities
                && !caps.modes.iter().any(|m| m == &mode)
            {
                return Err(reject(
                    RejectCode::InvalidCommand,
                    format!("`{mode}` is not a mode of this session"),
                ));
            }
            plain_effect(d, &session_id, EffectPayload::SetMode { mode })
        }
        SessionCommand::SetModel { session_id, model } => {
            live_session(d, &session_id, Operation::SetModel)?;
            plain_effect(d, &session_id, EffectPayload::SetModel { model })
        }
        SessionCommand::Stop { session_id, how } => {
            live_session(d, &session_id, Operation::Stop)?;
            plain_effect(d, &session_id, EffectPayload::Stop { how })
        }
    }
}

fn result(session_id: &SessionId, input_id: Option<InputId>) -> CommandResult {
    CommandResult {
        session_id: Some(session_id.clone()),
        input_id,
    }
}

fn start(
    d: &mut Decision<'_>,
    policy: &Policy,
    origin: RunOrigin,
    mut spec: SessionSpec,
    prompt: Option<fleetdeck_proto::command::UserInput>,
) -> Result<CommandResult, DecideError> {
    let provider = policy.instance(&spec.provider_instance).ok_or_else(|| {
        reject(
            RejectCode::UnknownProvider,
            format!(
                "`{}` is not a provider instance of this station",
                spec.provider_instance
            ),
        )
    })?;
    let cwd = policy.workspace(&spec.cwd).map_err(|e| match e {
        WorkspaceError::Missing(_) => reject(RejectCode::InvalidCommand, e.to_string()),
        WorkspaceError::Outside(_) => reject(RejectCode::Forbidden, e.to_string()),
    })?;
    if d.active_sessions()? >= u64::from(policy.max_sessions()) {
        return Err(reject(
            RejectCode::Forbidden,
            format!(
                "this station already runs its maximum of {} sessions",
                policy.max_sessions()
            ),
        ));
    }
    spec.cwd = cwd.to_string_lossy().into_owned();
    let session_id: SessionId = d.new_id();
    d.create_session(&NewSession {
        session_id: session_id.clone(),
        provider,
        provider_instance: spec.provider_instance.clone(),
        cwd: spec.cwd.clone(),
        worktree: None,
        mode: spec.mode.clone(),
        title: spec.title.clone(),
        offline_approval_policy: policy.offline(),
    })?;
    let prompt = prompt.map(|input| InputRecord {
        input_id: d.new_id(),
        origin,
        input,
    });
    let input_id = prompt.as_ref().map(|p| p.input_id.clone());
    d.enqueue(&session_id, EffectPayload::Start { spec, prompt })?;
    Ok(result(&session_id, input_id))
}

/// The session's projection, if it can take `op` now: it exists, has not
/// ended, has a live process, and supports `op` once the provider told its
/// capabilities (`session.started`).
fn live_session(
    d: &Decision<'_>,
    session_id: &SessionId,
    op: Operation,
) -> Result<SessionSummary, DecideError> {
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if matches!(s.state, SessionState::Exited | SessionState::Lost) {
        return Err(reject(RejectCode::InvalidState, "the session has ended"));
    }
    if let Some(caps) = &s.capabilities {
        caps.check(op).map_err(Reject::from)?;
    }
    if d.live_process(session_id)?.is_none() {
        return Err(reject(
            RejectCode::InvalidState,
            "the session has no running provider process yet",
        ));
    }
    Ok(s)
}

fn input_effect(
    d: &mut Decision<'_>,
    s: &SessionSummary,
    origin: RunOrigin,
    input: fleetdeck_proto::command::UserInput,
    payload: impl FnOnce(InputRecord) -> EffectPayload,
) -> Result<CommandResult, DecideError> {
    let record = InputRecord {
        input_id: d.new_id(),
        origin,
        input,
    };
    let input_id = record.input_id.clone();
    d.enqueue(&s.session_id, payload(record))?;
    Ok(result(&s.session_id, Some(input_id)))
}

fn plain_effect(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    payload: EffectPayload,
) -> Result<CommandResult, DecideError> {
    d.enqueue(session_id, payload)?;
    Ok(result(session_id, None))
}
