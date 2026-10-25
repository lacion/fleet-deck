//! The last fleet summary of the deck, kept in memory (R13 P4 task B2,
//! decision P4-8).
//!
//! The deck pushes the summary with the link method `fleet.summary`. The
//! station keeps the last valid one with its receive time and a counter
//! `seq`. The local socket op `fleet_summary` holds a request until `seq`
//! grows (see `local_summary`). Nothing here is ever written to disk.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_link::control::Control;
use fleetdeck_proto::fleet_summary::FleetSummary;
use fleetdeck_proto::palette::StationPalette;
use tokio::sync::watch;

/// The stored summary and the counters, under one lock.
#[derive(Debug, Default)]
struct State {
    stored: Option<Stored>,
    /// Grows when a stored summary differs from the last one, and when the
    /// link state changes between up and down.
    seq: u64,
    /// The number of live deck links.
    links: usize,
}

#[derive(Debug)]
struct Stored {
    summary: FleetSummary,
    received: Instant,
}

/// What a reader sees at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub seq: u64,
    /// At least one deck link is live.
    pub link_up: bool,
    /// Whole seconds since the stored summary arrived; 0 without one.
    pub held_s: u64,
    pub summary: Option<FleetSummary>,
}

/// The last fleet summary of this station, with `seq` and the link state.
#[derive(Debug)]
pub struct SummaryCell {
    state: Mutex<State>,
    /// The value of `seq`, for the waiters.
    wake: watch::Sender<u64>,
    /// The control of the current deck link that negotiated
    /// `desktop_shell.v1` (R13 P4 task E): the local wall op sends
    /// `display.directive` over it.
    deck: Mutex<Option<(u64, Control)>>,
    next_token: AtomicU64,
    /// The Omarchy theme palette of this machine (R13 P4 task F1): in
    /// memory only, never written to disk. The deck gets it at each link up.
    palette: Mutex<Option<StationPalette>>,
}

impl Default for SummaryCell {
    fn default() -> Self {
        Self::new()
    }
}

impl SummaryCell {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::default(),
            wake: watch::channel(0).0,
            deck: Mutex::default(),
            next_token: AtomicU64::new(0),
            palette: Mutex::default(),
        }
    }

    /// Keeps `palette` in memory; a later one replaces it.
    pub fn set_palette(&self, palette: StationPalette) {
        *self.palette.lock().unwrap_or_else(PoisonError::into_inner) = Some(palette);
    }

    /// The palette that the last `palette_set` op stored, if any.
    #[must_use]
    pub fn palette(&self) -> Option<StationPalette> {
        self.palette
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stores the control of a deck link with `desktop_shell.v1` and returns
    /// its token. A later link replaces it.
    pub fn set_deck(&self, control: Control) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        *self.deck.lock().unwrap_or_else(PoisonError::into_inner) = Some((token, control));
        token
    }

    /// Clears the deck control, but only when the slot still holds `token`:
    /// the end of an older link does not clear a newer one.
    pub fn clear_deck(&self, token: u64) {
        let mut slot = self.deck.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|(t, _)| *t == token) {
            *slot = None;
        }
    }

    /// The control of the current deck link that has `desktop_shell.v1`.
    #[must_use]
    pub fn deck(&self) -> Option<Control> {
        self.deck
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, c)| c.clone())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores a valid summary. `seq` grows only when the JSON differs from
    /// the stored one (the whole summary, `at` included).
    pub fn store(&self, summary: FleetSummary) {
        let mut state = self.lock();
        let changed = state.stored.as_ref().is_none_or(|s| s.summary != summary);
        state.stored = Some(Stored {
            summary,
            received: Instant::now(),
        });
        if changed {
            state.seq += 1;
            self.wake.send_replace(state.seq);
        }
    }

    /// Counts one live deck link until the guard drops. `seq` grows when the
    /// count goes from 0 to 1 and when it goes back to 0.
    #[must_use]
    pub fn link_up(self: &Arc<Self>) -> LinkUpGuard {
        let mut state = self.lock();
        state.links += 1;
        if state.links == 1 {
            state.seq += 1;
            self.wake.send_replace(state.seq);
        }
        LinkUpGuard {
            cell: Arc::clone(self),
        }
    }

    /// The state now.
    #[must_use]
    pub fn read(&self) -> Reading {
        let state = self.lock();
        Reading {
            seq: state.seq,
            link_up: state.links > 0,
            held_s: state
                .stored
                .as_ref()
                .map_or(0, |s| s.received.elapsed().as_secs()),
            summary: state.stored.as_ref().map(|s| s.summary.clone()),
        }
    }

    /// Waits until `seq > after` or `max` passes, whichever is first.
    pub async fn wait_after(&self, after: u64, max: Duration) {
        let mut seq = self.wake.subscribe();
        let _timed_out = tokio::time::timeout(max, seq.wait_for(|seq| *seq > after))
            .await
            .is_err();
    }
}

