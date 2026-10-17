//! The decisions of the worktree trash (DL-150): `session.worktree_trash`,
//! `_restore`, `_purge` and `_release`.
//!
//! A decision reads only durable state and the shell list, so it holds in
//! the command transaction. It runs no Git command and moves no file: the
//! trash decision inserts a `worktree_ops` row in the state `trashing` and
//! queues one effect ([`crate::workspace::trash`] does the file work).
//!
//! While a row is open (any state but `restored`, `purged`, `released`), it
//! holds the original folder and the trash folder against every other use:
//! a start, an import, a resume, a handover, a shell open and an
//! auto-continue refuse with [`custody`].

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{RejectCode, SessionId};

use super::decide::{reject, result};
use super::policy::{Policy, WorkspaceError};
use crate::shell::Shells;
use crate::store::worktree_ops::{
    FAILED, PHASE_PURGE, PHASE_RESTORE, PHASE_TRASH, PURGING, RESTORING, TRASHED, TRASHING,
};
use crate::store::{DecideError, Decision, NewWorktreeOp, WorktreeAction, WorktreeOp, overlaps};

/// The refusal text of a start, resume or shell inside a held folder.
pub const HELD: &str = "the worktree is in the trash or being moved; restore it first";

/// The folder of the trash beside a worktree.
pub const TRASH_DIR: &str = ".fleetdeck-trash";

/// Refuses `path` while an open worktree operation holds it.
///
/// # Errors
/// `invalid_state` with [`HELD`].
pub fn custody(d: &Decision<'_>, path: &str) -> Result<(), DecideError> {
    if d.open_worktree_op_over(path)?.is_some() {
        return Err(reject(RejectCode::InvalidState, HELD));
    }
    Ok(())
}

/// True when a live shell of the station has a start cwd that overlaps
/// `path`.
#[must_use]
pub fn shell_in(shells: Option<&Shells>, path: &str) -> bool {
    shells.is_some_and(|s| s.list().iter().any(|i| overlaps(&i.cwd, path)))
}

/// The refusal for a terminal that is about to open in a worktree.
pub const OPENING: &str = "a terminal is opening in this worktree; wait";

/// True when a start folder in `cwds` (the folders of queued or claimed
/// shell effects) overlaps `path`; the folder is compared as written and as
/// the policy resolves it.
#[must_use]
pub fn cwd_overlaps(cwds: &[String], policy: &Policy, path: &str) -> bool {
    cwds.iter().any(|cwd| {
        overlaps(cwd, path)
            || policy
                .workspace(cwd)
                .is_ok_and(|c| overlaps(&c.to_string_lossy(), path))
    })
}

fn opening_refusal() -> DecideError {
    reject(RejectCode::InvalidState, OPENING)
}

/// Refuses when a queued or claimed shell effect will open in `path`.
fn pending_shell(d: &Decision<'_>, policy: &Policy, path: &str) -> Result<(), DecideError> {
    let cwds = crate::store::shell_effects::pending_cwds(d.connection())?;
    if cwd_overlaps(&cwds, policy, path) {
        return Err(opening_refusal());
    }
    Ok(())
}

const OWNER_ONLY: &str = "only the owner's board changes worktrees";

/// Decides one of the four worktree commands.
///
/// # Errors
/// A stored rejection, or a store error.
pub fn decide(
    d: &mut Decision<'_>,
    policy: &Policy,
    origin: CommandOrigin,
    shells: Option<&Shells>,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    if origin != CommandOrigin::Board {
        return Err(reject(RejectCode::Forbidden, OWNER_ONLY));
    }
    match command {
        SessionCommand::WorktreeTrash { session_id } => {
            trash(d, policy, shells, &session_id, None)?;
            Ok(result(&session_id, None))
        }
        SessionCommand::WorktreeRestore { session_id } => {
            restore(d, &session_id)?;
            Ok(result(&session_id, None))
        }
        SessionCommand::WorktreePurge {
            session_id,
            confirm_name,
        } => {
            purge(d, policy, shells, &session_id, &confirm_name)?;
            Ok(result(&session_id, None))
        }
        SessionCommand::WorktreeRelease { session_id } => {
            release(d, &session_id)?;
            Ok(result(&session_id, None))
        }
        other => Err(reject(
            RejectCode::InvalidCommand,
            format!("`{}` is not a worktree command", other.method()),
        )),
    }
}

