//! The read permits of the coordinator desktop tools (R13 P6 §5.7): the deck
//! registers one bounded permit before it sends `desktop.coordinator_read`;
//! the target asks `desktop.authority_check` (phase `read`) before it returns
//! data. A permit allows one check and one answer: the first check takes it
//! out of the registry, whatever the answer is.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use fleetdeck_link::desktop_authority::read_hash;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorContext, ProtectedRead};
use ring::rand::{SecureRandom, SystemRandom};

use super::limits::{DeckSlot, Slots};
use crate::store::coordinator_desktop::OpClass;

/// What the deck knows about one read that it sent.
#[derive(Debug)]
pub(crate) struct Permit {
    pub(super) ctx: CoordinatorContext,
    pub(super) target: StationId,
    pub(super) station_node: String,
    pub(super) link_epoch: String,
    pub(super) class: OpClass,
    pub(super) read: ProtectedRead,
    pub(super) deadline: Instant,
    /// Keeps one slot of the shared deck counter while the permit lives.
    _slot: DeckSlot,
}

/// The registry of the live permits (at most
/// `limits::DECK_CALLS_IN_FLIGHT`, shared with the calls).
#[derive(Debug)]
pub(crate) struct Permits {
    slots: Arc<Slots>,
    map: Mutex<HashMap<String, Permit>>,
}

/// The read of a permit, as the target must send it back.
pub(crate) struct Registered {
    pub(crate) read_id: String,
    pub(crate) read_hash: String,
}

/// 128 random bits in lowercase hex (32 characters).
fn new_read_id() -> Option<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Permits {
    pub(crate) fn new(slots: Arc<Slots>) -> Self {
        Self {
            slots,
            map: Mutex::default(),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Permit>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers the permit of one read. `Err("busy")` when no slot is free;
    /// `Err("authority_unknown")` when no random id can be made.
    pub(crate) fn register(
        &self,
        ctx: CoordinatorContext,
        (target, station_node, link_epoch): (StationId, String, String),
        class: OpClass,
        read: ProtectedRead,
        deadline: Instant,
    ) -> Result<Registered, &'static str> {
        let slot = self.slots.take_deck().ok_or("busy")?;
        let read_id = new_read_id().ok_or("authority_unknown")?;
        let hash = read_hash(&ctx, &link_epoch, &read, &read_id, &station_node);
        self.map().insert(
            read_id.clone(),
            Permit {
                ctx,
                target,
                station_node,
                link_epoch,
                class,
                read,
                deadline,
                _slot: slot,
            },
        );
        Ok(Registered {
            read_id,
            read_hash: hash,
        })
    }

    /// Removes the permit of `read_id` (completion, error, cancellation).
    pub(crate) fn remove(&self, read_id: &str) {
        self.map().remove(read_id);
    }

    /// Takes the permit out of the registry: one check, one answer.
    pub(super) fn take(&self, read_id: &str) -> Option<Permit> {
        self.map().remove(read_id)
    }

    /// The live permits (tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map().len()
    }
}

/// What the target sent to `desktop.authority_check` with phase `read`, and
/// the connection that carried it.
pub(super) struct Arrival<'a> {
    /// The authenticated station of the link.
    pub station: &'a StationId,
    /// The node fingerprint of that station.
    pub station_node: &'a str,
    /// The `peer_desktop_epoch` of the arrival connection.
    pub epoch: Option<&'a str>,
    pub read: &'a ProtectedRead,
    /// The `action_hash` field of the check.
    pub hash: &'a str,
}

impl Permit {
    /// The five checks of §5.7 after the permit exists (the first one, that
    /// it exists and is not used, is the `take`): not expired, the asking
    /// station is the target, the arrival connection is the one that carried
    /// the read, the received read is byte-equal to the permit's, and the
    /// recomputed hash equals the received one.
    pub(super) fn matches(&self, got: &Arrival<'_>, read_id: &str, now: Instant) -> bool {
        if now > self.deadline {
            return false;
        }
        if *got.station != self.target || got.station_node != self.station_node {
            return false;
        }
        if got.epoch != Some(self.link_epoch.as_str()) {
            return false;
        }
        // Canonical bytes: `serde_json` writes the keys of a `Value` sorted.
        let bytes = |r: &ProtectedRead| {
            serde_json::to_value(r)
                .ok()
                .and_then(|v| serde_json::to_vec(&v).ok())
        };
        match (bytes(got.read), bytes(&self.read)) {
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
        let hash = read_hash(
            &self.ctx,
            &self.link_epoch,
            &self.read,
            read_id,
            got.station_node,
        );
        hash == got.hash
    }
}

/// Removes a permit when it is dropped: completion, error and cancellation
/// of the read all remove it.
pub(crate) struct PermitGuard<'a> {
    pub(crate) permits: &'a Permits,
    pub(crate) read_id: String,
}

impl Drop for PermitGuard<'_> {
    fn drop(&mut self) {
        self.permits.remove(&self.read_id);
    }
}

#[cfg(test)]
#[path = "permits_tests.rs"]
mod tests;
