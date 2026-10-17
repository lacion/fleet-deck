//! The source side of a session move (FG-9, design §12.2, §12.3 steps 1, 2
//! and 5, §12.4, §12.4A, ADR-009 §4):
//!
//! - [`decide_out`]: `SessionCommand::MoveOut`, the hold and the freeze. It
//!   needs the shells, so `SessionManager::submit` routes it like a land.
//! - [`decide_finish_undo`]: `MoveFinish` (the session becomes `moved`) and
//!   `MoveUndo` (it is resumable again).
//! - `impl SessionManager`: `session.move_export` (the plan and the open
//!   files), `session.move_abort` and `session.move_status`.

use std::time::{Duration, Instant};

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand, StopMode};
use fleetdeck_proto::fleet_git::{
    MoveInState, MovePublication, MoveStatusAnswer, MoveStatusState, PushState, TranscriptManifest,
};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use crate::coordinator::Bindings;
use crate::manager::SessionManager;
use crate::manager::decide::{reject, result};
use crate::manager::policy::Policy;
use crate::manager::{fleet_control, land};
use crate::shell::Shells;
use crate::store::git_state;
use crate::store::land_runs;
use crate::store::move_runs::{self, MoveOutState, NewMoveOut};
use crate::store::session_holds::{self, KIND_MOVE};
use crate::store::{DecideError, Decision, EffectPayload, StoreError};
use crate::workspace::GitOptions;
use crate::workspace::fleet_git::{
    Exported, MoveFileError, MoveSupport, export_files, local_tip, support,
};

/// The longest `move_id`, in bytes.
const MAX_MOVE_ID: usize = 128;
/// The code of a move that the source refuses (design §12.2).
const NOT_READY: &str = "move_not_ready";
/// How long `session.move_export` waits for the process to be gone (Q13).
#[cfg(not(test))]
const STOP_WAIT: Duration = Duration::from_secs(15);
/// The wait, short in the tests.
#[cfg(test)]
const STOP_WAIT: Duration = Duration::from_millis(700);
/// The poll of that wait.
const STOP_POLL: Duration = Duration::from_millis(100);
/// The time limit of the read of the branch tip.
const TIP_TIMEOUT: Duration = Duration::from_secs(10);

/// The text of a move that no longer holds the session.
const NO_MOVE: &str = "No move holds this session.";
const NOT_PUSHED: &str = "The branch is not pushed.";

fn refuse(code: &str, text: &str) -> DecideError {
    reject(RejectCode::InvalidState, format!("{code}: {text}"))
}

fn invalid_state(text: &str) -> DecideError {
    reject(RejectCode::InvalidState, text)
}

/// The refusal of a move command that reaches `decide::decide` (a direct
/// call has no shell list).
pub(in crate::manager) fn refuse_direct() -> DecideError {
    reject(
        RejectCode::Unsupported,
        "the session manager decides a move out with the shells of the station",
    )
}

pub(in crate::manager) fn move_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MOVE_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(in crate::manager) fn check_move_id(id: &str) -> Result<(), DecideError> {
    if move_id_ok(id) {
        Ok(())
    } else {
        Err(reject(
            RejectCode::InvalidCommand,
            "invalid_params: The move id is not valid.",
        ))
    }
}

/// Only the owner's board or a coordinator moves a session (design §12.2
/// item 4; Q14).
pub(in crate::manager) fn check_origin(origin: CommandOrigin) -> Result<(), DecideError> {
    if matches!(origin, CommandOrigin::Board | CommandOrigin::Coordinator) {
        Ok(())
    } else {
        Err(reject(
            RejectCode::Forbidden,
            "only the owner's board or a coordinator moves a session",
        ))
    }
}

/// The publication of a session under the hold (design §12.3 step 1) and the
/// push condition of §12.2 item 2: a worktree session with its own branch,
/// a push row `pushed`, and no newer tip that a trigger saw.
fn publication_of(
    conn: &rusqlite::Connection,
    session_id: &SessionId,
) -> Result<MovePublication, DecideError> {
    let not_pushed = || refuse(NOT_READY, NOT_PUSHED);
    let facts = land_runs::session_facts(conn, session_id)?.ok_or_else(not_pushed)?;
    let (true, true, Some(branch)) = (
        facts.kind == "worktree",
        facts.created_branch && facts.publish_ref.is_some(),
        facts.branch,
    ) else {
        return Err(not_pushed());
    };
    let push = git_state::push_row(conn, session_id)?.ok_or_else(not_pushed)?;
    let Some(oid) = push.last_pushed_oid.clone().filter(|_| {
        push.state == PushState::Pushed
            && push
                .desired_oid
                .as_ref()
                .is_none_or(|desired| Some(desired) == push.last_pushed_oid.as_ref())
    }) else {
        return Err(not_pushed());
    };
    Ok(MovePublication {
        local_branch: branch,
        dest: push.target_dest,
        ref_name: push.ref_name,
        oid,
    })
}

