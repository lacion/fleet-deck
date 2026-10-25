//! The in-memory in-flight slots of the coordinator desktop tools (R13 P6
//! §8.2): one shared counter of [`limits::DECK_CALLS_IN_FLIGHT`] for the P6
//! calls and the read permits of the whole deck, and at most
//! [`limits::MUTATIONS_IN_FLIGHT`] mutations of one grant. An image read takes
//! one shared slot, one of [`limits::IMAGES_IN_FLIGHT`] for its grant and one
//! of [`limits::DECK_ASSEMBLIES_IN_FLIGHT`] on the deck. A slot is a guard:
//! it is released when it is dropped, so every path releases it. Nothing here
//! is stored: a restart starts with no slot in use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_proto::coordinator_desktop_text::limits;

#[derive(Debug, Default)]
struct Counts {
    deck: u32,
    grants: HashMap<String, u32>,
    /// The image assemblies in flight on the deck.
    assemblies: u32,
    /// The image reads in flight of each grant.
    images: HashMap<String, u32>,
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

    /// The slots of one image read (§8.2): a shared deck slot, one of the
    /// grant's image slots and one deck assembly. `None` when any limit is
    /// reached (nothing is taken then).
    pub(crate) fn take_image(self: &Arc<Self>, grant_id: &str) -> Option<ImageSlot> {
        let mut counts = self.counts();
        let used = counts.images.get(grant_id).copied().unwrap_or(0);
        if counts.deck >= limits::DECK_CALLS_IN_FLIGHT
            || used >= limits::IMAGES_IN_FLIGHT
            || counts.assemblies >= limits::DECK_ASSEMBLIES_IN_FLIGHT
        {
            return None;
        }
        counts.deck += 1;
        counts.assemblies += 1;
        counts.images.insert(grant_id.to_owned(), used + 1);
        Some(ImageSlot {
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

/// The slots of one image read: released when it is dropped.
#[derive(Debug)]
pub(crate) struct ImageSlot {
    slots: Arc<Slots>,
    grant_id: String,
}

impl Drop for ImageSlot {
    fn drop(&mut self) {
        let mut counts = self.slots.counts();
        counts.deck = counts.deck.saturating_sub(1);
        counts.assemblies = counts.assemblies.saturating_sub(1);
        if let Some(n) = counts.images.get_mut(&self.grant_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.images.remove(&self.grant_id);
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

    #[test]
    fn an_image_read_takes_a_grant_slot_a_deck_assembly_and_a_shared_slot() {
        let slots = Arc::new(Slots::default());
        let one = slots.take_image("g1").expect("first");
        assert!(slots.take_image("g1").is_none(), "one image for a grant");
        assert_eq!(slots.in_use(), 1, "the shared slot is taken");
        // Four assemblies fill the deck.
        let more: Vec<_> = (2..=4)
            .map(|i| slots.take_image(&format!("g{i}")).expect("a slot"))
            .collect();
        assert!(slots.take_image("g5").is_none(), "the fifth assembly");
        assert_eq!(slots.in_use(), 4);
        drop(one);
        let again = slots.take_image("g1").expect("released");
        drop((more, again));
        assert_eq!(slots.in_use(), 0);
        assert!(slots.counts().images.is_empty(), "no leftover entry");
        assert_eq!(slots.counts().assemblies, 0);
    }

    #[test]
    fn an_image_read_needs_a_free_shared_slot() {
        let slots = Arc::new(Slots::default());
        let held: Vec<_> = (0..limits::DECK_CALLS_IN_FLIGHT)
            .map(|_| slots.take_deck().expect("a slot"))
            .collect();
        assert!(slots.take_image("g1").is_none());
        assert_eq!(slots.counts().assemblies, 0, "nothing is taken");
        drop(held);
        assert!(slots.take_image("g1").is_some());
    }

    #[allow(dead_code)]
    enum Held {
        Call(CallSlot),
        Deck(DeckSlot),
    }
}
