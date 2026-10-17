//! The live settings of the deck (DL-172, design 1.4): the fields that apply
//! while the process runs. The board reads them per use; a save builds a new
//! [`DeckLive`] (prepare) and swaps the pointer (publish, which cannot fail).

use std::sync::{Arc, PoisonError, RwLock};

use fleetdeck_core::config::DeckConfig;
use tokio::sync::Notify;

/// The live fields of `deck.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckLive {
    /// The name of this deck.
    pub name: String,
    /// Tell both sessions once when two sessions change the same file.
    pub conflict_mail: bool,
    /// The deck setting `fleet_control` (ADR-008 §3).
    pub fleet_control: bool,
    /// The deck setting `desktop_control` (R13 §6.4): read live at each
    /// decision; there is no copy in the store.
    pub desktop_control: bool,
    /// The deck setting `tailscale_cert` (DL-190).
    pub tailscale_cert: bool,
    /// The deck setting `tailscale_bin` (DL-190): the program of the certificate task.
    pub tailscale_bin: Option<std::path::PathBuf>,
}

impl DeckLive {
    /// The live fields of a parsed `deck.toml`.
    #[must_use]
    pub fn of(config: &DeckConfig) -> Self {
        Self {
            name: config.name.clone(),
            conflict_mail: config.conflict_mail,
            fleet_control: config.fleet_control,
            desktop_control: config.desktop_control,
            tailscale_cert: config.tailscale_cert,
            tailscale_bin: config.tailscale_bin.clone(),
        }
    }
}

/// The current [`DeckLive`]: readers clone the `Arc`, a publish swaps it. A
/// clone of the cell shares the value (the local socket publishes too). A
/// publish also wakes the tasks that wait on [`Self::changed`].
#[derive(Debug, Clone)]
pub struct LiveCell {
    value: Arc<RwLock<Arc<DeckLive>>>,
    changed: Arc<Notify>,
}

impl LiveCell {
    #[must_use]
    pub fn new(live: DeckLive) -> Self {
        Self {
            value: Arc::new(RwLock::new(Arc::new(live))),
            changed: Arc::new(Notify::new()),
        }
    }

    /// Notified by each publish (one stored permit if nobody waits).
    #[must_use]
    pub fn changed(&self) -> Arc<Notify> {
        Arc::clone(&self.changed)
    }

    /// The current value.
    #[must_use]
    pub fn get(&self) -> Arc<DeckLive> {
        Arc::clone(&self.value.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Publishes a prepared value.
    pub fn publish(&self, live: DeckLive) {
        *self.value.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(live);
        self.changed.notify_one();
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
            fleet_control: false,
            desktop_control: false,
            tailscale_cert: false,
            tailscale_bin: None,
        });
        let before = cell.get();
        cell.publish(DeckLive {
            name: "b".into(),
            conflict_mail: false,
            fleet_control: true,
            desktop_control: false,
            tailscale_cert: true,
            tailscale_bin: None,
        });
        assert_eq!(before.name, "a");
        let now = cell.get();
        assert_eq!((now.name.as_str(), now.conflict_mail), ("b", false));
        assert!(now.fleet_control && !before.fleet_control);
    }
}
