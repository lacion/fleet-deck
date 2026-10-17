//! The decisions of the `!` shell commands (B8, DL-196, design
//! `bang-commands.md`): `session.shell_command`, `session.release_shell`,
//! the root-busy refusal of the other commands, the interrupt of a shell and
//! the revert barrier. Nothing here runs a command.
//!
//! The caller gates ([`guard`]) run in `SessionManager::submit` before the
//! decision, because the decision does not see the caller. The decision runs
//! with the worktree root that `submit` resolved before the transaction
//! (D10 Q1): the canonical Git root of the session folder, else the
//! canonical folder itself.

use std::path::Path;

use fleetdeck_proto::command::{CommandOrigin, CommandResult, RevertResolution, SessionCommand};
use fleetdeck_proto::event::{Event, RevertState, SessionState, ToolStatus};
use fleetdeck_proto::{RejectCode, SessionId};

use super::decide::{reject, result};
use super::policy::Policy;
use crate::store::shell_runs::{Barrier, ShellState};
use crate::store::{Caller, DecideError, Decision, EffectPayload, Store};
use crate::workspace::{GitOptions, Worktree};

/// The refusal of a command while a shell command runs in the folder.
pub const BUSY: &str = "A shell command is running in this folder.";
/// The refusal of a revert across a shell command.
pub const RAN_AFTER_TURN: &str =
    "A shell command ran in this folder after this turn; the files cannot be reverted across it.";
const BOARD_ONLY: &str = "Shell commands come only from the board.";
const NO_COORDINATOR: &str = "A coordinator cannot run shell commands.";
const OFF: &str = "Shells are off on this station (allow_shell = false).";
const BAD_TEXT: &str = "A shell command has 1 to 4096 characters and no NUL.";
const RESUME_FIRST: &str = "Resume the session first.";
const NOT_IDLE: &str = "Wait for the current turn or command to end.";
const STOPPING: &str = "Fleet Deck is stopping.";
const SETTLED: &str = "This command has already settled.";
const NO_SUCH_COMMAND: &str = "No shell command with this id in this session.";
const NO_FOLDER: &str = "The session has no folder.";

/// The longest shell command, in characters.
pub const MAX_COMMAND_CHARS: usize = 4096;

/// The caller gates of `ShellCommand` and `ReleaseShell`: a paired deck, the
/// board origin, no authority, no coordinator origin. Any other command
/// passes. `has_authority` is true when the envelope carries an authority.
///
/// # Errors
/// A `forbidden` rejection.
pub fn guard(
    command: &SessionCommand,
    caller: &Caller,
    origin: CommandOrigin,
    has_authority: bool,
) -> Result<(), DecideError> {
    if !matches!(
        command,
        SessionCommand::ShellCommand { .. } | SessionCommand::ReleaseShell { .. }
    ) {
        return Ok(());
    }
    if has_authority || origin == CommandOrigin::Coordinator {
        return Err(reject(RejectCode::Forbidden, NO_COORDINATOR));
    }
    if !caller.is_peer() || origin != CommandOrigin::Board {
        return Err(reject(RejectCode::Forbidden, BOARD_ONLY));
    }
    Ok(())
}

/// True for the commands that need the worktree root of their session (or
/// of the folder that they start in).
#[must_use]
pub fn needs_root(command: &SessionCommand) -> bool {
    matches!(
        command,
        SessionCommand::Prompt { .. }
            | SessionCommand::FollowUp { .. }
            | SessionCommand::Steer { .. }
            | SessionCommand::ShellCommand { .. }
            | SessionCommand::Start { .. }
            | SessionCommand::Resume { .. }
            | SessionCommand::Import { .. }
            | SessionCommand::Revert { .. }
            | SessionCommand::ResolveRevert { .. }
    )
}

/// The root-busy refusal: while a shell row of `root` is unsettled, the
/// input, start, resume, import, revert and retry-restore commands of every
/// session of the root are refused. `Interrupt` and `Stop` are not listed.
/// `ShellCommand` has its own check in [`decide`], in the order of its gates.
/// With no root (a folder that does not exist yet) nothing is busy.
///
/// # Errors
/// An `invalid_state` rejection.
pub fn check_root(
    d: &Decision<'_>,
    root: Option<&str>,
    command: &SessionCommand,
) -> Result<(), DecideError> {
    let root = match command {
        SessionCommand::Prompt { .. }
        | SessionCommand::FollowUp { .. }
        | SessionCommand::Steer { .. }
        | SessionCommand::Start { .. }
        | SessionCommand::Resume { .. }
        | SessionCommand::Import { .. }
        | SessionCommand::Revert { .. } => root.map(str::to_owned),
        // A retry of a file restore is bound to the root of its revert.
        SessionCommand::ResolveRevert {
            revert_id,
            resolution: RevertResolution::RetryRestore,
            ..
        } => d
            .revert(revert_id)?
            .map(|r| r.revert.worktree_root)
            .or_else(|| root.map(str::to_owned)),
        _ => None,
    };
    if let Some(root) = root
        && d.shell_root_unsettled(&root)?
    {
        return Err(reject(RejectCode::InvalidState, BUSY));
    }
    Ok(())
}

