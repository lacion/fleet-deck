//! `display.directive` (R13 P4, feature `desktop_shell.v1`): a station asks
//! the deck for a wall directive (the hotkeys of the Omarchy desktop shell).
//!
//! Task A routes the method here. Until task E builds the handler, every
//! call is answered with the wire error `unsupported`.

use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, WireError};

/// The text of the stub answer.
pub(super) const UNSUPPORTED: &str = "The deck does not serve display.directive in this build.";

/// `display.directive`: a stub that answers `unsupported`.
pub(super) fn display_directive(_station: &StationId, _params: serde_json::Value) -> Outcome {
    Outcome::Err(WireError::new("unsupported", UNSUPPORTED))
}
