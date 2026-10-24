//! The link presence of the stations (B22, design `last-seen.md` §3.3):
//! which stations have a live link and since when (memory of this deck
//! process), and the last time the deck heard each one
//! (`stations.last_seen`). It also keeps the `station.info` stream of each
//! live link (G6, design `grant-idle.md` §3.2).

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fleetdeck_proto::StationId;
use rusqlite::params;
use tokio::sync::watch;

use super::grant_scopes::turns::Stream;
use super::{DeckStore, Inner, Result, call};

/// B26 §3.3a: the live links that wait for their first return check, by
/// station and link token, and the settle epoch of each station; in memory
/// only. The lock is never held together with another lock.
pub(super) struct SettleState {
    pending: Mutex<Pending>,
    /// Counts the changes of `pending` (the waiting requests watch it).
    changes: watch::Sender<u64>,
}

/// The state under the one lock of [`SettleState`].
#[derive(Default)]
struct Pending {
    /// The waiting link tokens of each station.
    links: BTreeMap<StationId, BTreeSet<u64>>,
    /// Grows by one with each returning link of the station
    /// ([`DeckStore::mark_unsettled`]). An entry is never removed, so an
    /// epoch is never used twice for one station.
    epochs: BTreeMap<StationId, u64>,
}

impl SettleState {
    pub(super) fn new() -> Self {
        Self {
            pending: Mutex::default(),
            changes: watch::channel(0).0,
        }
    }

    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    /// True when link `token` of `station` is live (in `link_features`).
    pub(super) fn link_live(&self, station: &StationId, token: u64) -> bool {
        self.link_features
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(station)
            .is_some_and(|links| links.contains_key(&token))
    }

    /// Removes `token` from the waiting links of `station`; bumps `changes`.
    /// True when the link waited.
    pub(super) fn settle(&self, station: &StationId, token: u64) -> bool {
        let waited = {
            let mut pending = self.settle.pending();
            let links = &mut pending.links;
            let waited = links
                .get_mut(station)
                .is_some_and(|tokens| tokens.remove(&token));
            if links.get(station).is_some_and(BTreeSet::is_empty) {
                links.remove(station);
            }
            waited
        };
        if waited {
            self.settle.changes.send_modify(|n| *n += 1);
        }
        waited
    }

    /// `clear_link_features` calls it (the link ended); bumps `changes`.
    pub(super) fn forget_settle(&self, station: &StationId, token: u64) {
        self.settle(station, token);
        self.settle.changes.send_modify(|n| *n += 1);
    }

    /// Records the stream of link `token` of `station` while that link
    /// lives (G6 §3.2). Lock order: `link_features`, then `link_streams`.
    pub(super) fn set_link_stream(&self, station: &StationId, token: u64, stream: Stream) -> bool {
        let features = self
            .link_features
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !features
            .get(station)
            .is_some_and(|links| links.contains_key(&token))
        {
            return false;
        }
        let mut streams = self
            .link_streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        streams
            .entry(station.clone())
            .or_default()
            .insert(token, stream);
        true
    }

    /// Forgets the stream of link `token` (the link ended).
    pub(super) fn forget_link_stream(&self, station: &StationId, token: u64) {
        let mut streams = self
            .link_streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(links) = streams.get_mut(station) {
            links.remove(&token);
            if links.is_empty() {
                streams.remove(station);
            }
        }
    }

    /// The stream of the newest live link of `station` that answered
    /// `station.info` (G6 §3.2 item 1).
    pub(super) fn current_stream(&self, station: &StationId) -> Option<Stream> {
        let features = self
            .link_features
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let live = features.get(station)?;
        let streams = self
            .link_streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        streams
            .get(station)?
            .iter()
            .rev()
            .find(|(token, _)| live.contains_key(token))
            .map(|(_, stream)| stream.clone())
    }
}

impl DeckStore {
    /// Link `token` of `station` waits for its return check, and the settle
    /// epoch of `station` grows by one. `DeckLink::up` calls it before
    /// `set_link_features`, so the link is never published without its gate.
    pub fn mark_unsettled(&self, station: &StationId, token: u64) {
        {
            let mut pending = self.inner.settle.pending();
            pending
                .links
                .entry(station.clone())
                .or_default()
                .insert(token);
            *pending.epochs.entry(station.clone()).or_default() += 1;
        }
        self.inner.settle.changes.send_modify(|n| *n += 1);
    }

