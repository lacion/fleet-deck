//! The in-memory in-flight slots of the coordinator desktop tools (R13 P6
//! §8.2): one shared counter of [`limits::DECK_CALLS_IN_FLIGHT`] for the P6
//! calls and the read permits of the whole deck, and at most
//! [`limits::MUTATIONS_IN_FLIGHT`] mutations of one grant. A slot is a guard:
//! it is released when it is dropped, so every path releases it. Nothing here
//! is stored: a restart starts with no slot in use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_proto::coordinator_desktop_text::limits;

#[derive(Debug, Default)]
struct Counts {
    deck: u32,
    grants: HashMap<String, u32>,
}

/// The slot counters. Share them through an `Arc`.
#[derive(Debug, Default)]
pub(crate) struct Slots {
    counts: Mutex<Counts>,
}

impl Slots {
    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One deck-wide slot, or `None` when all are in use.
    pub(crate) fn take_deck(self: &Arc<Self>) -> Option<DeckSlot> {
        let mut counts = self.counts();
        if counts.deck >= limits::DECK_CALLS_IN_FLIGHT {
            return None;
        }
        counts.deck += 1;
        Some(DeckSlot {
            slots: Arc::clone(self),
        })
    }

    /// One deck-wide slot and one slot of `grant_id`, or `None` when either
    /// limit is reached (nothing is taken then).
    pub(crate) fn take_call(self: &Arc<Self>, grant_id: &str) -> Option<CallSlot> {
        let mut counts = self.counts();
        let used = counts.grants.get(grant_id).copied().unwrap_or(0);
        if counts.deck >= limits::DECK_CALLS_IN_FLIGHT || used >= limits::MUTATIONS_IN_FLIGHT {
            return None;
        }
        counts.deck += 1;
        counts.grants.insert(grant_id.to_owned(), used + 1);
        Some(CallSlot {
            slots: Arc::clone(self),
            grant_id: grant_id.to_owned(),
        })
    }

    /// The deck-wide slots in use (tests).
    #[cfg(test)]
    pub(crate) fn in_use(&self) -> u32 {
        self.counts().deck
    }
}

/// A deck-wide slot: held by a read permit.
#[derive(Debug)]
pub(crate) struct DeckSlot {
    slots: Arc<Slots>,
}

impl Drop for DeckSlot {
    fn drop(&mut self) {
        let mut counts = self.slots.counts();
        counts.deck = counts.deck.saturating_sub(1);
    }
}

/// A deck-wide slot and a slot of one grant: held by one mutation.
#[derive(Debug)]
pub(crate) struct CallSlot {
    slots: Arc<Slots>,
    grant_id: String,
}

impl Drop for CallSlot {
    fn drop(&mut self) {
        let mut counts = self.slots.counts();
        counts.deck = counts.deck.saturating_sub(1);
        if let Some(n) = counts.grants.get_mut(&self.grant_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.grants.remove(&self.grant_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_has_two_mutation_slots_and_a_drop_releases_them() {
        let slots = Arc::new(Slots::default());
        let one = slots.take_call("g1").expect("first");
        let two = slots.take_call("g1").expect("second");
        assert!(slots.take_call("g1").is_none(), "a third is refused");
        // Another grant is not affected.
        let other = slots.take_call("g2").expect("another grant");
        assert_eq!(slots.in_use(), 3);
        drop(one);
        assert_eq!(slots.in_use(), 2);
        let again = slots.take_call("g1").expect("released");
        drop((two, other, again));
        assert_eq!(slots.in_use(), 0);
        assert!(slots.counts().grants.is_empty(), "no leftover entry");
    }

    #[test]
    fn calls_and_permits_share_the_deck_counter() {
        let slots = Arc::new(Slots::default());
        let mut held = Vec::new();
        for i in 0..limits::DECK_CALLS_IN_FLIGHT {
            // Calls of many grants and permits fill the same 64.
            if i % 2 == 0 {
                held.push(Held::Call(
                    slots.take_call(&format!("g{i}")).expect("a slot"),
                ));
            } else {
                held.push(Held::Deck(slots.take_deck().expect("a slot")));
            }
        }
        assert!(slots.take_deck().is_none(), "the 65th permit");
        assert!(slots.take_call("fresh").is_none(), "the 65th call");
        assert_eq!(slots.in_use(), 64);
        held.pop();
        assert!(slots.take_deck().is_some());
    }

    #[allow(dead_code)]
    enum Held {
        Call(CallSlot),
        Deck(DeckSlot),
    }
}
