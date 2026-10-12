//! The decision step of each session command (PLAN §6.4): checks against
//! the station policy and the session's projection, then the effects, all
//! in the command's transaction. Nothing here touches a provider.

use fleetdeck_proto::command::{
    ApprovalDecision, CommandOrigin, CommandResult, SessionCommand, SessionSpec, WorkspaceRequest,
};
use fleetdeck_proto::event::{
    ProviderKind, RequestKind, ResolvedBy, ResponseChannel, RunOrigin, SessionState,
};
use fleetdeck_proto::{InputId, Operation, Reject, RejectCode, SessionId, SessionSummary};
use fleetdeck_providers::codex::validate_question_answer;

use super::policy::{Policy, WorkspaceError};
use crate::store::{
    AnswerClaim, DecideError, Decision, EffectPayload, InputRecord, NewSession, RequestState,
    RuntimeRequest, WorkspacePlan,
};
use crate::workspace::provision;

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

pub(super) fn reject(code: RejectCode, message: impl Into<String>) -> DecideError {
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
        SessionCommand::Start {
            spec,
            prompt,
            workspace,
        } => start(d, policy, origin_run, spec, prompt, workspace, None),
        SessionCommand::Import {
            spec,
            native_session_id,
            prompt,
        } => {
            fleetdeck_proto::command::check_native_session_id(&native_session_id)
                .map_err(|why| reject(RejectCode::InvalidCommand, why))?;
            start(
                d,
                policy,
                origin_run,
                spec,
                prompt,
                None,
                Some(native_session_id),
            )
        }
        SessionCommand::Resume { session_id } => {
            super::resume::decide_resume(d, policy, &session_id)
        }
        SessionCommand::Fork { .. } => Err(reject(
            RejectCode::Unsupported,
            "fork is not part of this station build yet (DL-037)",
        )),
        SessionCommand::Rename { session_id, title } => rename(d, &session_id, title),
        SessionCommand::SetAutoContinue {
            session_id,
            enabled,
        } => super::auto_continue::decide_set(d, origin, &session_id, enabled),
        SessionCommand::CoordinatorRenew {
            session_id,
            grant_id,
            generation,
        } => {
            super::coordinator_command::renew(d, policy, origin, &session_id, &grant_id, generation)
        }
        SessionCommand::CoordinatorRevoke {
            session_id,
            grant_id,
        } => super::coordinator_command::revoke(d, policy, origin, &session_id, &grant_id),
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
            let request = d
                .request(&request_id)?
                .filter(|r| r.session_id == session_id);
            // The answers to a question travel in `updated_input` (Claude
            // `AskUserQuestion`, Codex `requestUserInput`); that is an
            // answer, not an edit of a tool input (DL-068).
            let question = request
                .as_ref()
                .is_some_and(|r| r.kind == RequestKind::Question);
            let op = if decision.updated_input.is_some() && !question {
                Operation::EditInput
            } else {
                Operation::Answer
            };
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
            // Admission before the claim (M2 gate correction): a first
            // answer to an open question must be one that the provider can
            // take, or it would hold the request's answer slot with an
            // effect that the provider refuses.
            if request.state == RequestState::Open && request.receipt.is_none() {
                admit_answer(d, &session_id, &request, &decision)?;
            }
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
        SessionCommand::Revert {
            session_id,
            turn_id,
        } => super::revert::decide_revert(d, policy, &session_id, &turn_id),
        SessionCommand::ResolveRevert {
            session_id,
            revert_id,
            resolution,
        } => super::revert::decide_resolve(d, policy, &session_id, &revert_id, resolution),
        SessionCommand::Stop { session_id, how } => {
            live_session(d, &session_id, Operation::Stop)?;
            plain_effect(d, &session_id, EffectPayload::Stop { how })
        }
    }
}

