//! The upload stream of a move (FG-9, design §12.3 step 3, ADR-009 §9): the
//! deck writes the transcript files into a `move-import:<move_id>` stream.
//!
//! - [`MoveUploads`]: the pending manifests of `session.move_prepare`, the
//!   active uploads, and the staging root. One value for each station.
//! - [`serve_upload`]: the task of one stream. It checks the tombstone, makes
//!   the staging folder, records it in `move_staging` before the first byte,
//!   writes and checks the files, and answers by closing its half of the
//!   stream (success) or by resetting the stream with a code (failure).
//!   A failure, a reset and a cancel delete the folder by its identity.
//! - [`MoveUploads::settle`]: the part of `session.move_discard` that resets
//!   an active upload and waits until its task ended.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::fleet_git::{TranscriptManifest, code};
use tokio::sync::{Notify, mpsc, watch};

use super::move_export::MoveFileError;
use super::move_import::{Receiver, delete_folder_by_identity};
use crate::store::Store;
use crate::store::move_staging::StagingState;

/// What the stream task receives from the data channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadEvent {
    /// Received bytes.
    Data(Vec<u8>),
    /// The deck closed its half: the upload is complete.
    Closed,
    /// The deck reset the stream, or the channel ended.
    Reset,
}

/// What the stream task does to its stream.
#[async_trait]
pub trait UploadPort: Send + Sync {
    /// Gives `n` consumed bytes back to the deck.
    async fn consumed(&self, n: usize);
    /// Closes the station half: the success signal.
    async fn close(&self);
    /// Resets the stream with a reason (a code).
    async fn reset(&self, reason: &str);
}

/// The reset reason of a stream that the station does not serve (no staging
/// root).
pub const UNSUPPORTED: &str = "unsupported";
/// The reset reason of a move that has an upload or a staging record already.
pub const BUSY: &str = "busy";
/// The reset reason of a stream for a move that was not prepared.
pub const UNKNOWN_MOVE: &str = "unknown_move";
/// The reset reason of a failure of the station.
pub const INTERNAL: &str = "internal";

/// An active upload of the registry.
struct Active {
    reset: Arc<dyn Fn(&str) + Send + Sync>,
    cancel: Arc<Notify>,
    done: watch::Receiver<bool>,
}

#[derive(Default)]
struct Inner {
    dir: OnceLock<PathBuf>,
    pending: Mutex<HashMap<String, TranscriptManifest>>,
    active: Mutex<HashMap<String, Active>>,
}

/// The uploads of a station (see the module text). Cloning shares the state.
#[derive(Clone, Default)]
pub struct MoveUploads {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MoveUploads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoveUploads").finish_non_exhaustive()
    }
}

/// The registration of one active upload: the entry leaves the registry and
/// the waiters of [`MoveUploads::settle`] wake up when it drops.
pub struct UploadGuard {
    uploads: MoveUploads,
    move_id: String,
    cancel: Arc<Notify>,
    done: watch::Sender<bool>,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        self.uploads
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.move_id);
        let _ = self.done.send(true);
    }
}

impl UploadGuard {
    /// Waits for the cancel of a discard.
    pub async fn cancelled(&self) {
        self.cancel.notified().await;
    }
}

/// What [`MoveUploads::settle`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// No upload was active.
    NoUpload,
    /// An active upload ended.
    Ended,
    /// It did not end in time.
    Timeout,
}

impl MoveUploads {
    /// Sets the staging root (the first call wins).
    pub fn set_dir(&self, dir: PathBuf) {
        let _ = self.inner.dir.set(dir);
    }

    /// The staging root, once set.
    #[must_use]
    pub fn dir(&self) -> Option<&PathBuf> {
        self.inner.dir.get()
    }

    /// Keeps the manifest of a prepared move until its stream opens. `false`
    /// when an upload of the move is active.
    #[must_use]
    pub fn put_pending(&self, move_id: &str, manifest: TranscriptManifest) -> bool {
        if self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(move_id)
        {
            return false;
        }
        self.inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(move_id.to_owned(), manifest);
        true
    }

    /// Takes the manifest of a prepared move.
    #[must_use]
    pub fn take_pending(&self, move_id: &str) -> Option<TranscriptManifest> {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(move_id)
    }