/// Decides `session.move_out` (design §12.3 step 1): the checks of §12.2 items
/// 1, 2 and 5 and the worktree safety checks of the land, then the
/// `move_out_runs` row (with the publication), the hold, and the stop of an
/// idle live session (Q13). The session manager calls it with the shells.
///
/// # Errors
/// A stored rejection, or a store error.
pub(in crate::manager) fn decide_out(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    origin: CommandOrigin,
    shells: Option<&Shells>,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    check_origin(origin)?;
    fleet_control::check(d, policy, bindings, &command)?;
    let SessionCommand::MoveOut {
        session_id,
        move_id,
        to_station,
    } = command
    else {
        return Err(refuse_direct());
    };
    check_move_id(&move_id)?;
    let conn = d.connection();
    if move_runs::get_out(conn, &move_id)?.is_some() {
        return Err(reject(
            RejectCode::InvalidCommand,
            "invalid_params: This move id exists already.",
        ));
    }
    if session_holds::get(conn, &session_id)?.is_some_and(|h| h.kind == KIND_MOVE) {
        return Err(refuse(NOT_READY, "The session is already moving."));
    }
    // A land or a move holds its session, and a moved session does not move.
    session_holds::check_free(conn, &session_id)?;
    let summary = d
        .summary(&session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let stored = summary.worktree.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session has no worktree of its own",
        )
    })?;
    if !land::idle_or_ended(&summary) {
        let text = if summary.open_requests.is_empty() {
            "A turn is running."
        } else {
            "An approval is open."
        };
        return Err(refuse(NOT_READY, text));
    }
    let publication = publication_of(d.connection(), &session_id)?;
    land::worktree_checks(d, policy, shells, &session_id, &stored)?;
    let now = d.now();
    let new = NewMoveOut {
        move_id: move_id.clone(),
        command_id: d.command_id().as_str().to_owned(),
        session_id: session_id.clone(),
        to_station: to_station.to_string(),
        publication,
        authority: d.authority().cloned(),
    };
    move_runs::insert_out(d.connection(), &new, now)?;
    session_holds::insert(d.connection(), &session_id, KIND_MOVE, &move_id, now)?;
    // The freeze (Q13): an idle live session ends before the copy.
    if d.live_process(&session_id)?.is_some() {
        d.enqueue(
            &session_id,
            EffectPayload::Stop {
                how: StopMode::Graceful,
                authority: None,
            },
        )?;
    }
    Ok(result(&session_id, None))
}

/// Decides `session.move_finish` and `session.move_undo` (design §12.3 step
/// 5, §12.4A step U2).
///
/// # Errors
/// A stored rejection, or a store error.
pub(in crate::manager) fn decide_finish_undo(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    origin: CommandOrigin,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    check_origin(origin)?;
    fleet_control::check(d, policy, bindings, &command)?;
    match command {
        SessionCommand::MoveFinish {
            session_id,
            move_id,
            to_station,
            to_session,
        } => {
            check_move_id(&move_id)?;
            finish(d, &session_id, &move_id, (to_station, to_session))
        }
        SessionCommand::MoveUndo {
            session_id,
            move_id,
        } => {
            check_move_id(&move_id)?;
            undo(d, &session_id, &move_id)
        }
        _ => Err(refuse_direct()),
    }
}

/// Step 5: the session becomes `moved`.
fn finish(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    move_id: &str,
    to: (fleetdeck_proto::StationId, SessionId),
) -> Result<CommandResult, DecideError> {
    let summary = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let row = move_runs::get_out(d.connection(), move_id)?;
    match &row {
        Some(r) if &r.session_id != session_id => {
            return Err(reject(
                RejectCode::InvalidCommand,
                "invalid_params: The move does not belong to this session.",
            ));
        }
        Some(r) if r.state == MoveOutState::Finished => return Ok(result(session_id, None)),
        Some(r) if r.state != MoveOutState::Holding => {
            return Err(invalid_state("The move was cancelled."));
        }
        Some(_) => {}
        // The record is lost: the move still ends when nothing runs.
        None => {
            match &summary.moved_to {
                Some(m) if m.move_id == move_id => return Ok(result(session_id, None)),
                Some(_) => return Err(invalid_state("The session moved in another move.")),
                None => {}
            }
            if summary.current_run.is_some() || summary.current_turn.is_some() {
                return Err(invalid_state(
                    "The source session runs again. Stop it, then Fleet Deck finishes the move.",
                ));
            }
        }
    }
    let now = d.now();
    d.append(
        session_id,
        Event::SessionMoved {
            move_id: move_id.to_owned(),
            to_station: to.0,
            to_session: to.1,
        }
        .into(),
    )?;
    d.move_stop_push(session_id)?;
    session_holds::delete(d.connection(), session_id, move_id)?;
    if row.is_some() {
        move_runs::transition_out(
            d.connection(),
            move_id,
            MoveOutState::Holding,
            MoveOutState::Finished,
            now,
        )?;
    }
    Ok(result(session_id, None))
}

