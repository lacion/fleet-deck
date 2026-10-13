//! The live settings of the deck (DL-172, design 1.4): the fields that apply
//! while the process runs. The board reads them per use; a save builds a new
//! [`DeckLive`] (prepare) and swaps the pointer (publish, which cannot fail).

use std::sync::{Arc, PoisonError, RwLock};

use fleetdeck_core::config::DeckConfig;

/// The live fields of `deck.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckLive {
    /// The name of this deck.
    pub name: String,
    /// Tell both sessions once when two sessions change the same file.
    pub conflict_mail: bool,
}

impl DeckLive {
    /// The live fields of a parsed `deck.toml`.
    #[must_use]
    pub fn of(config: &DeckConfig) -> Self {
        Self {
            name: config.name.clone(),
            conflict_mail: config.conflict_mail,
        }
    }
}

/// The current [`DeckLive`]: readers clone the `Arc`, a publish swaps it.
#[derive(Debug)]
pub struct LiveCell(RwLock<Arc<DeckLive>>);

impl LiveCell {
    #[must_use]
    pub fn new(live: DeckLive) -> Self {
        Self(RwLock::new(Arc::new(live)))
    }

    /// The current value.
    #[must_use]
    pub fn get(&self) -> Arc<DeckLive> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Publishes a prepared value.
    pub fn publish(&self, live: DeckLive) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(live);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_publish_swaps_the_value_and_an_old_reader_keeps_its_own() {
        let cell = LiveCell::new(DeckLive {
            name: "a".into(),
            conflict_mail: true,
        });
        let before = cell.get();
        cell.publish(DeckLive {
            name: "b".into(),
            conflict_mail: false,
        });
        assert_eq!(before.name, "a");
        let now = cell.get();
        assert_eq!((now.name.as_str(), now.conflict_mail), ("b", false));
    }
}
