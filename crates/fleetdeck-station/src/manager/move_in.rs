//! The target side of a move (FG-9, design §12.3): the per-move locks, the
//! state of the uploads, `session.move_prepare` and `session.move_discard`.
//! The decision of `MoveIn` is in `move_in_decide.rs`. The launch admission,
//! the commit, the release and the undo handlers come in T4.2d.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    MoveDiscardAnswer, MoveDiscardEffect, MoveDiscardState, TranscriptManifest, code,
};
use fleetdeck_proto::{Event, SessionId};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::manager::SessionManager;
use crate::manager::land::move_session::move_id_ok;
use crate::store::move_discard::Cleanup;
use crate::store::move_staging::StagingState;
use crate::store::{NewEvent, Store, StoreError};
use crate::workspace::fleet_git::{
    MoveFileError, MoveUploads, Settled, check_manifest, delete_cleanup, delete_folder_by_identity,
    mark_deleted,
};

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
    // (FG-9: T4.2d uses it)
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn try_lock(&self, move_id: &str) -> Option<OwnedMutexGuard<()>> {
        self.of(move_id).try_lock_owned().ok()
    }
}

/// What the target keeps for the moves in memory: the locks and the uploads.
/// One slot on the session manager.
#[derive(Debug, Default)]
pub struct MoveState {
    locks: MoveLocks,
    uploads: MoveUploads,
}

impl MoveState {
    /// The per-move locks.
    #[must_use]
    // (FG-9: T4.2d uses it)
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn locks(&self) -> &MoveLocks {
        &self.locks
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
    /// The memory of the target moves, made at its first use.
    #[must_use]
    pub fn move_state(&self) -> &MoveState {
        self.moves.get_or_init(MoveState::default)
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

    /// `session.move_discard` (design §12.3, "Abort", steps 1 to 6), without
    /// the `MoveIn` effect (T4.2d): the per-move lock, the transaction by
    /// state, then the settle of an active upload and the deletion by
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
        let ended = (tx.state == MoveDiscardState::Abandoned)
            .then(|| tx.session_id.clone().map(|id| (id, END_ABANDONED)))
            .flatten();
        let effect = match state.uploads.settle(move_id, SETTLE).await {
            Settled::NoUpload => MoveDiscardEffect::None,
            Settled::Ended => MoveDiscardEffect::Settled,
            Settled::Timeout => {
                let (store, uploads) = (self.store.clone(), state.uploads.clone());
                let (id, cleanup) = (move_id.to_owned(), tx.cleanup);
                tokio::spawn(async move {
                    uploads.wait_ended(&id).await;
                    finish_discard(&store, &id, cleanup, ended).await;
                });
                return Ok(MoveDiscardAnswer {
                    state: MoveDiscardState::Settling,
                    effect: None,
                });
            }
        };
        finish_discard(&self.store, move_id, tx.cleanup, ended).await;
        Ok(answer(tx.state, effect))
    }
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
