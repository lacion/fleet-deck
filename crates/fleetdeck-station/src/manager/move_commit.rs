//! The release, the failure step and the confirm of a moved-in session (FG-9,
//! design §12.3 step 6, "The rules after the release", §12.4A U1, T4.2d):
//! `session.move_release`, `session.move_fail`, the confirm watch of a Claude
//! session, and the check of the first input.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    MoveFailAnswer, MoveInState, MoveReleaseAnswer, MoveReleaseState, code,
};
use tokio::sync::watch;

use super::MoveState;
use super::prompt::{Held, held_prompt};
use crate::manager::{SessionManager, SessionTarget};
use crate::store::move_commit::Released;
use crate::store::move_runs::{self, MoveInRow};
use crate::store::session_holds;
use crate::store::{DecideError, Decision, EffectPayload, Store, StoreError};
use crate::workspace::fleet_git::MoveFileError;

/// The longest pause between two looks of the confirm watch.
const POLL: Duration = Duration::from_millis(100);

fn internal(e: &StoreError) -> MoveFileError {
    MoveFileError::new("internal", e.to_string())
}

fn not_ready() -> MoveFileError {
    MoveFileError::new("invalid_state", "The move is not ready to release.")
}

/// The decision check of an input of a session (design §11.4, §12.3): the
/// hold and the moved checks, then the first input of a moved-in session
/// after its release is noted in the same transaction (revision 4, NEW-2).
///
/// # Errors
/// The rejection of a hold, or a store error.
pub(in crate::manager) fn check_input(
    d: &mut Decision<'_>,
    session_id: &SessionId,
) -> Result<(), DecideError> {
    session_holds::check_free(d.connection(), session_id)?;
    let now = d.now();
    if let Some(row) = move_runs::in_of_session(d.connection(), session_id)? {
        move_runs::note_first_input(d.connection(), &row.move_id, now)?;
    }
    Ok(())
}

impl SessionManager {
    /// The text of the held first prompt of a branch-only move (design
    /// §12.6), from the plan of its `MoveIn` effect and the publication ref
    /// of the session. `None` (with a log line) when the plan is missing.
    async fn held_prompt_of(&self, row: &MoveInRow) -> Option<String> {
        let effects = match self.store.session_effects(row.session_id.clone()).await {
            Ok(effects) => effects,
            Err(e) => {
                tracing::warn!(error = %e, "the plan of the move is unreadable");
                return None;
            }
        };
        let plan = effects.iter().find_map(|e| match &e.payload {
            EffectPayload::MoveIn {
                title,
                publication,
                source_session,
                summary,
                ..
            } => Some((
                title.clone(),
                publication.clone(),
                source_session.clone(),
                summary.clone(),
            )),
            _ => None,
        });
        let git = match self.store.git_session(row.session_id.clone()).await {
            Ok(git) => git,
            Err(e) => {
                tracing::warn!(error = %e, "the Git row of the moved session is unreadable");
                None
            }
        };
        let (Some((title, publication, source_session, summary)), Some(publish_ref)) =
            (plan, git.and_then(|g| g.publish_ref))
        else {
            tracing::warn!(move_id = %row.move_id, "the held prompt of the move cannot be built");
            return None;
        };
        let source = row.source_station.clone().unwrap_or_default();
        Some(held_prompt(&Held {
            title: title.as_deref().unwrap_or("untitled"),
            source_station: &source,
            source_session: source_session.as_str(),
            branch: &publication.local_branch,
            oid: &publication.oid,
            publish_ref: &publish_ref,
            summary: summary.as_deref(),
        }))
    }

    /// `session.move_release` (design §12.3 step 6): the hold goes, the row
    /// is `released` (Claude) or `confirmed` (Codex), and a branch-only move
    /// sends its held first prompt. A repeat answers the stored state.
    ///
    /// # Errors
    /// [`MoveFileError`]: `invalid_state` when the move is not ready, or
    /// `internal`.
    pub async fn move_release(&self, move_id: &str) -> Result<MoveReleaseAnswer, MoveFileError> {
        let Some(row) = self
            .store
            .move_in_run(move_id.to_owned())
            .await
            .map_err(|e| internal(&e))?
        else {
            return Err(not_ready());
        };
        let held = if row.state == MoveInState::Resumed && row.native_session_id.is_none() {
            self.held_prompt_of(&row).await
        } else {
            None
        };
        let outcome = self
            .store
            .move_release_tx(move_id.to_owned(), held)
            .await
            .map_err(|e| internal(&e))?;
        let state = match outcome.released {
            Released::Released => MoveReleaseState::Released,
            Released::Confirmed => MoveReleaseState::Confirmed,
            Released::NotReady(_) => return Err(not_ready()),
        };
        if outcome.changed && row.provider == ProviderKind::Claude {
            self.spawn_confirm(&row);
        }
        Ok(MoveReleaseAnswer { state })
    }

