//! Revert admission (PLAN §6.6, L13; DL-087).
//!
//! The decision runs in the command transaction and touches neither the
//! provider nor the workspace. It refuses the revert, with the ordinary
//! stored rejection receipt, unless every check passes:
//!
//! 1. the session is live and its provider can roll back its conversation
//!    (`capabilities.rollback`; unknown capabilities fail closed);
//! 2. the session is ready: running, no turn, no run, no open request, no
//!    background work, no pending input effect, no open revert;
//! 3. the target turn has a finalized row of the live process with an
//!    `available` start checkpoint, every later row is finalized or
//!    abandoned, and the latest finalized turn has an `available`
//!    completion checkpoint (the expected workspace before the restore);
//! 4. the worktree is still inside an allowed root.
//!
//! The admitted revert is a row (the durable fence), a
//! `revert.updated {admitted}` event and one revert effect.

use fleetdeck_proto::command::CommandResult;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Operation, RejectCode, SessionId, TurnId};

use super::decide::{live_session, reject, result};
use super::policy::Policy;
use crate::store::{
    DecideError, Decision, EffectPayload, NewRevert, RowSide, RowState, TurnCheckpointRow,
};

/// Decides `session.revert`.
///
/// # Errors
/// A stored rejection, or a store error.
pub fn decide_revert(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> Result<CommandResult, DecideError> {
    let s = live_session(d, session_id, Operation::Revert)?;
    if !s.capabilities.as_ref().is_some_and(|c| c.rollback) {
        return Err(reject(
            RejectCode::Unsupported,
            "the provider cannot roll back its conversation, so the files are not reverted",
        ));
    }
    if d.open_revert(session_id)?.is_some() {
        return Err(reject(RejectCode::InvalidState, "another revert is open"));
    }
    let busy = s.state != SessionState::Running
        || s.current_turn.is_some()
        || s.current_run.is_some()
        || !s.open_requests.is_empty()
        || !s.background_tasks.is_empty();
    if busy || d.pending_input_effects(session_id)? > 0 {
        return Err(reject(
            RejectCode::InvalidState,
            "the session is busy: a turn, input, request or background task is pending",
        ));
    }
    let process = d.live_process(session_id)?.ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session has no running provider process",
        )
    })?;
    let rows = d.revertable_turns(session_id)?;
    let at = rows
        .iter()
        .position(|r| {
            r.turn_id.as_ref() == Some(turn_id)
                && r.process.as_ref() == Some(&process)
                && r.state == RowState::Finalized
        })
        .ok_or_else(|| {
            reject(
                RejectCode::InvalidCommand,
                "no finalized turn of this provider process has that id",
            )
        })?;
    let target = &rows[at];
    let RowSide::Available(target_commit) = &target.start else {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "the turn has no start checkpoint: {}",
                side_reason(&target.start)
            ),
        ));
    };
    let later = &rows[at..];
    if later.iter().any(|r| r.state == RowState::Open) {
        return Err(reject(
            RejectCode::InvalidState,
            "a later turn is not finalized yet",
        ));
    }
    let finalized: Vec<&TurnCheckpointRow> = later
        .iter()
        .filter(|r| r.state == RowState::Finalized)
        .collect();
    if finalized
        .iter()
        .any(|r| r.process.as_ref() != Some(&process) || r.worktree_root != target.worktree_root)
    {
        return Err(reject(
            RejectCode::InvalidState,
            "a later turn ran in another process or worktree",
        ));
    }
    let current = finalized
        .last()
        .copied()
        .ok_or_else(|| reject(RejectCode::InvalidState, "no finalized turn"))?;
    let RowSide::Available(current_commit) = &current.end else {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "the latest turn has no completion checkpoint: {}",
                side_reason(&current.end)
            ),
        ));
    };
    let root = target.worktree_root.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the turn has no worktree checkpoint",
        )
    })?;
    if policy.workspace(&root).is_err() {
        return Err(reject(
            RejectCode::Forbidden,
            "the worktree is outside the allowed roots",
        ));
    }
    let turns = u32::try_from(finalized.len())
        .map_err(|_| reject(RejectCode::InvalidCommand, "too many turns"))?;
    let revert = NewRevert {
        revert_id: d.new_id(),
        session_id: session_id.clone(),
        process,
        turn_id: turn_id.clone(),
        app_turn: target.app_turn.clone(),
        current_app_turn: current.app_turn.clone(),
        turns,
        target_commit: target_commit.clone(),
        current_commit: current_commit.clone(),
        worktree_root: root,
    };
    d.admit_revert(&revert)?;
    d.enqueue(
        session_id,
        EffectPayload::Revert {
            revert_id: revert.revert_id.clone(),
            app_turn: revert.app_turn.0.clone(),
            turns,
        },
    )?;
    Ok(result(session_id, None))
}

fn side_reason(side: &RowSide) -> &str {
    match side {
        RowSide::Unavailable(reason) => reason,
        RowSide::Open | RowSide::Capturing => "not decided",
        RowSide::Available(_) => "available",
    }
}
