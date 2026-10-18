//! The last fleet summary of the deck, kept in memory (R13 P4 task B2,
//! decision P4-8).
//!
//! The deck pushes the summary with the link method `fleet.summary`. The
//! station keeps the last valid one with its receive time and a counter
//! `seq`. The local socket op `fleet_summary` holds a request until `seq`
//! grows (see `local_summary`). Nothing here is ever written to disk.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::fleet_summary::FleetSummary;
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
        }
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