/// The refusal for a row that is in the wrong state for the action.
fn wrong_state(row: &WorktreeOp, action: &str) -> DecideError {
    let text = match row.state.as_str() {
        TRASHED => "the worktree is already in the trash".to_owned(),
        TRASHING | RESTORING | PURGING => "the worktree is being moved; wait".to_owned(),
        FAILED => format!(
            "the last move of the worktree failed ({}); restore it or stop tracking it",
            row.phase.as_deref().unwrap_or("unknown phase")
        ),
        other => format!("the worktree is `{other}`, so it cannot be {action}"),
    };
    reject(RejectCode::InvalidState, text)
}

fn not_in_trash() -> DecideError {
    reject(RejectCode::InvalidState, "the worktree is not in the trash")
}

fn shell_refusal() -> DecideError {
    reject(
        RejectCode::InvalidState,
        "a shell is open in this worktree; close it first",
    )
}

/// The refusal for another live or held session in the worktree.
fn sharing_refusal(d: &Decision<'_>, path: &str, except: &SessionId) -> Result<(), DecideError> {
    if let Some(other) = d.sessions_over(path, except)?.first() {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "session `{}` is live in the same worktree",
                other.session_id
            ),
        ));
    }
    Ok(())
}

/// The id prefix of the trash operations that the Fleet Git cleanup makes.
pub const CLEANUP_PREFIX: &str = "cleanup-";

/// The trash decision of the Fleet Git cleanup loop (FG-3, design §6.3): the
/// same checks and the same row as the board's trash, with the expectation
/// `(refs/heads/<branch>, tip)` that the trash effect checks again right
/// before the move. Only the cleanup loop calls it. The command id (the op
/// id) must be `cleanup-<session id>-<unix ms>`. Returns the op id.
///
/// # Errors
/// A stored rejection (the refusal text), or a store error.
pub fn decide_cleanup(
    d: &mut Decision<'_>,
    policy: &Policy,
    shells: Option<&Shells>,
    session_id: &SessionId,
    expect: (String, String),
) -> Result<String, DecideError> {
    trash(d, policy, shells, session_id, Some(expect))?;
    Ok(d.operation_id())
}

/// The suffix of a trash folder name: the first 12 characters of the op id,
/// lower case; for a cleanup op id the last 12 characters of the session id
/// (the random part of its ULID).
fn folder_suffix(op_id: &str, session_id: &SessionId) -> String {
    if op_id.starts_with(CLEANUP_PREFIX) {
        let tail: Vec<char> = session_id.as_str().chars().rev().take(12).collect();
        tail.into_iter().rev().collect::<String>().to_lowercase()
    } else {
        op_id.chars().take(12).collect::<String>().to_lowercase()
    }
}

fn trash(
    d: &mut Decision<'_>,
    policy: &Policy,
    shells: Option<&Shells>,
    session_id: &SessionId,
    expect: Option<(String, String)>,
) -> Result<(), DecideError> {
    // (FG-8) A land or a move holds its session.
    crate::store::session_holds::check_free(d.connection(), session_id)?;
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let stored = s.worktree.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session has no worktree of its own",
        )
    })?;
    if !matches!(s.state, SessionState::Exited | SessionState::Lost) {
        return Err(reject(
            RejectCode::InvalidState,
            "the session has not ended",
        ));
    }
    if s.current_run.is_some() || s.current_turn.is_some() {
        return Err(reject(
            RejectCode::InvalidState,
            "a turn runs in the session",
        ));
    }
    if s.terminal.is_some() {
        return Err(reject(
            RejectCode::InvalidState,
            "a terminal holds the session; return it to board control first",
        ));
    }
    if d.unsettled_effects(session_id)? > 0 {
        return Err(reject(
            RejectCode::InvalidState,
            "the session has work that is not done yet",
        ));
    }
    if let Some(row) = d.open_worktree_op(session_id)? {
        return Err(wrong_state(&row, "moved to the trash"));
    }
    if let Some(other) = d.open_revert_over(&stored)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a revert of session `{}` holds this worktree",
                other.revert.session_id
            ),
        ));
    }
    custody(d, &stored)?;
    sharing_refusal(d, &stored, session_id)?;
    if shell_in(shells, &stored) {
        return Err(shell_refusal());
    }
    pending_shell(d, policy, &stored)?;
    let canonical = policy.workspace(&stored).map_err(|e| match e {
        WorkspaceError::Missing(_) => reject(RejectCode::InvalidState, e.to_string()),
        WorkspaceError::Outside(_) => reject(RejectCode::Forbidden, e.to_string()),
    })?;
    let meta = std::fs::symlink_metadata(Path::new(&stored))
        .map_err(|e| reject(RejectCode::InvalidState, format!("`{stored}`: {e}")))?;
    if !meta.file_type().is_dir() {
        return Err(reject(
            RejectCode::InvalidState,
            "the worktree path is not a folder (a symbolic link is refused)",
        ));
    }
    // A linked worktree has a `.git` file; the main checkout has a `.git`
    // folder.
    let linked =
        std::fs::symlink_metadata(canonical.join(".git")).is_ok_and(|m| m.file_type().is_file());
    if !linked {
        return Err(reject(
            RejectCode::InvalidState,
            "this is not a linked worktree",
        ));
    }
    let parent = canonical.parent().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the worktree has no parent folder",
        )
    })?;
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| reject(RejectCode::InvalidState, "the worktree name is not text"))?;
    if policy.workspace(&parent.to_string_lossy()).is_err() {
        return Err(reject(
            RejectCode::Forbidden,
            "the folder beside the worktree is outside the allowed roots",
        ));
    }
    let op_id = d.operation_id();
    if op_id.is_empty()
        || !op_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(reject(
            RejectCode::InvalidCommand,
            "the command id cannot name a trash folder",
        ));
    }
    let suffix = folder_suffix(&op_id, session_id);
    let trash_path = parent
        .join(TRASH_DIR)
        .join(format!("{name}--{suffix}"))
        .to_string_lossy()
        .into_owned();
    d.insert_worktree_op(&NewWorktreeOp {
        op_id: op_id.clone(),
        session_id: session_id.clone(),
        original_path: canonical.to_string_lossy().into_owned(),
        trash_path,
        dev: meta.dev(),
        ino: meta.ino(),
        expect,
    })?;
    d.enqueue_worktree(session_id, &op_id, WorktreeAction::Trash)?;
    Ok(())
}

