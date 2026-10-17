//! The target side of a move (FG-9, design §12.3): the per-move locks, the
//! state of the uploads and of the running `MoveIn` effects,
//! `session.move_prepare` and `session.move_discard`. The decision of `MoveIn`
//! is in `move_in_decide.rs`. The effect task is in `move_launch.rs`, the
//! commit, the release and the confirm in `move_commit.rs`, and the held
//! prompt in `move_prompt.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    MoveDiscardAnswer, MoveDiscardEffect, MoveDiscardState, TranscriptManifest, code,
};
use fleetdeck_proto::{Event, SessionId};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, watch};

use crate::manager::SessionManager;
use crate::manager::land::move_session::move_id_ok;
use crate::store::move_discard::Cleanup;
use crate::store::move_staging::StagingState;
use crate::store::{NewEvent, Store, StoreError};
use crate::workspace::fleet_git::{
    MoveFileError, MoveUploads, Settled, check_manifest, delete_cleanup, delete_folder_by_identity,
    mark_deleted,
};

#[path = "move_commit.rs"]
mod commit;
pub(in crate::manager) use commit::check_input;
#[path = "move_prompt.rs"]
pub(in crate::manager) mod prompt;

/// The text of a tombstoned move id (design §12.7, `move_discarded`).
pub const DISCARDED_TEXT: &str = "This move was cancelled; it does not run again.";
/// The end reason of a session whose copy was not used (design §12.7).
const END_ABANDONED: &str = "move_abandoned";

/// How long a discard waits for an upload to end (design §12.3, "Abort").
#[cfg(not(test))]
const SETTLE: Duration = Duration::from_secs(20);
/// The wait, short in the tests.
#[cfg(test)]
const SETTLE: Duration = Duration::from_millis(400);

/// The per-move locks of the target (design §12.3, "The per-move lock and
/// the tombstone", revision 4, B4): one `tokio::sync::Mutex<()>` for each
/// `move_id`, created at its first use. Three parties take it: the launch
/// admission of the `MoveIn` effect, the commit transaction of the effect, and
/// the discard handler.
#[derive(Debug, Default)]
pub struct MoveLocks {
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl MoveLocks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The lock of `move_id`; the same lock for the same id.
    fn of(&self, move_id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(move_id.to_owned()).or_default())
    }

    /// Waits for the lock of `move_id` and holds it until the guard drops.
    pub async fn lock(&self, move_id: &str) -> OwnedMutexGuard<()> {
        self.of(move_id).lock_owned().await
    }

    /// The lock of `move_id` when it is free now; `None` while a party holds
    /// it.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn try_lock(&self, move_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.of(move_id).try_lock_owned().ok()
    }
}

/// The durations of the commit and the confirm (design §12.3, "The commit
/// point"). Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveTimeouts {
    /// A Claude process must be alive this long after the spawn.
    pub liveness: Duration,
    /// The provider must answer the resume within this time.
    pub resume: Duration,
    /// The first prompt after the release must start the conversation within
    /// this time.
    pub first_input: Duration,
}

impl Default for MoveTimeouts {
    fn default() -> Self {
        Self {
            liveness: Duration::from_secs(5),
            resume: Duration::from_secs(120),
            first_input: Duration::from_secs(120),
        }
    }
}

/// A running `MoveIn` effect: its cancel flag and its end flag.
#[derive(Debug)]
pub struct RunSlot {
    /// `true` asks the task to stop (a discard).
    pub cancel: watch::Sender<bool>,
    /// `true` after the task settled its effect.
    pub done: watch::Sender<bool>,
}

/// What the target keeps for the moves in memory: the locks, the uploads and
/// the running effects. One shared value of the manager and its target.
#[derive(Debug, Default)]
pub struct MoveState {
    locks: MoveLocks,
    uploads: MoveUploads,
    runs: Mutex<HashMap<String, Arc<RunSlot>>>,
    timeouts: Mutex<MoveTimeouts>,
}

impl MoveState {
    /// The per-move locks.
    #[must_use]
    pub fn locks(&self) -> &MoveLocks {
        &self.locks
    }

    /// Registers the running effect of `move_id`; `None` when one runs.
    pub fn register(&self, move_id: &str) -> Option<Arc<RunSlot>> {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        if runs.contains_key(move_id) {
            return None;
        }
        let slot = Arc::new(RunSlot {
            cancel: watch::channel(false).0,
            done: watch::channel(false).0,
        });
        runs.insert(move_id.to_owned(), Arc::clone(&slot));
        Some(slot)
    }

    /// The running effect of `move_id`, if any.
    #[must_use]
    pub fn run_of(&self, move_id: &str) -> Option<Arc<RunSlot>> {
        let runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        runs.get(move_id).cloned()
    }