/// Holds one live deck link in a [`SummaryCell`].
#[derive(Debug)]
pub struct LinkUpGuard {
    cell: Arc<SummaryCell>,
}

impl Drop for LinkUpGuard {
    fn drop(&mut self) {
        let mut state = self.cell.lock();
        state.links = state.links.saturating_sub(1);
        if state.links == 0 {
            state.seq += 1;
            self.cell.wake.send_replace(state.seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleetdeck_proto::fleet_summary::{DeckState, SummaryCounts};

    pub(crate) fn summary(at: &str) -> FleetSummary {
        FleetSummary {
            v: 1,
            at: at.to_owned(),
            board: None,
            deck: DeckState::Up,
            counts: SummaryCounts {
                working: 1,
                idle: 0,
                needs_you: 0,
                blocked: 0,
                lost: 0,
            },
            needs_you: Vec::new(),
            usage: None,
            stations: Vec::new(),
        }
    }

    #[test]
    fn the_palette_is_kept_in_memory_and_a_later_one_replaces_it() {
        let cell = SummaryCell::new();
        assert_eq!(cell.palette(), None);
        let a = crate::local_palette::tests::sample("#000000");
        let b = crate::local_palette::tests::sample("#ffffff");
        cell.set_palette(a.clone());
        assert_eq!(cell.palette(), Some(a));
        cell.set_palette(b.clone());
        assert_eq!(cell.palette(), Some(b));
    }

    #[test]
    fn summary_cell_starts_empty() {
        let read = SummaryCell::new().read();
        assert_eq!(read.seq, 0);
        assert!(!read.link_up);
        assert_eq!(read.summary, None);
    }

    #[test]
    fn summary_seq_grows_only_for_a_different_json() {
        let cell = SummaryCell::new();
        cell.store(summary("a"));
        assert_eq!(cell.read().seq, 1);
        cell.store(summary("a"));
        assert_eq!(cell.read().seq, 1, "the same summary keeps seq");
        cell.store(summary("b"));
        let read = cell.read();
        assert_eq!(read.seq, 2, "`at` is part of the JSON");
        assert_eq!(read.summary, Some(summary("b")));
    }

    #[test]
    fn summary_seq_grows_when_the_link_goes_up_and_down() {
        let cell = Arc::new(SummaryCell::new());
        let first = cell.link_up();
        let read = cell.read();
        assert_eq!((read.seq, read.link_up), (1, true));
        let second = cell.link_up();
        assert_eq!(cell.read().seq, 1, "a second link is not a change");
        drop(first);
        let read = cell.read();
        assert_eq!((read.seq, read.link_up), (1, true));
        drop(second);
        let read = cell.read();
        assert_eq!((read.seq, read.link_up), (2, false));
    }

    #[tokio::test]
    async fn summary_wait_ends_when_seq_grows() {
        let cell = Arc::new(SummaryCell::new());
        let waiter = {
            let cell = Arc::clone(&cell);
            tokio::spawn(async move { cell.wait_after(0, Duration::from_secs(20)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        cell.store(summary("a"));
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the waiter wakes")
            .unwrap();
    }
}
