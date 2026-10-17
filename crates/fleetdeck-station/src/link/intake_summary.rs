//! `fleet.summary` (R13 P4, feature `desktop_shell.v1`): the fleet summary
//! that the deck sends to this station.
//!
//! Task A routes the method here. Until task B2 builds the intake, every
//! call is answered with the wire error `unsupported`.

use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;

/// The text of the stub answer.
pub(super) const UNSUPPORTED: &str = "The fleet summary is not served by this station build.";

/// `fleet.summary`: a stub that answers `unsupported`.
pub(super) fn fleet_summary(_ctx: &StationLink, _params: serde_json::Value) -> Outcome {
    Outcome::Err(WireError::new("unsupported", UNSUPPORTED))
}