/// The refusal of a shell command that reaches the decision without the
/// session manager (a direct call has no resolved root).
pub(super) fn refuse_direct() -> DecideError {
    reject(
        RejectCode::Unsupported,
        "shell commands are decided by the session manager",
    )
}

/// Decides `ShellCommand` and `ReleaseShell`. `root` is the resolved root of
/// the session folder; `restarting` is the restart freeze.
///
/// # Errors
/// A stored rejection, or a store error.
pub fn decide(
    d: &mut Decision<'_>,
    policy: &Policy,
    restarting: bool,
    root: Option<&str>,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    match command {
        SessionCommand::ShellCommand {
            session_id,
            command,
        } => shell_command(d, policy, restarting, root, &session_id, command),
        SessionCommand::ReleaseShell {
            session_id,
            effect_id,
        } => release(d, &session_id, &effect_id),
        _ => Err(refuse_direct()),
    }
}

fn shell_command(
    d: &mut Decision<'_>,
    policy: &Policy,
    restarting: bool,
    root: Option<&str>,
    session_id: &SessionId,
    command: String,
) -> Result<CommandResult, DecideError> {
    if !policy.allow_shell() {
        return Err(reject(RejectCode::Forbidden, OFF));
    }
    // (FG-8) A land holds its session.
    crate::store::session_holds::check_free(d.connection(), session_id)?;
    if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS || command.contains('\0') {
        return Err(reject(RejectCode::InvalidCommand, BAD_TEXT));
    }
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if s.state == SessionState::Exited && s.hibernated {
        return Err(reject(RejectCode::InvalidState, RESUME_FIRST));
    }
    if matches!(s.state, SessionState::Exited | SessionState::Lost) {
        return Err(reject(RejectCode::InvalidState, "the session has ended"));
    }
    // The same revert holds as `live_session` (B8, D10 Q5).
    if let Some(revert) = s.revert.as_ref().filter(|r| r.blocks_input()) {
        return Err(reject(
            RejectCode::InvalidState,
            match revert.state {
                RevertState::ReconciliationRequired => {
                    "a revert needs reconciliation: the session takes no input"
                }
                _ => "a revert runs: the session takes no input",
            },
        ));
    }
    if let Some(cwd) = s.cwd.as_deref()
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
    if s.current_run.is_some()
        || s.current_turn.is_some()
        || d.pending_input_effects(session_id)? > 0
        || !d.shell_open_rows(session_id)?.is_empty()
    {
        return Err(reject(RejectCode::InvalidState, NOT_IDLE));
    }
    let root = root.ok_or_else(|| reject(RejectCode::InvalidState, NO_FOLDER))?;
    if d.shell_root_unsettled(root)? {
        return Err(reject(RejectCode::InvalidState, BUSY));
    }
    if !d.shell_admission_open()? {
        return Err(reject(RejectCode::InvalidState, STOPPING));
    }
    if restarting {
        return Err(reject(
            RejectCode::InvalidState,
            super::restart_gate::RESTARTING,
        ));
    }
    let provider = s
        .provider
        .unwrap_or(fleetdeck_proto::event::ProviderKind::Unknown);
    let effect_id = d
        .enqueue(
            session_id,
            EffectPayload::ShellCommand {
                command: command.clone(),
                provider,
                worktree_root: root.to_owned(),
            },
        )?
        .effect_id
        .as_str()
        .to_owned();
    d.shell_insert(&effect_id, session_id, provider, root, &command)?;
    Ok(result(session_id, None))
}

