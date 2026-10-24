//! The decision step of each session command (PLAN §6.4): checks against
//! the station policy and the session's projection, then the effects, all
//! in the command's transaction. Nothing here touches a provider.

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, SessionCommand, SessionSpec, WorkspaceRequest,
};
use fleetdeck_proto::event::{ProviderKind, RequestKind, ResolvedBy, RunOrigin, SessionState};
use fleetdeck_proto::session::BatchRef;
use fleetdeck_proto::{InputId, Operation, Reject, RejectCode, SessionId, SessionSummary};

use super::decide_rename::rename;
use super::policy::{Policy, WorkspaceError};
use super::shell_decide::ShellInterrupt;
use crate::coordinator::Bindings;
use crate::store::{
    AnswerClaim, DecideError, Decision, EffectPayload, InputRecord, NewSession, RequestState,
    WorkspacePlan,
};
use crate::workspace::clone_reuse::{self, CloneTarget};
use crate::workspace::provision;
use workspace::fleet_git::move_in_decide::decide_in;

#[path = "decide_workspace.rs"]
mod workspace;

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
    bindings: &Bindings,
    origin: CommandOrigin,
    restarting: bool,
    mut command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    command.clear_station_local();
    let origin_run = run_origin(origin);
    super::fleet_control::check(d, policy, bindings, &command)?;
    match command {
        SessionCommand::Start {
            spec,
            prompt,
            workspace,
            setup_cmd,
            batch,
            issue,
            origin_text,
            fleet_git,
        } => {
            if let Some(cmd) = &setup_cmd {
                workspace::check_setup_cmd(policy, origin_run, cmd)?;
            }
            if let Some(why) = batch.as_deref().and_then(BatchRef::problem) {
                return Err(reject(RejectCode::InvalidCommand, why));
            }
            if let Some(issue) = &issue {
                issue
                    .check()
                    .map_err(|why| reject(RejectCode::InvalidCommand, why))?;
            }
            start(
                d,
                policy,
                bindings,
                origin_run,
                spec,
                prompt,
                workspace.map(|w| *w),
                None,
                setup_cmd,
                StartFacts {
                    issue: issue.map(|i| *i),
                    origin_text,
                    batch: batch.map(|b| *b),
                    fleet_git,
                    ..StartFacts::default()
                },
            )
        }
        SessionCommand::Import {
            spec,
            native_session_id,
            prompt,
            transcript,
        } => {
            fleetdeck_proto::command::check_native_session_id(&native_session_id)
                .map_err(|why| reject(RejectCode::InvalidCommand, why))?;
            start(
                d,
                policy,
                bindings,
                origin_run,
                spec,
                prompt,
                None,
                Some(native_session_id),
                None,
                StartFacts {
                    transcript,
                    ..StartFacts::default()
                },
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
        } => super::coordinator_command::renew(
            d,
            bindings,
            origin,
            &session_id,
            &grant_id,
            generation,
        ),
        SessionCommand::CoordinatorRevoke {
            session_id,
            grant_id,
        } => super::coordinator_command::revoke(d, bindings, origin, &session_id, &grant_id),
        command @ SessionCommand::CoordinatorSet { .. } => {
            super::coordinator_command::set_command(d, bindings, origin, command)
        }
        SessionCommand::Prompt { session_id, input } => {
            refuse_resume_while_restarting(d, restarting, &session_id, origin_run)?;
            if let Some(resumed) = resume_hibernated(d, policy, &session_id, origin_run, &input)? {
                return Ok(resumed);
            }
            let s = live_session(d, &session_id, Operation::Prompt)?;
            input_effect(d, &s, origin_run, input, EffectPayload::prompt)
        }
        SessionCommand::Steer { session_id, input } => {
            refuse_resume_while_restarting(d, restarting, &session_id, origin_run)?;
            if let Some(resumed) = resume_hibernated(d, policy, &session_id, origin_run, &input)? {
                return Ok(resumed);
            }
            let s = live_session(d, &session_id, Operation::Steer)?;
            input_effect(d, &s, origin_run, input, EffectPayload::steer)
        }
        SessionCommand::FollowUp { session_id, input } => {
            refuse_resume_while_restarting(d, restarting, &session_id, origin_run)?;
            if let Some(resumed) = resume_hibernated(d, policy, &session_id, origin_run, &input)? {
                return Ok(resumed);
            }
            let s = live_session(d, &session_id, Operation::FollowUp)?;
            input_effect(d, &s, origin_run, input, EffectPayload::follow_up)
        }
        SessionCommand::Interrupt { session_id } => {
            // A shell command of the session is cancelled (B8).
            let shell = super::shell_decide::interrupt(d, &session_id)?;
            if let ShellInterrupt::Done(done) = shell {
                return Ok(done);
            }
            let s = live_session(d, &session_id, Operation::Interrupt)?;
            // A Codex command sent before its turn started: the provider
            // holds the interrupt (B8).
            if matches!(shell, ShellInterrupt::None)
                && s.current_turn.is_none()
                && s.current_run.is_none()
                && s.state != SessionState::Starting
            {
                return Err(reject(RejectCode::InvalidState, "no turn runs"));
            }
            plain_effect(d, &session_id, EffectPayload::Interrupt { authority: None })
        }
        SessionCommand::Answer {
            session_id,
            request_id,
            mut decision,
        } => {
            let request = d.request(&session_id, &request_id)?;
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
                super::fleet_control::admit_answer(
                    d,
                    &session_id,
                    &request,
                    &mut decision,
                    origin,
                )?;
            }
            let by = resolved_by(origin);
            match d.claim_answer(&session_id, &request.request_id, &decision.option_id, by)? {
                AnswerClaim::First => {
                    let payload = EffectPayload::Answer {
                        request_id,
                        decision,
                        by,
                        authority: None,
                    };
                    plain_effect(d, &session_id, payload)
                }
                // L3: a second answer gets the first answer's outcome and
                // sends nothing (a coordinator: `already_answered`, ADR-008).
                AnswerClaim::Repeated(first) => {
                    super::fleet_control::repeated(&session_id, origin, &first)
                }
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
            plain_effect(
                d,
                &session_id,
                EffectPayload::SetMode {
                    mode,
                    authority: None,
                },
            )
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
            // A start that waits for its setup command or prepares its
            // workspace has no process yet; its stop cancels that work
            // (DL-148).
            if !(crate::store::setup_runs::setup_run_for(d.connection(), &session_id)?
                || crate::store::provisioning::provisioning_for(d.connection(), &session_id)?)
            {
                live_session(d, &session_id, Operation::Stop)?;
            }
            plain_effect(
                d,
                &session_id,
                EffectPayload::Stop {
                    how,
                    authority: None,
                },
            )
        }
        // The session manager decides a land with the shells (FG-8).
        SessionCommand::Land { .. } => Err(super::land::refuse_direct()),
        SessionCommand::MoveOut { .. } => Err(super::land::move_session::refuse_direct()),
        command @ (SessionCommand::MoveFinish { .. } | SessionCommand::MoveUndo { .. }) => {
            super::land::move_session::decide_finish_undo(d, policy, bindings, origin, command)
        }
        command @ SessionCommand::MoveIn { .. } => decide_in(d, policy, bindings, origin, command),
        // The session manager decides a usage recount (B20).
        SessionCommand::UsageRecount { .. } => Err(super::usage_recount::refuse_direct()),
        // The session manager decides these with the resolved root (B8).
        SessionCommand::ShellCommand { .. } | SessionCommand::ReleaseShell { .. } => {
            Err(super::shell_decide::refuse_direct())
        }
        // The session manager runs the process check first (`submit`); a
        // direct call has none and is refused after the row checks (DL-182).
        SessionCommand::ReconcileExecution {
            session_id,
            shim_id,
        } => super::shims::decide_reconcile(d, origin, &session_id, &shim_id, None),
        // The session manager decides this with the stop plans (`submit`).
        SessionCommand::StopExecution { .. } => Err(super::stop_decide::refuse_direct()),
        // The session manager decides these with the shells of the station
        // (`SessionManager::submit`); here no shell list is known (DL-150).
        command @ (SessionCommand::WorktreeTrash { .. }
        | SessionCommand::WorktreeRestore { .. }
        | SessionCommand::WorktreePurge { .. }
        | SessionCommand::WorktreeRelease { .. }) => {
            super::trash::decide(d, policy, origin, None, command)
        }
    }
}