/// Checks the payload of a first answer to an open request against the
/// provider's own rules, before the answer is claimed. Only Codex questions
/// have payload rules here: an RPC question needs the answer map that
/// [`validate_question_answer`] accepts, and the free answer (`answer`) of a
/// message question needs a text. Other requests pass unchanged. Missing
/// request metadata fails closed.
fn admit_answer(
    d: &Decision<'_>,
    session_id: &SessionId,
    request: &RuntimeRequest,
    decision: &ApprovalDecision,
) -> Result<(), DecideError> {
    if request.kind != RequestKind::Question
        || !request
            .options
            .iter()
            .any(|o| o.option_id == decision.option_id)
    {
        // An unknown option keeps its own refusal from the claim.
        return Ok(());
    }
    let missing = || {
        reject(
            RejectCode::InvalidCommand,
            "the station has no record of this question's input",
        )
    };
    let summary = d.summary(session_id)?.ok_or_else(missing)?;
    match summary.provider {
        Some(ProviderKind::Claude) => return Ok(()),
        Some(ProviderKind::Codex) => {}
        Some(ProviderKind::Unknown) | None => return Err(missing()),
    }
    let open = summary
        .open_requests
        .iter()
        .find(|r| r.request_id == request.request_id)
        .ok_or_else(missing)?;
    match open.response {
        ResponseChannel::RpcReply => {
            let params = open.input.as_ref().ok_or_else(missing)?;
            validate_question_answer(params, decision.updated_input.as_ref())
                .map(|_| ())
                .map_err(|e| reject(RejectCode::InvalidCommand, e.to_string()))
        }
        ResponseChannel::Message => {
            // The same rule as the Codex core: the free answer needs a
            // non-empty text; an offered option is its own answer.
            if decision.option_id == CODEX_FREE_ANSWER
                && decision.text.as_ref().is_none_or(String::is_empty)
            {
                return Err(reject(
                    RejectCode::InvalidCommand,
                    "the answer needs a message",
                ));
            }
            Ok(())
        }
    }
}

/// The option id of a Codex free answer (`codex/requests.rs`).
const CODEX_FREE_ANSWER: &str = "answer";

/// `session.rename` (DL-095): a station event and no effect. It changes no
/// provider state, so an ended session or a session in a revert can be
/// renamed. An unchanged title is accepted without an event.
fn rename(
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

pub(super) fn result(session_id: &SessionId, input_id: Option<InputId>) -> CommandResult {
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
    workspace: Option<WorkspaceRequest>,
    import: Option<String>,
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
    if let Some(coordinator) = &spec.coordinator {
        super::coordinator_command::check_spec(
            origin,
            provider,
            coordinator,
            import.is_some(),
            policy.coordinator_bindings(),
        )?;
    }
    let canonical_inside = |p: &std::path::Path| policy.workspace(&p.to_string_lossy()).ok();
    let refused = |r: provision::Refused| {
        let code = if r.forbidden {
            RejectCode::Forbidden
        } else {
            RejectCode::InvalidCommand
        };
        reject(code, r.message)
    };
    let (mut cwd, mut plan) = match &workspace {
        Some(WorkspaceRequest::Clone { url }) => {
            let (plan, path) =
                provision::clone_plan(url, &spec.cwd, canonical_inside).map_err(refused)?;
            (path, Some(plan))
        }
        _ => (
            policy.workspace(&spec.cwd).map_err(|e| match e {
                WorkspaceError::Missing(_) => reject(RejectCode::InvalidCommand, e.to_string()),
                WorkspaceError::Outside(_) => reject(RejectCode::Forbidden, e.to_string()),
            })?,
            None,
        ),
    };
    let active = d.active_sessions()?;
    if active >= u64::from(policy.max_sessions()) {
        return Err(reject(
            RejectCode::Forbidden,
            format!(
                "this station already runs its maximum of {} sessions",
                policy.max_sessions()
            ),
        ));
    }
    let session_id: SessionId = d.new_id();
    let callsign = new_callsign(d, &session_id, active)?;
    let mut worktree = None;
    if matches!(workspace, Some(WorkspaceRequest::Worktree)) {
        let (worktree_plan, path) =
            provision::worktree_plan(&cwd, &callsign, |p| canonical_inside(p).is_some())
                .map_err(refused)?;
        worktree = Some(path.to_string_lossy().into_owned());
        plan = Some(worktree_plan);
        cwd = path;
    }
    spec.cwd = cwd.to_string_lossy().into_owned();
    if let Some(other) = d.open_revert_over(&spec.cwd)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a revert of session `{}` holds this worktree",
                other.revert.session_id
            ),
        ));
    }
    if let Some(native) = &import
        && let Some(other) = d.native_session_in_use(native)?
    {
        return Err(reject(
            RejectCode::InvalidState,
            format!("session `{other}` already drives this native session"),
        ));
    }
    let branch = match &plan {
        Some(WorkspacePlan::Worktree { branch, .. }) => Some(branch.clone()),
        Some(WorkspacePlan::Clone { .. }) => None,
        None => fleetdeck_core::git::current_branch(&cwd),
    };
    d.create_session(&NewSession {
        session_id: session_id.clone(),
        provider,
        provider_instance: spec.provider_instance.clone(),
        cwd: spec.cwd.clone(),
        worktree: worktree.clone(),
        branch,
        mode: spec.mode.clone(),
        title: spec.title.clone(),
        offline_approval_policy: policy.offline(),
    })?;
    d.append(
        &session_id,
        fleetdeck_proto::event::Event::SessionNamed { callsign, worktree }.into(),
    )?;
    let prompt = prompt.map(|input| InputRecord {
        input_id: d.new_id(),
        origin,
        input,
    });
    let input_id = prompt.as_ref().map(|p| p.input_id.clone());
    let payload = match import {
        // An import is a native resume of a session that this station
        // never ran (DL-308): the provider continues the conversation.
        Some(native) => {
            d.set_native_session_id(&session_id, &native)?;
            EffectPayload::Resume {
                native_session_id: native,
                spec,
                prompt,
                continue_by: None,
            }
        }
        None => EffectPayload::Start {
            spec,
            prompt,
            workspace: plan,
        },
    };
    d.enqueue(&session_id, payload)?;
    Ok(result(&session_id, input_id))
}

