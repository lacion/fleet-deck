//! The live recordings of the station (R13 P5b task S1): the stop latch of
//! each, keyed by the deck fingerprint and the capture ID, and the three
//! station-wide actions on them (the active one of a deck, a stop, the
//! shutdown).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_proto::recording::StopReason;

use super::Holder;
use super::latch::Latch;
use crate::desktop::Desktop;
use crate::store::{Store, StoreError};

type Key = (String, String);

/// The recordings that run now.
#[derive(Default)]
pub(crate) struct Registry {
    live: Mutex<HashMap<Key, Arc<Latch>>>,
}

/// Removes its recording from the registry when it is dropped.
pub(crate) struct Registered<'a> {
    registry: &'a Registry,
    key: Key,
}

impl Registry {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Arc<Latch>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a recording; it stays until the guard is dropped.
    pub(crate) fn insert(&self, latch: &Arc<Latch>) -> Registered<'_> {
        let key = (latch.deck_fp().to_owned(), latch.capture_id().to_owned());
        self.map().insert(key.clone(), Arc::clone(latch));
        Registered {
            registry: self,
            key,
        }
    }

    pub(crate) fn get(&self, deck_fp: &str, capture_id: &str) -> Option<Arc<Latch>> {
        self.map()
            .get(&(deck_fp.to_owned(), capture_id.to_owned()))
            .cloned()
    }

    fn all(&self) -> Vec<Arc<Latch>> {
        self.map().values().cloned().collect()
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.registry.map().remove(&self.key);
    }
}

/// The recording that holds the capture slot for one deck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveHandle {
    pub capture_id: String,
}

/// What a stop request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopOutcome {
    /// The recording runs on this station: the signal and the latch went out.
    Live,
    /// The recording is running but has no live handle yet: the latch is
    /// written, and the operation rechecks it before the spawn.
    Latched,
    /// The row is not a running recording.
    NotRunning,
}

impl Desktop {
    /// The recording of `deck_fp` that holds the capture slot (also while it
    /// prepares and finalizes). A screenshot, another deck's recording or a
    /// free slot answers `None`.
    pub(crate) fn recording_of(&self, deck_fp: &str) -> Option<ActiveHandle> {
        match self.capture.holder() {
            Some(Holder::Recording {
                deck_fp: fp,
                capture_id,
            }) if fp == deck_fp => Some(ActiveHandle { capture_id }),
            _ => None,
        }
    }

    /// Holds the capture slot as a screenshot does (tests).
    #[cfg(test)]
    pub(crate) fn hold_screenshot(&self) -> impl Drop + '_ {
        self.capture.try_take().unwrap()
    }

    /// One stop request of the deck `deck_fp` for the recording `capture_id`
    /// (design §4.4): the latch of `reason`, and the stop of the live
    /// recorder. The caller checked that the capture is a recording of this
    /// deck and which reasons the deck may send.
    ///
    /// # Errors
    /// The store error of the latch write of a recording without a live
    /// handle.
    pub(crate) async fn stop_recording(
        &self,
        store: &Store,
        deck_fp: &str,
        capture_id: &str,
        reason: StopReason,
    ) -> Result<StopOutcome, StoreError> {
        let save = reason.is_save();
        if let Some(latch) = self.recordings.get(deck_fp, capture_id) {
            latch.stop(save, reason).await;
            return Ok(StopOutcome::Live);
        }
        Ok(
            match store
                .recording_latch(deck_fp, capture_id, save, reason)
                .await?
            {
                Some(_) => StopOutcome::Latched,
                None => StopOutcome::NotRunning,
            },
        )
    }

    /// Orderly shutdown: latches `station_shutdown` and discards every live
    /// recording. The kill goes out before this returns; the durable latch
    /// and the cleanup follow in the tasks of the operations. A recording that
    /// is left over is marked `recording_interrupted` by the next start.
    pub fn shutdown_recordings(&self) {
        for latch in self.recordings.all() {
            latch.discard_in_background(StopReason::StationShutdown);
        }
    }
}
