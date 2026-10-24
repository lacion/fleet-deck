//! Injected wall clock (PLAN §13: replays are deterministic because the clock
//! and the id source are injected).
//!
//! Time on the wire and in the databases is Unix milliseconds (`u64`).
//! Timers and timeouts use `tokio::time`, which tests pause on their own.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A source of wall-clock time in Unix milliseconds.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now_ms(&self) -> u64;
}

/// A shared clock handle.
pub type SharedClock = Arc<dyn Clock>;

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    }
}

/// A clock that moves only when a test moves it. Clones share the time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    pub fn new(start_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start_ms)))
    }

    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_is_after_2025() {
        assert!(SystemClock.now_ms() > 1_735_689_600_000);
    }

    #[test]
    fn the_manual_clock_moves_only_on_request_and_clones_share_it() {
        let clock = ManualClock::new(1_000);
        let shared: SharedClock = Arc::new(clock.clone());
        assert_eq!(shared.now_ms(), 1_000);
        clock.advance(Duration::from_secs(2));
        assert_eq!(shared.now_ms(), 3_000);
        clock.set(42);
        assert_eq!(shared.now_ms(), 42);
    }
}
