//! Injected id source (PLAN §12: `ulid`; §13: deterministic replays).
//!
//! Every id that Fleet Deck makes (sessions, runs, commands, incarnations,
//! inputs, plans) is a ULID string in a typed newtype of `fleetdeck-proto`.
//! ULIDs sort by creation time; [`UlidSource`] is strictly monotonic inside
//! one process, also when many ids share one millisecond.

use std::sync::{Arc, Mutex};

use ring::rand::{SecureRandom, SystemRandom};
use ulid::Ulid;

use crate::clock::SharedClock;

/// A source of new ULIDs.
pub trait IdSource: Send + Sync + std::fmt::Debug {
    fn next_ulid(&self) -> Ulid;
}

/// A shared id source handle.
pub type SharedIds = Arc<dyn IdSource>;

/// A new typed id, for example `new_id::<SessionId>(&*ids)`.
pub fn new_id<I: From<String>>(ids: &dyn IdSource) -> I {
    I::from(ids.next_ulid().to_string())
}

/// The real id source: clock time plus 80 random bits, strictly increasing.
#[derive(Debug)]
pub struct UlidSource {
    clock: SharedClock,
    rng: SystemRandom,
    last: Mutex<Ulid>,
}

impl UlidSource {
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            rng: SystemRandom::new(),
            last: Mutex::new(Ulid::nil()),
        }
    }

    fn random_bits(&self) -> u128 {
        let mut bytes = [0u8; 16];
        // The system generator does not fail on Linux or macOS. If it ever
        // does, the monotonic counter below still keeps ids unique.
        let _ = self.rng.fill(&mut bytes);
        u128::from_le_bytes(bytes)
    }
}

impl IdSource for UlidSource {
    fn next_ulid(&self) -> Ulid {
        let fresh = Ulid::from_parts(self.clock.now_ms(), self.random_bits());
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        // Same millisecond, or a clock that went back: count up from the
        // last id. `increment` returns None only after 2^80 ids in one ms.
        let id = if fresh > *last {
            fresh
        } else {
            last.increment()
                .unwrap_or_else(|| Ulid::from_parts(last.timestamp_ms() + 1, 0))
        };
        *last = id;
        id
    }
}

/// A deterministic id source for tests and replays: clock time plus a counter
/// (1, 2, 3, ...).
#[derive(Debug)]
pub struct SeqIds {
    clock: SharedClock,
    next: Mutex<u128>,
}

impl SeqIds {
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            next: Mutex::new(1),
        }
    }
}

impl IdSource for SeqIds {
    fn next_ulid(&self) -> Ulid {
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        let id = Ulid::from_parts(self.clock.now_ms(), *next);
        *next += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::SessionId;

    use super::*;
    use crate::clock::{ManualClock, SystemClock};

    #[test]
    fn ulids_are_strictly_increasing_inside_one_millisecond() {
        let clock = ManualClock::new(1_700_000_000_000);
        let ids = UlidSource::new(Arc::new(clock.clone()));
        let mut prev = ids.next_ulid();
        for _ in 0..1_000 {
            let id = ids.next_ulid();
            assert!(id > prev);
            assert_eq!(id.timestamp_ms(), 1_700_000_000_000);
            prev = id;
        }
        // A clock that goes back does not make an older id.
        clock.set(1_000);
        assert!(ids.next_ulid() > prev);
    }

    #[test]
    fn ulids_from_the_system_clock_are_unique_across_threads() {
        let ids: SharedIds = Arc::new(UlidSource::new(Arc::new(SystemClock)));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let ids = ids.clone();
                std::thread::spawn(move || (0..500).map(|_| ids.next_ulid()).collect::<Vec<_>>())
            })
            .collect();
        let mut all: Vec<Ulid> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n);
    }

    #[test]
    fn sequential_ids_are_deterministic_and_typed() {
        let make = || SeqIds::new(Arc::new(ManualClock::new(5_000)));
        let (a, b) = (make(), make());
        for _ in 0..10 {
            assert_eq!(a.next_ulid(), b.next_ulid());
        }
        let id: SessionId = new_id(&a);
        assert_eq!(id.as_str().len(), 26);
        assert_eq!(id.as_str(), Ulid::from_parts(5_000, 11).to_string());
    }
}