fn restore(d: &mut Decision<'_>, session_id: &SessionId) -> Result<(), DecideError> {
    let row = d.open_worktree_op(session_id)?.ok_or_else(not_in_trash)?;
    let allowed = match row.state.as_str() {
        TRASHED => true,
        FAILED => matches!(
            row.phase.as_deref(),
            Some(PHASE_TRASH) | Some(PHASE_RESTORE)
        ),
        _ => false,
    };
    if !allowed {
        return Err(wrong_state(&row, "restored"));
    }
    if !d.move_worktree_op(&row.op_id, &row.state, RESTORING)? {
        return Err(wrong_state(&row, "restored"));
    }
    d.enqueue_worktree(session_id, &row.op_id, WorktreeAction::Restore)?;
    Ok(())
}

fn purge(
    d: &mut Decision<'_>,
    policy: &Policy,
    shells: Option<&Shells>,
    session_id: &SessionId,
    confirm_name: &str,
) -> Result<(), DecideError> {
    let row = d.open_worktree_op(session_id)?.ok_or_else(not_in_trash)?;
    let allowed = match row.state.as_str() {
        TRASHED => true,
        FAILED => row.phase.as_deref() == Some(PHASE_PURGE),
        _ => false,
    };
    if !allowed {
        return Err(wrong_state(&row, "deleted"));
    }
    let folder = Path::new(&row.trash_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if confirm_name != folder {
        return Err(reject(
            RejectCode::InvalidCommand,
            "type the folder name to confirm",
        ));
    }
    if let Some(parent) = Path::new(&row.trash_path).parent()
        && policy.workspace(&parent.to_string_lossy()).is_err()
    {
        return Err(reject(
            RejectCode::Forbidden,
            "the trash folder is outside the allowed roots",
        ));
    }
    sharing_refusal(d, &row.trash_path, session_id)?;
    sharing_refusal(d, &row.original_path, session_id)?;
    if shell_in(shells, &row.trash_path) || shell_in(shells, &row.original_path) {
        return Err(shell_refusal());
    }
    pending_shell(d, policy, &row.trash_path)?;
    pending_shell(d, policy, &row.original_path)?;
    if !d.move_worktree_op(&row.op_id, &row.state, PURGING)? {
        return Err(wrong_state(&row, "deleted"));
    }
    d.enqueue_worktree(session_id, &row.op_id, WorktreeAction::Purge)?;
    Ok(())
}

fn release(d: &mut Decision<'_>, session_id: &SessionId) -> Result<(), DecideError> {
    let row = d.open_worktree_op(session_id)?.ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the worktree has no failed move to stop tracking",
        )
    })?;
    if row.state != FAILED {
        return Err(reject(
            RejectCode::InvalidState,
            "only a failed move can be released",
        ));
    }
    if !d.move_worktree_op(&row.op_id, FAILED, crate::store::worktree_ops::RELEASED)? {
        return Err(wrong_state(&row, "released"));
    }
    Ok(())
}