    /// The settle epoch of `station` when no live link of it waits for its
    /// return check; `None` while one waits. A live answer of the station
    /// (`coordinator.policy_now`) holds only while
    /// [`DeckStore::settled_since`] with this epoch is true (B26 §3.3a).
    #[must_use]
    pub fn settle_epoch(&self, station: &StationId) -> Option<u64> {
        let pending = self.inner.settle.pending();
        if pending
            .links
            .get(station)
            .is_some_and(|tokens| !tokens.is_empty())
        {
            return None;
        }
        Some(pending.epochs.get(station).copied().unwrap_or(0))
    }

    /// True when no live link of `station` waits now and no link of it
    /// returned since the deck read `epoch` with [`DeckStore::settle_epoch`].
    #[must_use]
    pub fn settled_since(&self, station: &StationId, epoch: u64) -> bool {
        self.settle_epoch(station) == Some(epoch)
    }

    /// True when link `token` of `station` is live (in `link_features`) and
    /// does not wait. A link that ended is never settled.
    #[must_use]
    pub fn link_settled(&self, station: &StationId, token: u64) -> bool {
        self.inner.link_live(station, token)
            && !self
                .inner
                .settle
                .pending()
                .links
                .get(station)
                .is_some_and(|tokens| tokens.contains(&token))
    }

    /// True when no live link of `station` waits (also true without a link).
    #[must_use]
    pub fn station_settled(&self, station: &StationId) -> bool {
        self.settle_epoch(station).is_some()
    }

    /// Waits at most `wait` until `link_settled(station, token)`. It
    /// subscribes to `changes` first, then checks, then waits for a change
    /// (`tokio::time::timeout_at`) and checks again. False at the deadline or
    /// when the link ended.
    pub async fn wait_link_settled(&self, station: &StationId, token: u64, wait: Duration) -> bool {
        let mut changes = self.inner.settle.changes.subscribe();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if self.link_settled(station, token) {
                return true;
            }
            if !self.inner.link_live(station, token) {
                return false;
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => {}
                _ => return false,
            }
        }
    }

    /// Records whether a link to `station` is up. The first `up` keeps
    /// the deck time as the start of the continuous link; a later `up`
    /// (a second link) keeps it; `down` forgets it. The board version
    /// grows only on a change.
    pub fn set_reachable(&self, station: &StationId, up: bool) {
        let now = self.now_ms();
        let changed = {
            let mut map = self.reachable_map();
            if up {
                match map.entry(station.clone()) {
                    Entry::Vacant(e) => {
                        e.insert(now);
                        true
                    }
                    Entry::Occupied(_) => false,
                }
            } else {
                map.remove(station).is_some()
            }
        };
        if changed {
            self.board_changed();
        }
    }

    /// True while the link to `station` is up.
    #[must_use]
    pub fn is_reachable(&self, station: &StationId) -> bool {
        self.reachable_map().contains_key(station)
    }

    /// The Unix ms since which `station` is reachable without a gap;
    /// `None` while it is not reachable.
    #[must_use]
    pub fn linked_since(&self, station: &StationId) -> Option<i64> {
        self.reachable_map().get(station).copied()
    }

    /// Records that the deck heard `station` at `at` (Unix ms). The
    /// stored time never moves back. An unknown station is not an error.
    /// No board change: the board shows the time only for a station that
    /// is not reachable, and the end of the last link changes the
    /// reachability after this write (design §3.4).
    ///
    /// # Errors
    /// A database error.
    pub async fn station_heard(&self, id: StationId, at: i64) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE stations SET last_seen = MAX(COALESCE(last_seen, ?2), ?2)
                 WHERE station_id = ?1",
                params![id.as_str(), at],
            )?;
            Ok(())
        })
        .await
    }

    fn reachable_map(&self) -> MutexGuard<'_, BTreeMap<StationId, i64>> {
        self.inner
            .reachable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "station_link_tests.rs"]
mod tests;