    /// `session.move_fail` (design §12.4A step U1): under the per-move lock,
    /// `resume_failed` becomes `failed`. A process that still runs is ended.
    /// Any other state answers that state and changes nothing.
    ///
    /// # Errors
    /// [`MoveFileError`]: `invalid_state` for an unknown move, or `internal`.
    pub async fn move_fail(&self, move_id: &str) -> Result<MoveFailAnswer, MoveFileError> {
        let Some(row) = self
            .store
            .move_in_run(move_id.to_owned())
            .await
            .map_err(|e| internal(&e))?
        else {
            return Err(MoveFileError::new(
                "invalid_state",
                "No move on this station uses this id.",
            ));
        };
        let guard = self.move_state().locks().lock(move_id).await;
        let state = self.store.move_fail_tx(move_id.to_owned()).await;
        drop(guard);
        let state = state
            .map_err(|e| internal(&e))?
            .ok_or_else(|| MoveFileError::new("invalid_state", "The move row is gone."))?;
        if matches!(state, MoveInState::ResumeFailed | MoveInState::Failed) {
            self.target.end_process(&row.session_id).await;
        }
        Ok(MoveFailAnswer { state })
    }

    /// Starts the confirm watch of a released Claude session.
    fn spawn_confirm(&self, row: &MoveInRow) {
        let watch = Confirm {
            store: self.store.clone(),
            target: Arc::clone(&self.target),
            moves: Arc::clone(&self.moves),
            move_id: row.move_id.clone(),
            session: row.session_id.clone(),
            expected: row.native_session_id.clone(),
        };
        let shutdown = self.shutdown.subscribe();
        tokio::spawn(watch.run(shutdown));
    }
}

/// The confirm watch of a released Claude session (design §12.3, "The rules
/// after the release"): `session.started` with the expected id (any id for a
/// branch-only move) confirms; another id, an exit before `session.started`,
/// or no `session.started` within 120 s after the first input fails.
struct Confirm {
    store: Store,
    target: Arc<SessionTarget>,
    moves: Arc<MoveState>,
    move_id: String,
    session: SessionId,
    /// `None` for a branch-only move.
    expected: Option<String>,
}

impl Confirm {
    async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut head = self.store.subscribe();
        loop {
            if *shutdown.borrow() {
                return;
            }
            let row = match self.store.move_in_run(self.move_id.clone()).await {
                Ok(Some(row)) if row.state == MoveInState::Released => row,
                Ok(_) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "the confirm watch cannot read the move");
                    return;
                }
            };
            match self.store.session_native_id(self.session.clone()).await {
                Ok(Some(id)) => {
                    match &self.expected {
                        Some(expected) if id != *expected => {
                            let text =
                                format!("Claude started conversation {id}, expected {expected}.");
                            self.fail(text).await;
                        }
                        _ => self.confirm().await,
                    }
                    return;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "the native id is unreadable"),
            }
            if self.gone().await {
                let line = self.target.exit_line(&self.session).await;
                self.fail(line).await;
                return;
            }
            if let Some(first) = row.first_input_at {
                let limit = self.moves.timeouts().first_input;
                let waited = self.store.now_ms().saturating_sub(first);
                if u128::try_from(waited).unwrap_or(0) >= limit.as_millis() {
                    self.fail("Claude did not answer the first prompt within 120 s.".to_owned())
                        .await;
                    return;
                }
            }
            tokio::select! {
                _ = head.changed() => {}
                () = tokio::time::sleep(POLL) => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// True when the session has no live process.
    async fn gone(&self) -> bool {
        match self.store.live_process(self.session.clone()).await {
            Ok(process) => process.is_none(),
            Err(e) => {
                tracing::warn!(error = %e, "the live process is unreadable");
                false
            }
        }
    }

    async fn confirm(&self) {
        let moved = self
            .store
            .move_in_advance(
                self.move_id.clone(),
                MoveInState::Released,
                MoveInState::Confirmed,
                None,
                move_runs::Tombstone::Ignore,
            )
            .await;
        if let Err(e) = moved {
            tracing::warn!(error = %e, "the confirm of a move was not stored");
        }
    }

    /// The failure after the commit: the process ends, then one transaction
    /// stores `resume_failed`, the hold and the stopped push row.
    async fn fail(&self, text: String) {
        self.target.end_process(&self.session).await;
        let stored = self
            .store
            .move_resume_failed(
                self.move_id.clone(),
                code::MOVE_RESUME_FAILED.to_owned(),
                text,
            )
            .await;
        if let Err(e) = stored {
            tracing::warn!(error = %e, "the failed resume of a move was not stored");
        }
    }
}