/// The first free callsign of the station for a new session (DL-200).
fn new_callsign(
    d: &Decision<'_>,
    session_id: &SessionId,
    start: u64,
) -> Result<String, DecideError> {
    for candidate in provision::callsign_candidates(session_id.as_str(), start) {
        if !d.callsign_taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(reject(RejectCode::InvalidState, "no free callsign"))
}

/// The session's projection, if it can take `op` now: it exists, has not
/// ended, has a live process, and supports `op` once the provider told its
/// capabilities (`session.started`).
pub(super) fn live_session(
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
    // A revert holds the session from admission through reconciliation
    // (PLAN §6.6): no new input, no answer, no mode change; a stop stays
    // possible.
    if op != Operation::Stop
        && let Some(revert) = s.revert.as_ref().filter(|r| r.blocks_input())
    {
        return Err(reject(
            RejectCode::InvalidState,
            match revert.state {
                fleetdeck_proto::event::RevertState::ReconciliationRequired => {
                    "a revert needs reconciliation: the session takes no input"
                }
                _ => "a revert runs: the session takes no input",
            },
        ));
    }
    // A partial stdin frame left the provider's input in an unknown state
    // (ADR-005 §4.3): no new input; interrupt and stop stay possible.
    if !matches!(op, Operation::Stop | Operation::Interrupt)
        && let Some(reason) = d.shim_input_blocked(session_id)?
    {
        return Err(reject(
            RejectCode::InvalidState,
            format!("the session input is blocked ({reason}): interrupt or stop it"),
        ));
    }
    // A revert of another session in the same worktree holds it too
    // (DL-093): no input and no answer that would start writes there.
    if matches!(
        op,
        Operation::Prompt
            | Operation::Steer
            | Operation::FollowUp
            | Operation::Answer
            | Operation::EditInput
            | Operation::Revert
    ) && let Some(cwd) = s.cwd.as_deref()
        && let Some(other) = d.open_revert_over(cwd)?
        && other.revert.session_id != *session_id
    {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a revert of session `{}` holds this worktree: the session takes no input",
                other.revert.session_id
            ),
        ));
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