    /// The effect of `move_id` ended and settled: it leaves the registry and
    /// the waiters wake.
    pub fn finish(&self, move_id: &str) {
        let slot = {
            let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
            runs.remove(move_id)
        };
        if let Some(slot) = slot {
            slot.done.send_replace(true);
        }
    }

    /// The durations of the commit and the confirm.
    #[must_use]
    pub fn timeouts(&self) -> MoveTimeouts {
        *self.timeouts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the durations (tests).
    #[cfg(test)]
    pub fn set_timeouts(&self, timeouts: MoveTimeouts) {
        *self.timeouts.lock().unwrap_or_else(PoisonError::into_inner) = timeouts;
    }
}

fn internal(e: &StoreError) -> MoveFileError {
    MoveFileError::new("internal", e.to_string())
}

/// What the discard still does after the settle: it deletes the files by
/// identity, and it ends the session of an abandoned copy.
async fn finish_discard(
    store: &Store,
    move_id: &str,
    cleanup: Option<Cleanup>,
    ended: Option<(SessionId, &'static str)>,
) {
    if let Some(cleanup) = cleanup {
        let _ = tokio::task::spawn_blocking(move || delete_cleanup(&cleanup)).await;
        mark_deleted(store, move_id).await;
    }
    let Some((session_id, reason)) = ended else {
        return;
    };
    match store.summary(session_id.clone()).await {
        Ok(Some(summary))
            if !matches!(summary.state, SessionState::Exited | SessionState::Lost) =>
        {
            let end = NewEvent::from(Event::SessionEnded {
                reason: reason.to_owned(),
                exit_code: None,
            });
            if let Err(e) = store.append(session_id, vec![end]).await {
                tracing::warn!(error = %e, "the abandoned move session is not ended");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "the abandoned move session is unreadable"),
    }
}

/// Deletes the staging folders of the uploads that a restart cut short.
async fn clear_uploading(store: &Store) {
    let rows = match store.move_staging_in(StagingState::Uploading).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "the cut uploads are unreadable");
            return;
        }
    };
    for row in rows {
        let path = PathBuf::from(&row.path);
        let (dev, ino) = (row.dev, row.ino);
        let _ = tokio::task::spawn_blocking(move || {
            delete_folder_by_identity(Path::new(&path), dev, ino)
        })
        .await;
        mark_deleted(store, &row.move_id).await;
    }
}

impl SessionManager {
    /// The memory of the target moves, shared with the target.
    #[must_use]
    pub fn move_state(&self) -> &MoveState {
        &self.moves
    }

    /// The uploads of the station.
    #[must_use]
    pub fn move_uploads(&self) -> &MoveUploads {
        &self.move_state().uploads
    }

    /// Gives the station the staging root `<station dir>/moves` (the first
    /// call wins). Without it `session.move_prepare` answers `unsupported`.
    pub fn set_moves_dir(&self, dir: PathBuf) {
        self.move_uploads().set_dir(dir);
    }

    /// Deletes the staging folders of the uploads that a restart cut short,
    /// by their recorded identity (design §12.3 step 3). The station calls it
    /// once at its start, before the first link.
    pub async fn clear_cut_uploads(&self) {
        clear_uploading(&self.store).await;
    }

    /// `session.move_prepare` (design §12.3 step 3): the checks, then the
    /// manifest waits for the upload stream.
    ///
    /// # Errors
    /// [`MoveFileError`]: `invalid_params`, `move_discarded` (a tombstoned
    /// id), `unsupported` (no staging root), `move_too_large`,
    /// `move_checksum`, `invalid_state` (an upload runs) or `internal`.
    pub async fn move_prepare(
        &self,
        move_id: &str,
        manifest: TranscriptManifest,
    ) -> Result<(), MoveFileError> {
        if !move_id_ok(move_id) {
            return Err(MoveFileError::new(
                "invalid_params",
                "The move id is not valid.",
            ));
        }
        if self
            .store
            .move_tombstoned(move_id.to_owned())
            .await
            .map_err(|e| internal(&e))?
        {
            return Err(MoveFileError::new(code::MOVE_DISCARDED, DISCARDED_TEXT));
        }
        if self.move_uploads().dir().is_none() {
            return Err(MoveFileError::new(
                "unsupported",
                "this station has no folder for moved conversation files",
            ));
        }
        check_manifest(&manifest)?;
        if self.move_uploads().put_pending(move_id, manifest) {
            Ok(())
        } else {
            Err(MoveFileError::new(
                "invalid_state",
                "An upload of this move runs already.",
            ))
        }
    }