/// Step U2: the session is resumable again.
fn undo(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    move_id: &str,
) -> Result<CommandResult, DecideError> {
    let summary = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if summary
        .moved_to
        .as_ref()
        .is_none_or(|m| m.move_id != move_id)
    {
        return Err(invalid_state("This session did not move in this move."));
    }
    let now = d.now();
    d.append(
        session_id,
        Event::SessionMoveUndone {
            move_id: move_id.to_owned(),
        }
        .into(),
    )?;
    move_runs::transition_out(
        d.connection(),
        move_id,
        MoveOutState::Finished,
        MoveOutState::Undone,
        now,
    )?;
    d.move_restore_push(session_id)?;
    Ok(result(session_id, None))
}

fn internal(e: &StoreError) -> MoveFileError {
    MoveFileError::new("internal", e.to_string())
}

fn status_of_out(state: MoveOutState) -> MoveStatusState {
    match state {
        MoveOutState::Holding => MoveStatusState::Holding,
        MoveOutState::Finished => MoveStatusState::Finished,
        MoveOutState::Aborted => MoveStatusState::Aborted,
        MoveOutState::Undone => MoveStatusState::Undone,
    }
}

fn status_of_in(state: MoveInState) -> MoveStatusState {
    match state {
        MoveInState::Staged => MoveStatusState::Staged,
        MoveInState::Imported => MoveStatusState::Imported,
        MoveInState::Launching => MoveStatusState::Launching,
        MoveInState::LaunchRefused => MoveStatusState::LaunchRefused,
        MoveInState::Resumed => MoveStatusState::Resumed,
        MoveInState::Released => MoveStatusState::Released,
        MoveInState::Confirmed => MoveStatusState::Confirmed,
        MoveInState::ResumeFailed => MoveStatusState::ResumeFailed,
        MoveInState::Failed => MoveStatusState::Failed,
        MoveInState::Abandoned => MoveStatusState::Abandoned,
        MoveInState::Discarded => MoveStatusState::Discarded,
        MoveInState::Unknown => MoveStatusState::Unknown,
    }
}

impl SessionManager {
    /// Waits until the session has no live provider process (Q13): the move
    /// stopped it, and the transcript must not change during the copy.
    async fn wait_stopped(&self, session_id: &SessionId) -> Result<(), MoveFileError> {
        let end = Instant::now() + STOP_WAIT;
        loop {
            match self.store.live_process(session_id.clone()).await {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => {}
                Err(e) => return Err(internal(&e)),
            }
            if Instant::now() >= end {
                return Err(MoveFileError::new(
                    "invalid_state",
                    "The session is still stopping.",
                ));
            }
            tokio::time::sleep(STOP_POLL).await;
        }
    }

    /// The tip of the local branch must be the pushed commit (Q4): a commit
    /// that is not on the remote would be lost by the move.
    async fn check_tip(
        &self,
        session_id: &SessionId,
        publication: &MovePublication,
    ) -> Result<(), MoveFileError> {
        let git = self
            .store
            .git_session(session_id.clone())
            .await
            .map_err(|e| internal(&e))?;
        let not_pushed = || MoveFileError::new("invalid_state", NOT_PUSHED);
        let git = git.ok_or_else(not_pushed)?;
        let tip = local_tip(
            &GitOptions::isolated(),
            std::path::Path::new(&git.repo_root),
            &publication.local_branch,
            TIP_TIMEOUT,
        )
        .await;
        if tip.as_deref() == Some(publication.oid.as_str()) {
            Ok(())
        } else {
            Err(not_pushed())
        }
    }