    /// Registers an active upload; `None` when one is active for the move.
    /// `reset` resets the stream with a reason.
    #[must_use]
    pub fn register(
        &self,
        move_id: &str,
        reset: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Option<UploadGuard> {
        let mut active = self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if active.contains_key(move_id) {
            return None;
        }
        let cancel = Arc::new(Notify::new());
        let (done, ended) = watch::channel(false);
        active.insert(
            move_id.to_owned(),
            Active {
                reset,
                cancel: Arc::clone(&cancel),
                done: ended,
            },
        );
        Some(UploadGuard {
            uploads: self.clone(),
            move_id: move_id.to_owned(),
            cancel,
            done,
        })
    }

    /// Resets the active upload of `move_id` with `move_discarded`, cancels
    /// its task and waits until the task ended, at most `limit`.
    pub async fn settle(&self, move_id: &str, limit: Duration) -> Settled {
        let (reset, cancel, mut done) = {
            let active = self
                .inner
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(entry) = active.get(move_id) else {
                return Settled::NoUpload;
            };
            (
                Arc::clone(&entry.reset),
                Arc::clone(&entry.cancel),
                entry.done.clone(),
            )
        };
        reset(code::MOVE_DISCARDED);
        cancel.notify_one();
        match tokio::time::timeout(limit, done.wait_for(|ended| *ended)).await {
            Ok(_) => Settled::Ended,
            Err(_) => Settled::Timeout,
        }
    }

    /// Waits without a limit until the upload of `move_id` ended.
    pub async fn wait_ended(&self, move_id: &str) {
        let done = {
            let active = self
                .inner
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            active.get(move_id).map(|entry| entry.done.clone())
        };
        if let Some(mut done) = done {
            let _ = done.wait_for(|ended| *ended).await;
        }
    }
}

/// The reset reason of a failure.
fn reason_of(e: &MoveFileError) -> &str {
    match e.code {
        code::MOVE_TOO_LARGE | code::MOVE_CHECKSUM => e.code,
        _ => INTERNAL,
    }
}

/// Deletes the staging folder by its identity and marks its record deleted.
async fn clean(store: &Store, move_id: &str, folder: &super::move_import::Folder) {
    let (path, dev, ino) = (folder.path.clone(), folder.dev, folder.ino);
    let _ = tokio::task::spawn_blocking(move || delete_folder_by_identity(&path, dev, ino)).await;
    mark_deleted(store, move_id).await;
}

/// Marks the staging record of `move_id` deleted, whatever its state.
pub async fn mark_deleted(store: &Store, move_id: &str) {
    for from in [StagingState::Uploading, StagingState::Ready] {
        if let Err(e) = store
            .move_staging_set(move_id.to_owned(), from, StagingState::Deleted)
            .await
        {
            tracing::warn!(error = %e, "the staging record is not marked deleted");
        }
    }
}

/// Serves one `move-import:` stream (design §12.3 step 3). `events` carries
/// what the data channel delivers for the stream.
pub async fn serve_upload(
    store: &Store,
    uploads: &MoveUploads,
    port: &dyn UploadPort,
    reset: Arc<dyn Fn(&str) + Send + Sync>,
    move_id: &str,
    mut events: mpsc::UnboundedReceiver<UploadEvent>,
) {
    let Some(moves) = uploads.dir().cloned() else {
        port.reset(UNSUPPORTED).await;
        return;
    };
    let Some(guard) = uploads.register(move_id, reset) else {
        port.reset(BUSY).await;
        return;
    };
    // The tombstone is read after the registration: a discard that inserts it
    // later finds this upload in the registry and cancels it.
    match store.move_tombstoned(move_id.to_owned()).await {
        Ok(false) => {}
        Ok(true) => {
            port.reset(code::MOVE_DISCARDED).await;
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "the tombstone is unreadable");
            port.reset(INTERNAL).await;
            return;
        }
    }
    let Some(manifest) = uploads.take_pending(move_id) else {
        port.reset(UNKNOWN_MOVE).await;
        return;
    };
    let made = {
        let (id, manifest) = (move_id.to_owned(), manifest.clone());
        tokio::task::spawn_blocking(move || Receiver::create(&moves, &id, &manifest)).await
    };
    let mut receiver = match made {
        Ok(Ok(receiver)) => receiver,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the staging folder is not made");
            port.reset(INTERNAL).await;
            return;
        }
        Err(_) => {
            port.reset(INTERNAL).await;
            return;
        }
    };
    let folder = receiver.folder().clone();
    let recorded = store
        .move_staging_insert(
            move_id.to_owned(),
            (
                folder.path.to_string_lossy().into_owned(),
                folder.dev,
                folder.ino,
            ),
            manifest,
        )
        .await;
    match recorded {
        Ok(true) => {}
        Ok(false) => {
            // Another upload or a ready record holds the move.
            let (path, dev, ino) = (folder.path.clone(), folder.dev, folder.ino);
            let _ = tokio::task::spawn_blocking(move || delete_folder_by_identity(&path, dev, ino))
                .await;
            port.reset(BUSY).await;
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "the staging record is not stored");
            clean(store, move_id, &folder).await;
            port.reset(INTERNAL).await;
            return;
        }
    }
    loop {
        let event = tokio::select! {
            biased;
            () = guard.cancelled() => {
                // The discard reset the stream already.
                clean(store, move_id, &folder).await;
                return;
            }
            event = events.recv() => event,
        };
        match event {
            Some(UploadEvent::Data(bytes)) => {
                let n = bytes.len();
                let step = tokio::task::spawn_blocking(move || {
                    let done = receiver.feed(&bytes);
                    (receiver, done)
                })
                .await;
                let (back, done) = match step {
                    Ok(pair) => pair,
                    Err(_) => {
                        clean(store, move_id, &folder).await;
                        port.reset(INTERNAL).await;
                        return;
                    }
                };
                receiver = back;
                if let Err(e) = done {
                    clean(store, move_id, &folder).await;
                    port.reset(reason_of(&e)).await;
                    return;
                }
                port.consumed(n).await;
            }
            Some(UploadEvent::Closed) => {
                let finished = tokio::task::spawn_blocking(move || receiver.finish()).await;
                match finished {
                    Ok(Ok(_)) => {
                        let ready = store
                            .move_staging_set(
                                move_id.to_owned(),
                                StagingState::Uploading,
                                StagingState::Ready,
                            )
                            .await;
                        if matches!(ready, Ok(true)) {
                            port.close().await;
                        } else {
                            clean(store, move_id, &folder).await;
                            port.reset(INTERNAL).await;
                        }
                    }
                    Ok(Err(e)) => {
                        clean(store, move_id, &folder).await;
                        port.reset(reason_of(&e)).await;
                    }
                    Err(_) => {
                        clean(store, move_id, &folder).await;
                        port.reset(INTERNAL).await;
                    }
                }
                return;
            }
            Some(UploadEvent::Reset) | None => {
                clean(store, move_id, &folder).await;
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "move_upload_tests.rs"]
mod tests;
