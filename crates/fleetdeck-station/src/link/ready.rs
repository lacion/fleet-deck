//! Connection-local link readiness (R13 P8 T2, design `omarchy-p8.md` 2.1).
//!
//! A link is *ready* when the deck acknowledged the first snapshot of this
//! connection (an `ack` at or above its watermark; an empty snapshot has
//! watermark 0), or when the stream really replays after an accepted
//! `events.replay` of this connection (a cursor that the stream turns into a
//! snapshot does not count). The state belongs to one connection: [`ReadyGuard`] removes
//! it when the link ends. The coordinator acknowledgement watermark cannot
//! serve: it does not tell "no acknowledgement" from the acknowledgement of
//! sequence zero.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleetdeck_core::identity::Fingerprint;

/// The live links of this station and whether each one is ready.
#[derive(Debug, Default)]
pub struct LinkReady {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    next: u64,
    /// Per-link id: the peer fingerprint and the readiness.
    links: BTreeMap<u64, (Fingerprint, bool)>,
}

impl LinkReady {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a live link of `peer`, not ready. The guard removes it.
    #[must_use]
    pub fn open(self: &Arc<Self>, peer: Fingerprint) -> ReadyGuard {
        let mut state = self.lock();
        let id = state.next;
        state.next += 1;
        state.links.insert(id, (peer, false));
        ReadyGuard {
            ready: Arc::clone(self),
            id,
        }
    }

    /// True when some live link of `peer` is ready.
    #[must_use]
    pub fn is_up(&self, peer: &Fingerprint) -> bool {
        self.lock()
            .links
            .values()
            .any(|(fingerprint, ready)| *ready && fingerprint == peer)
    }
}

/// One registered link. Dropping it clears the readiness of the link.
#[derive(Debug)]
pub struct ReadyGuard {
    ready: Arc<LinkReady>,
    id: u64,
}

/// A clonable handle to the readiness flag of one link. It does not keep the
/// link alive: after the guard dropped, a call changes nothing.
#[derive(Debug, Clone)]
pub struct ReadyHandle {
    ready: Arc<LinkReady>,
    id: u64,
}

impl ReadyHandle {
    /// Marks the link ready.
    pub fn mark_ready(&self) {
        if let Some(link) = self.ready.lock().links.get_mut(&self.id) {
            link.1 = true;
        }
    }
}

impl ReadyGuard {
    /// A handle to the readiness flag of this link.
    #[must_use]
    pub fn handle(&self) -> ReadyHandle {
        ReadyHandle {
            ready: Arc::clone(&self.ready),
            id: self.id,
        }
    }

    /// Marks this link ready.
    pub fn mark_ready(&self) {
        self.handle().mark_ready();
    }

    /// An `ack` of `station_seq` was accepted. `first_snapshot` is the
    /// watermark of the first snapshot sent on this link, if one went out:
    /// the link is ready when the ack is at or above it.
    pub fn acked(&self, station_seq: u64, first_snapshot: Option<u64>) {
        if first_snapshot.is_some_and(|watermark| station_seq >= watermark) {
            self.mark_ready();
        }
    }
}

impl Drop for ReadyGuard {
    fn drop(&mut self) {
        self.ready.lock().links.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> Fingerprint {
        format!("{byte:02x}").repeat(32).parse().unwrap()
    }

    #[test]
    fn a_new_link_is_not_ready() {
        let ready = Arc::new(LinkReady::default());
        let _guard = ready.open(fp(1));
        assert!(!ready.is_up(&fp(1)));
    }

    #[test]
    fn an_ack_at_or_above_the_first_watermark_makes_the_link_ready() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        // An ack below the watermark does not count.
        guard.acked(6, Some(7));
        assert!(!ready.is_up(&fp(1)));
        // An ack before any snapshot went out does not count.
        guard.acked(100, None);
        assert!(!ready.is_up(&fp(1)));
        guard.acked(7, Some(7));
        assert!(ready.is_up(&fp(1)));
    }

    #[test]
    fn an_ack_above_the_first_watermark_counts() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        guard.acked(9, Some(7));
        assert!(ready.is_up(&fp(1)));
    }

    #[test]
    fn an_empty_snapshot_with_an_ack_of_zero_counts() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        guard.acked(0, Some(0));
        assert!(ready.is_up(&fp(1)));
    }

    #[test]
    fn an_accepted_replay_makes_the_link_ready() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        guard.mark_ready();
        assert!(ready.is_up(&fp(1)));
    }

    #[test]
    fn a_handle_marks_the_same_link_ready_and_cannot_outlive_it() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        let handle = guard.handle();
        let clone = handle.clone();
        assert!(!ready.is_up(&fp(1)));
        clone.mark_ready();
        assert!(ready.is_up(&fp(1)));
        drop(guard);
        assert!(!ready.is_up(&fp(1)));
        // A late call after the guard dropped does not bring the link back.
        handle.mark_ready();
        assert!(!ready.is_up(&fp(1)));
    }

    #[test]
    fn dropping_the_guard_clears_the_readiness() {
        let ready = Arc::new(LinkReady::default());
        let guard = ready.open(fp(1));
        guard.mark_ready();
        assert!(ready.is_up(&fp(1)));
        drop(guard);
        assert!(!ready.is_up(&fp(1)));
    }

    #[test]
    fn a_link_of_another_peer_does_not_count() {
        let ready = Arc::new(LinkReady::default());
        let other = ready.open(fp(2));
        other.mark_ready();
        assert!(!ready.is_up(&fp(1)));
        assert!(ready.is_up(&fp(2)));
    }

    #[test]
    fn one_ready_link_of_the_peer_is_enough_and_ends_with_its_guard() {
        let ready = Arc::new(LinkReady::default());
        let old = ready.open(fp(1));
        let new = ready.open(fp(1));
        new.mark_ready();
        assert!(ready.is_up(&fp(1)));
        // The other link of the peer ends: the ready one stays.
        drop(old);
        assert!(ready.is_up(&fp(1)));
        drop(new);
        assert!(!ready.is_up(&fp(1)));
    }
}