    /// The `MoveIn` effect of a discarded move (design §12.3, "Abort", step
    /// 4): a queued effect is retired (`Cancelled`); a running one is
    /// signalled, the process of an abandoned copy is ended, and the wait for
    /// its settle is at most `SETTLE`. `Timeout` carries the slot to wait for.
    async fn settle_effect(
        &self,
        move_id: &str,
        session: Option<&SessionId>,
        abandoned: bool,
    ) -> Result<EffectWait, StoreError> {
        if let Some(session) = session
            && self.store.retire_queued_move_in(session.clone()).await?
        {
            return Ok(EffectWait::Cancelled);
        }
        let slot = self.move_state().run_of(move_id);
        if let Some(slot) = &slot {
            slot.cancel.send_replace(true);
        }
        if abandoned && let Some(session) = session {
            self.target.end_process(session).await;
        }
        let Some(slot) = slot else {
            return Ok(EffectWait::None);
        };
        let mut done = slot.done.subscribe();
        let ended = tokio::time::timeout(SETTLE, done.wait_for(|d| *d)).await;
        Ok(if ended.is_ok() {
            EffectWait::Settled
        } else {
            EffectWait::Timeout(slot)
        })
    }

    /// `session.move_discard` (design §12.3, "Abort", steps 1 to 6): the
    /// per-move lock, the transaction by state, then the settle of the
    /// `MoveIn` effect and of an active upload, and the deletion by
    /// identity. A settle of more than 20 s answers `settling` and goes on in
    /// the background.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_discard(&self, move_id: &str) -> Result<MoveDiscardAnswer, StoreError> {
        let state = self.move_state();
        let guard = state.locks.lock(move_id).await;
        let station = self.policy.current().name().to_owned();
        let tx = self
            .store
            .move_discard_tx(move_id.to_owned(), station)
            .await?;
        drop(guard);
        let answer = |state, effect| MoveDiscardAnswer {
            state,
            effect: Some(effect),
        };
        if tx.state == MoveDiscardState::Resumed {
            return Ok(answer(tx.state, MoveDiscardEffect::None));
        }
        let abandoned = tx.state == MoveDiscardState::Abandoned;
        let ended = abandoned
            .then(|| tx.session_id.clone().map(|id| (id, END_ABANDONED)))
            .flatten();
        let first = self
            .settle_effect(move_id, tx.session_id.as_ref(), abandoned)
            .await?;
        let upload = state.uploads.settle(move_id, SETTLE).await;
        let late = matches!(first, EffectWait::Timeout(_)) || matches!(upload, Settled::Timeout);
        if !late {
            let effect = match (first, upload) {
                (EffectWait::Cancelled, _) => MoveDiscardEffect::Cancelled,
                (EffectWait::Settled, _) | (_, Settled::Ended) => MoveDiscardEffect::Settled,
                _ => MoveDiscardEffect::None,
            };
            finish_discard(&self.store, move_id, tx.cleanup, ended).await;
            return Ok(answer(tx.state, effect));
        }
        let slow = match first {
            EffectWait::Timeout(slot) => Some(slot),
            _ => None,
        };
        let (store, uploads) = (self.store.clone(), state.uploads.clone());
        let (id, cleanup) = (move_id.to_owned(), tx.cleanup);
        tokio::spawn(async move {
            if let Some(slot) = slow {
                let mut done = slot.done.subscribe();
                let _ = done.wait_for(|d| *d).await;
            }
            uploads.wait_ended(&id).await;
            finish_discard(&store, &id, cleanup, ended).await;
        });
        Ok(MoveDiscardAnswer {
            state: MoveDiscardState::Settling,
            effect: None,
        })
    }
}

/// What the discard found of the `MoveIn` effect of the move.
enum EffectWait {
    /// No effect runs.
    None,
    /// A queued effect was retired.
    Cancelled,
    /// A running effect settled.
    Settled,
    /// A running effect did not settle in time.
    Timeout(Arc<RunSlot>),
}

#[cfg(test)]
#[path = "move_target_tests.rs"]
mod target_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::MoveLocks;

    #[tokio::test]
    async fn one_move_id_has_one_lock_and_other_ids_are_independent() {
        let locks = Arc::new(MoveLocks::new());
        let held = locks.lock("m1").await;
        assert!(locks.try_lock("m1").is_none(), "the same id is held");
        assert!(locks.try_lock("m2").is_some(), "another id is free");
        // A waiter on the same id gets the lock only after the holder drops.
        let waiter = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let _guard = locks.lock("m1").await;
            })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "the waiter waits for the holder");
        drop(held);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter takes the lock after the drop")
            .unwrap();
        assert!(locks.try_lock("m1").is_some());
    }
}
