//! A deterministic time line: one manual clock and one sequential id source.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::{Clock, IdSource, ManualClock, SeqIds, SharedClock, SharedIds};
use ulid::Ulid;

/// 2026-01-01T00:00:00Z in Unix milliseconds: the start of every test clock.
pub const EPOCH_MS: u64 = 1_767_225_600_000;

/// A manual clock and the id source that reads it.
///
/// Clones share the same clock and the same id counter.
#[derive(Debug, Clone)]
pub struct Det {
    clock: ManualClock,
    ids: Arc<SeqIds>,
}

impl Det {
    /// Starts at [`EPOCH_MS`].
    #[must_use]
    pub fn new() -> Self {
        Self::at(EPOCH_MS)
    }

    /// Starts at `start_ms`.
    #[must_use]
    pub fn at(start_ms: u64) -> Self {
        let clock = ManualClock::new(start_ms);
        let ids = Arc::new(SeqIds::new(Arc::new(clock.clone())));
        Self { clock, ids }
    }

    #[must_use]
    pub fn clock(&self) -> SharedClock {
        Arc::new(self.clock.clone())
    }

    #[must_use]
    pub fn ids(&self) -> SharedIds {
        self.ids.clone()
    }

    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    pub fn set(&self, ms: u64) {
        self.clock.set(ms);
    }

    #[must_use]
    pub fn next_ulid(&self) -> Ulid {
        self.ids.next_ulid()
    }

    /// The next id as a typed id (for example `SessionId`).
    #[must_use]
    pub fn next_id<I: From<String>>(&self) -> I {
        fleetdeck_core::new_id(self.ids.as_ref())
    }
}

impl Default for Det {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::SessionId;

    use super::*;

    #[test]
    fn two_runs_give_the_same_ids_and_times() {
        let run = || {
            let det = Det::new();
            let a: SessionId = det.next_id();
            det.advance(Duration::from_secs(1));
            let b = det.next_ulid();
            (a, b, det.now_ms())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn ids_follow_the_clock_and_always_increase() {
        let det = Det::new();
        let first = det.next_ulid();
        let second = det.next_ulid();
        assert!(second > first);
        assert_eq!(first.timestamp_ms(), EPOCH_MS);
        det.advance(Duration::from_millis(250));
        assert_eq!(det.next_ulid().timestamp_ms(), EPOCH_MS + 250);
    }

    #[test]
    fn clones_share_one_time_line() {
        let det = Det::new();
        let other = det.clone();
        let shared = det.clock();
        other.advance(Duration::from_secs(5));
        assert_eq!(det.now_ms(), EPOCH_MS + 5_000);
        assert_eq!(shared.now_ms(), EPOCH_MS + 5_000);
        let a = det.next_ulid();
        let b = other.ids().next_ulid();
        assert!(b > a);
    }
}