fn release(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    effect_id: &str,
) -> Result<CommandResult, DecideError> {
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let row = d
        .shell_run(effect_id)?
        .filter(|r| r.session_id == *session_id)
        .ok_or_else(|| reject(RejectCode::InvalidCommand, NO_SUCH_COMMAND))?;
    let ended = matches!(s.state, SessionState::Exited | SessionState::Lost);
    let releasable = row.state == ShellState::AwaitingProof || (!row.state.is_terminal() && ended);
    if !releasable || !d.shell_settle(effect_id, ShellState::Unknown)? {
        return Err(reject(RejectCode::InvalidState, SETTLED));
    }
    let mut refs = serde_json::Map::new();
    refs.insert("shell".to_owned(), "released".into());
    d.append(
        session_id,
        Event::ToolCompleted {
            node_id: format!("shell-{effect_id}").into(),
            status: ToolStatus::Failed,
            files: Vec::new(),
            exit_code: None,
            provider_refs: refs,
        }
        .into(),
    )?;
    // The station has no generic audit event kind: the owner's release is
    // recorded as a session notice (B8).
    d.append(
        session_id,
        Event::SessionNotice {
            code: "shell_released".to_owned(),
            text: format!("{effect_id} {}", row.worktree_root),
        }
        .into(),
    )?;
    Ok(result(session_id, None))
}

/// What the shell part of an interrupt decided (B8).
pub(super) enum ShellInterrupt {
    /// The session has no run: the interrupt is accepted with no effect.
    Done(CommandResult),
    /// A Codex command may be with the provider before its turn started: the
    /// interrupt effect is enqueued without the "no turn runs" check.
    Provider,
    /// No shell row, or a run exists: the usual interrupt goes on.
    None,
}

/// The interrupt of a session that has a shell command: stores a cancel
/// request on each of its non-terminal rows. [`ShellInterrupt::Done`] when
/// the session has no run, so the interrupt is accepted with no effect;
/// [`ShellInterrupt::Provider`] when no run exists yet but a Codex row is
/// `running` (its `thread/shellCommand` was sent, and the Codex core holds
/// the interrupt until `turn/started`); [`ShellInterrupt::None`] when the
/// usual interrupt goes on (no shell row, or a run exists).
///
/// # Errors
/// A store error.
pub(super) fn interrupt(
    d: &mut Decision<'_>,
    session_id: &SessionId,
) -> Result<ShellInterrupt, DecideError> {
    let rows = d.shell_open_rows(session_id)?;
    if rows.is_empty() {
        return Ok(ShellInterrupt::None);
    }
    for row in &rows {
        d.shell_request_cancel(&row.effect_id)?;
    }
    let run = d
        .summary(session_id)?
        .is_some_and(|s| s.current_run.is_some() || s.current_turn.is_some());
    if run {
        return Ok(ShellInterrupt::None);
    }
    let sent = rows
        .iter()
        .any(|r| r.provider == "codex" && r.state == ShellState::Running);
    Ok(if sent {
        ShellInterrupt::Provider
    } else {
        ShellInterrupt::Done(result(session_id, None))
    })
}

/// The revert barrier (B8): refuses while a shell row of `root` is
/// unsettled, and when the start checkpoint of the target turn `app_turn`
/// was taken at a lower folder generation than the root has now.
///
/// # Errors
/// An `invalid_state` rejection.
pub fn revert_barrier(d: &Decision<'_>, root: &str, app_turn: &str) -> Result<(), DecideError> {
    match d.shell_barrier(root, app_turn)? {
        None => Ok(()),
        Some(Barrier::Running) => Err(reject(RejectCode::InvalidState, BUSY)),
        Some(Barrier::RanAfterTurn) => Err(reject(RejectCode::InvalidState, RAN_AFTER_TURN)),
    }
}

/// The worktree root of a folder, as the turn fences name it: the canonical
/// Git root, else the canonical folder itself. `None` when the folder does
/// not exist.
pub async fn root_of(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    match Worktree::open(path, GitOptions::isolated()).await {
        Ok(worktree) => Some(worktree.root().to_string_lossy().into_owned()),
        Err(_) => std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

/// The root that [`check_root`] and [`decide`] use for `command`, resolved
/// before the decision transaction (D10 Q1). A start or an import uses its
/// `spec.cwd`; the other commands use the folder of their session.
///
/// # Errors
/// A store error while the session is read.
pub async fn resolve_root(
    store: &Store,
    command: &SessionCommand,
) -> crate::store::Result<Option<String>> {
    if !needs_root(command) {
        return Ok(None);
    }
    let cwd = match command {
        SessionCommand::Start { spec, .. } | SessionCommand::Import { spec, .. } => {
            Some(spec.cwd.clone())
        }
        other => match other.session_id() {
            Some(id) => store.summary(id.clone()).await?.and_then(|s| s.cwd),
            None => None,
        },
    };
    Ok(match cwd {
        Some(cwd) => root_of(Path::new(&cwd)).await,
        None => None,
    })
}