    /// The open files of the move, `None` for a branch-only move: a session
    /// without a native id, or a provider that is not `Native`.
    async fn open_files(&self, session_id: &SessionId) -> Result<Option<Exported>, MoveFileError> {
        let facts = self
            .store
            .history_facts(session_id.clone())
            .await
            .map_err(|e| internal(&e))?;
        let Some(facts) = facts else {
            return Ok(None);
        };
        let native = facts
            .native
            .clone()
            .filter(|n| fleetdeck_proto::command::check_native_session_id(n).is_ok());
        let (Some(native), MoveSupport::Native) = (native, support(facts.provider)) else {
            return Ok(None);
        };
        let policy = self.policy.current();
        let env = crate::history::env_for(Some(&policy), &facts.provider_instance);
        let home = crate::history::locate::resolve_home(&env, facts.provider)
            .filter(|h| h.is_absolute())
            .ok_or_else(MoveFileError::not_found)?;
        let private = self.private_paths();
        let limits = self.store.history().limits().clone();
        let kind = facts.provider;
        let cwd = facts.cwd;
        let task = tokio::task::spawn_blocking(move || {
            let trust = crate::history::locate::Trust {
                private: &private,
                uid: rustix::process::getuid().as_raw(),
                limits: &limits,
                deadline: Instant::now() + limits.deadline,
            };
            export_files(&home, kind, &cwd, &native, &trust)
        });
        match task.await {
            Ok(Ok(exported)) => Ok(Some(exported)),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(MoveFileError::new("internal", e.to_string())),
        }
    }

    /// The `holding` row of `move_id`, else the error `invalid_state`.
    async fn holding_row(&self, move_id: &str) -> Result<move_runs::MoveOutRow, MoveFileError> {
        self.store
            .move_out_run(move_id.to_owned())
            .await
            .map_err(|e| internal(&e))?
            .filter(|r| r.state == MoveOutState::Holding)
            .ok_or_else(|| MoveFileError::new("invalid_state", NO_MOVE))
    }

    /// `session.move_export` (design §12.3 step 2): the publication and, for
    /// a native move, the manifest of the files. The token of the stream is
    /// the caller's (it needs the data channel of the link).
    ///
    /// # Errors
    /// [`MoveFileError`]: `invalid_state` (no move holds the session, the
    /// process still runs, the branch is not pushed, or the files are not
    /// found), `move_too_large`, `busy` or `internal`.
    pub async fn move_export_plan(
        &self,
        move_id: &str,
    ) -> Result<(MovePublication, Option<TranscriptManifest>), MoveFileError> {
        let row = self.holding_row(move_id).await?;
        self.wait_stopped(&row.session_id).await?;
        self.check_tip(&row.session_id, &row.publication).await?;
        let exported = self.open_files(&row.session_id).await?;
        Ok((row.publication, exported.map(|e| e.manifest)))
    }

    /// The open files of the move for its export stream; `None` for a
    /// branch-only move.
    ///
    /// # Errors
    /// [`MoveFileError`], as [`Self::move_export_plan`].
    pub async fn move_export_open(&self, move_id: &str) -> Result<Option<Exported>, MoveFileError> {
        let row = self.holding_row(move_id).await?;
        self.wait_stopped(&row.session_id).await?;
        self.open_files(&row.session_id).await
    }

    /// `session.move_abort` on the source: the hold ends, the session is as
    /// before. An unknown or closed id is ok (ADR-009 §4).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_abort(&self, move_id: &str) -> Result<(), StoreError> {
        self.store
            .move_abort_out(move_id.to_owned())
            .await
            .map(|_| ())
    }

    /// `session.move_status` (design §12.3, "Deck restart"): the state of the
    /// source row, else of the target row, else `discarded` for a tombstone,
    /// else `unknown`.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_status(&self, move_id: &str) -> Result<MoveStatusAnswer, StoreError> {
        let id = move_id.to_owned();
        if let Some(row) = self.store.move_out_run(id.clone()).await? {
            return Ok(MoveStatusAnswer {
                state: status_of_out(row.state),
                session_id: Some(row.session_id),
                code: None,
                text: None,
            });
        }
        if let Some(row) = self.store.move_in_run(id.clone()).await? {
            return Ok(MoveStatusAnswer {
                state: status_of_in(row.state),
                session_id: Some(row.session_id),
                code: row.code,
                text: row.message,
            });
        }
        let state = if self.store.move_tombstoned(id).await? {
            MoveStatusState::Discarded
        } else {
            MoveStatusState::Unknown
        };
        Ok(MoveStatusAnswer {
            state,
            session_id: None,
            code: None,
            text: None,
        })
    }
}

#[cfg(test)]
#[path = "move_tests.rs"]
mod tests;
