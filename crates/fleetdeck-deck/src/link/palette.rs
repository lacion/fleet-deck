//! `station.palette` (R13 P4, feature `desktop_shell.v1`): a station sends
//! the Omarchy theme palette of its desktop to the deck.
//!
//! Task A routes the method here. Until task F1 builds the handler, every
//! call is answered with the wire error `unsupported`.

use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, WireError};

/// The text of the stub answer.
pub(super) const UNSUPPORTED: &str = "The deck does not serve station.palette in this build.";

/// `station.palette`: a stub that answers `unsupported`.
pub(super) fn station_palette(_station: &StationId, _params: serde_json::Value) -> Outcome {
    Outcome::Err(WireError::new("unsupported", UNSUPPORTED))
}
