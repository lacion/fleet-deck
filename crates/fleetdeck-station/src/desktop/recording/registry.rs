//! The live recordings of the station (R13 P5b task S1): the stop latch of
//! each, keyed by the deck fingerprint and the capture ID, and the three
//! station-wide actions on them (the active one of a deck, a stop, the
//! shutdown).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleetdeck_proto::recording::StopReason;

use super::Holder;
use super::latch::Latch;
use crate::desktop::Desktop;
use crate::store::{Store, StoreError};

type Key = (String, String);

/// How many stops without a live recording the registry keeps; the oldest
/// goes first.
const MAX_PENDING: usize = 64;

/// A stop that came before its recording registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingStop {
    pub save: bool,
    pub reason: StopReason,
}

enum Entry {
    /// The recording runs: its memory latch.
    Live(Arc<Latch>),
    /// No recording yet: the stop that the recording gets when it registers.
    Pending(PendingStop),
}

#[derive(Default)]
struct Slots {
    map: HashMap<Key, Entry>,
    /// The keys of the pending stops, the oldest first.
    pending: VecDeque<Key>,
}

impl Slots {
    /// Takes out the pending stop of `key`, if there is one.
    fn take_pending(&mut self, key: &Key) -> Option<PendingStop> {
        let Some(Entry::Pending(stop)) = self.map.get(key) else {
            return None;
        };
        let stop = *stop;
        self.map.remove(key);
        self.pending.retain(|k| k != key);
        Some(stop)
    }
}

/// The recordings that run now, and the stops that wait for a recording.
#[derive(Default)]
pub(crate) struct Registry {
    slots: Mutex<Slots>,
    /// What a test holds still (never in a production build).
    #[cfg(test)]
    pub(crate) hooks: super::rig_tests::Hooks,
}

/// Removes its recording from the registry when it is dropped.
pub(crate) struct Registered<'a> {
    registry: &'a Registry,
    key: Key,
}

impl Registry {
    fn slots(&self) -> MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a recording; it stays until the guard is dropped. A stop that
    /// waits for it is applied to its latch before it becomes live, under
    /// the same lock.
    pub(crate) fn insert(&self, latch: &Arc<Latch>) -> Registered<'_> {
        let key = (latch.deck_fp().to_owned(), latch.capture_id().to_owned());
        {
            let mut slots = self.slots();
            if let Some(stop) = slots.take_pending(&key) {
                latch.signal(stop.save, stop.reason);
            }
            slots
                .map
                .insert(key.clone(), Entry::Live(Arc::clone(latch)));
        }
        Registered {
            registry: self,
            key,
        }
    }

    pub(crate) fn get(&self, deck_fp: &str, capture_id: &str) -> Option<Arc<Latch>> {
        match self
            .slots()
            .map
            .get(&(deck_fp.to_owned(), capture_id.to_owned()))
        {
            Some(Entry::Live(latch)) => Some(Arc::clone(latch)),
            _ => None,
        }
    }

    fn all(&self) -> Vec<Arc<Latch>> {
        self.slots()
            .map
            .values()
            .filter_map(|entry| match entry {
                Entry::Live(latch) => Some(Arc::clone(latch)),
                Entry::Pending(_) => None,
            })
            .collect()
    }

    /// The memory half of a stop, in one step under the lock (no await). A
    /// live recording gets the stop in its latch at once and is returned.
    /// Without one, the stop waits as a pending stop (a discard replaces a
    /// save, never the reverse; the oldest of more than [`MAX_PENDING`]
    /// goes) and the answer is `None`.
    fn stop_in_memory(
        &self,
        deck_fp: &str,
        capture_id: &str,
        stop: PendingStop,
    ) -> Option<Arc<Latch>> {
        let key = (deck_fp.to_owned(), capture_id.to_owned());
        let mut slots = self.slots();
        match slots.map.get_mut(&key) {
            Some(Entry::Live(latch)) => {
                latch.signal(stop.save, stop.reason);
                return Some(Arc::clone(latch));
            }
            Some(Entry::Pending(held)) => {
                if held.save && !stop.save {
                    *held = stop;
                }
            }
            None => {
                if slots.pending.len() >= MAX_PENDING
                    && let Some(oldest) = slots.pending.pop_front()
                {
                    slots.map.remove(&oldest);
                }
                slots.pending.push_back(key.clone());
                slots.map.insert(key, Entry::Pending(stop));
            }
        }
        None
    }

    /// Drops the pending stop of a recording (the store answered that the
    /// row is not a running recording).
    fn drop_pending(&self, deck_fp: &str, capture_id: &str) {
        let key = (deck_fp.to_owned(), capture_id.to_owned());
        self.slots().take_pending(&key);
    }

    /// The pending stop of a recording, if any (tests).
    #[cfg(test)]
    pub(crate) fn pending_stop(&self, deck_fp: &str, capture_id: &str) -> Option<PendingStop> {
        match self
            .slots()
            .map
            .get(&(deck_fp.to_owned(), capture_id.to_owned()))
        {
            Some(Entry::Pending(stop)) => Some(*stop),
            _ => None,
        }
    }

    /// How many pending stops there are (tests).
    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.slots().pending.len()
    }

    /// Stops a recording in memory, as `stop_recording` does first (tests).
    #[cfg(test)]
    pub(crate) fn stop_pending_or_live(&self, deck_fp: &str, capture_id: &str, stop: PendingStop) {
        drop(self.stop_in_memory(deck_fp, capture_id, stop));
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut slots = self.registry.slots();
        if matches!(slots.map.get(&self.key), Some(Entry::Live(_))) {
            slots.map.remove(&self.key);
        }
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
        // First, in memory and before any await: a live recording gets the
        // stop in its latch; else the stop waits in the registry, and the
        // recording that registers later applies it before it launches.
        let stop = PendingStop { save, reason };
        if let Some(latch) = self.recordings.stop_in_memory(deck_fp, capture_id, stop) {
            latch.stop(save, reason).await;
            return Ok(StopOutcome::Live);
        }
        #[cfg(test)]
        self.recordings.hooks.wait_after_miss().await;
        let held = store
            .recording_latch(deck_fp, capture_id, save, reason)
            .await?;
        if held.is_none() {
            self.recordings.drop_pending(deck_fp, capture_id);
            return Ok(StopOutcome::NotRunning);
        }
        #[cfg(test)]
        self.recordings.hooks.wait_after_write().await;
        // The operation may have registered meanwhile: it took the pending
        // stop at its registration. Its latch gets the durable write and the
        // phase here too.
        if let Some(latch) = self.recordings.get(deck_fp, capture_id) {
            latch.stop(save, reason).await;
            return Ok(StopOutcome::Live);
        }
        Ok(StopOutcome::Latched)
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