pub(super) fn result(session_id: &SessionId, input_id: Option<InputId>) -> CommandResult {
    CommandResult {
        session_id: Some(session_id.clone()),
        input_id,
        seq: None,
    }
}

/// What a start knows about the text of its first prompt (R3, DL-164).
#[derive(Debug, Default)]
struct StartFacts {
    issue: Option<fleetdeck_proto::integrations::IssueRef>,
    origin_text: bool,
    /// The `Nx` batch of the start (R1.4, DL-170).
    batch: Option<BatchRef>,
    transcript: Option<fleetdeck_proto::native::TranscriptPin>, // B7
    /// The Fleet Git snapshot of the start (FG-1, design §3.3).
    fleet_git: Option<Box<fleetdeck_proto::fleet_git::FleetGitStart>>,
}

/// The modes that a start from third-party text may use, and the mode that
/// it gets when none is chosen (R3.1): the most restrictive of its provider.
///
/// Claude: `default` or `plan`. Codex: the approval policy `untrusted` with
/// any sandbox but full access.
fn restrict_mode(kind: ProviderKind, chosen: Option<&str>) -> Result<String, DecideError> {
    use fleetdeck_providers::{claude, codex};
    let (safest, allowed) = match kind {
        ProviderKind::Claude => (
            claude::SAFEST_MODE,
            chosen.is_none_or(|m| matches!(m, "default" | "plan")),
        ),
        ProviderKind::Codex => (
            codex::SAFEST_MODE,
            chosen.is_none_or(|m| {
                codex::Mode::parse(m).is_some_and(|m| {
                    m.approval_policy == "untrusted" && m.sandbox != codex::FULL_ACCESS_SANDBOX
                })
            }),
        ),
        ProviderKind::Unknown => {
            return Err(reject(
                RejectCode::Forbidden,
                "a start from issue or pull request text needs a known provider",
            ));
        }
    };
    if !allowed {
        return Err(reject(
            RejectCode::Forbidden,
            "a start from issue or pull request text runs in a restrictive mode only",
        ));
    }
    Ok(chosen.unwrap_or(safest).to_owned())
}

