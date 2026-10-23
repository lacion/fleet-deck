//! The link presence of the stations (B22, design `last-seen.md` §3.3):
//! which stations have a live link and since when (memory of this deck
//! process), and the last time the deck heard each one
//! (`stations.last_seen`).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{MutexGuard, PoisonError};

use fleetdeck_proto::StationId;
use rusqlite::params;

use super::{DeckStore, Result, call};

impl DeckStore {
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