#[allow(clippy::too_many_arguments)]
fn start(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    origin: RunOrigin,
    mut spec: SessionSpec,
    prompt: Option<fleetdeck_proto::command::UserInput>,
    workspace: Option<WorkspaceRequest>,
    import: Option<String>,
    setup_cmd: Option<String>,
    facts: StartFacts,
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
    if facts.origin_text {
        spec.mode = Some(restrict_mode(provider, spec.mode.as_deref())?);
    }
    // A worker grant (MCP P4a §6.2) that the station cannot hand off is
    // dropped from the spec; the start goes on without it and records the
    // skip below.
    let admission_skip = super::worker_grant::admit(
        origin,
        provider,
        &mut spec,
        import.is_some(),
        bindings,
        d.command_id(),
    )?;
    // Before the existence check: a folder in the trash is missing at its
    // original place, and the owner needs the reason (DL-150).
    super::trash::custody(d, &spec.cwd)?;
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
            let (path, plan) = match clone_reuse::clone_or_reuse(url, &spec.cwd, canonical_inside)
                .map_err(refused)?
            {
                CloneTarget::Clone(plan, path) => (path, Some(plan)),
                CloneTarget::Reuse(path) => (path, None),
            };
            // A clone that another start still runs owns its directory.
            let key = path.to_string_lossy();
            if crate::store::provisioning::clone_pending_for(d.connection(), &key)? {
                return Err(refused(clone_reuse::being_cloned(&path)));
            }
            (path, plan)
        }
        _ => (
            policy.workspace(&spec.cwd).map_err(|e| match e {
                WorkspaceError::Missing(_) => reject(RejectCode::InvalidCommand, e.to_string()),
                WorkspaceError::Outside(_) => reject(
                    RejectCode::OutsideRoots,
                    super::policy_limits::outside_roots_text(&spec.cwd, policy.name()),
                ),
            })?,
            None,
        ),
    };
    // Fleet Git (FG-1): the snapshot is checked against the station before
    // anything is made (design §4.2 step 1). A clone start takes none.
    let home = match facts.fleet_git.as_deref() {
        Some(_) if matches!(workspace, Some(WorkspaceRequest::Clone { .. })) => {
            return Err(reject(
                RejectCode::InvalidCommand,
                "invalid_params: a clone start cannot carry Fleet Git settings",
            ));
        }
        Some(fg) => Some(workspace::fleet_git::check(
            d,
            policy,
            fg,
            &cwd,
            workspace.as_ref(),
        )?),
        None => None,
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
    // The branch that the session works on: the chosen branch of a
    // worktree, else the current branch of a start in place (DL-144).
    let ticket = match &workspace {
        Some(WorkspaceRequest::Worktree { branch, base }) => {
            provision::check_branch_choice(branch.as_deref(), base.as_deref()).map_err(refused)?;
            branch.as_deref().and_then(provision::ticket_key)
        }
        Some(WorkspaceRequest::Clone { .. }) => None,
        None => fleetdeck_core::git::current_branch(&cwd)
            .as_deref()
            .and_then(provision::ticket_key),
    };
    let callsign = workspace::new_callsign(d, &session_id, active, ticket.as_deref())?;
    let mut worktree = None;
    if let Some((worktree_plan, path)) = workspace::worktree_block(
        &cwd,
        &callsign,
        workspace.as_ref(),
        facts.fleet_git.as_deref(),
        |p| canonical_inside(p).is_some(),
    )
    .map_err(refused)?
    {
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
    super::trash::custody(d, &spec.cwd)?;
    if let Some(native) = &import {
        super::resume::import_exclusion(d, policy, provider, native)?;
    }
    if let Some(native) = &import
        && let Some(other) = d.native_session_held(native)?
    {
        return Err(reject(
            RejectCode::InvalidState,
            format!("session `{other}` holds this native session in a terminal"),
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
    // MCP P4b §9: an owner grant of a live deck gets its record at once.
    let record_seq =
        super::coordinator_command::set::record_start(d, bindings, &session_id, &spec)?;
    d.append(
        &session_id,
        fleetdeck_proto::event::Event::SessionNamed { callsign, worktree }.into(),
    )?;
    if let Some(event) = admission_skip.as_ref().and_then(|s| s.event()) {
        d.append(&session_id, event)?;
    }
    if let (Some(fg), Some(home)) = (facts.fleet_git.as_deref(), home.as_deref()) {
        let owns = workspace::fleet_git::owns_branch(workspace.as_ref());
        workspace::fleet_git::record(d, &session_id, fg, plan.as_ref(), home, owns)?;
    }
    // The batch of an `Nx` start (R1.4, DL-170): the station records it with
    // the name, in the same decision, so a redelivered start adds nothing.
    if let Some(batch) = facts.batch {
        d.append(
            &session_id,
            fleetdeck_proto::event::Event::SessionBatch { batch }.into(),
        )?;
    }
    if let Some(issue) = facts.issue {
        d.append(
            &session_id,
            fleetdeck_proto::event::Event::SessionIssue { issue }.into(),
        )?;
    }
    let prompt = prompt.map(|input| InputRecord {
        input_id: d.new_id(),
        origin,
        input,
        shell_context: Vec::new(),
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
                authority: None,
                import: true,
                transcript: facts.transcript,
            }
        }
        None => EffectPayload::Start {
            spec,
            prompt,
            workspace: plan,
            setup_cmd,
            authority: None,
            fleet_git: facts.fleet_git,
        },
    };
    d.enqueue(&session_id, payload)?;
    let mut done = result(&session_id, input_id);
    done.seq = record_seq;
    Ok(done)
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

/// True when an input from `origin` resumes the session: it is exited and
/// hibernated, and no rule keeps the input from resuming it.
pub(super) fn resumes_hibernated(
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
fn refuse_resume_while_restarting(
    d: &mut Decision<'_>,
    restarting: bool,
    session_id: &SessionId,
    origin: RunOrigin,
) -> Result<(), DecideError> {
    // (FG-8) A land holds its session: no input. (FG-9) The first input of a moved session.
    super::land::move_in::check_input(d, session_id)?;
    if restarting && resumes_hibernated(d, session_id, origin)? {
        return Err(reject(
            RejectCode::InvalidState,
            super::restart_gate::RESTARTING,
        ));
    }
    Ok(())
}

/// An input for a hibernated session resumes it with that input as the first
/// input of the new process. `None`: the session is not hibernated (or does
/// not exist), and the normal input checks apply.
fn resume_hibernated(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    origin: RunOrigin,
    input: &fleetdeck_proto::command::UserInput,
) -> Result<Option<CommandResult>, DecideError> {
    if !resumes_hibernated(d, session_id, origin)? {
        return Ok(None);
    }
    let record = super::shell_context::record(d, session_id, origin, input.clone())?;
    super::resume::admit_resume(d, policy, session_id, Some(record), None).map(Some)
}

fn input_effect(
    d: &mut Decision<'_>,
    s: &SessionSummary,
    origin: RunOrigin,
    input: fleetdeck_proto::command::UserInput,
    payload: impl FnOnce(InputRecord) -> EffectPayload,
) -> Result<CommandResult, DecideError> {
    let record = super::shell_context::record(d, &s.session_id, origin, input)?;
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
