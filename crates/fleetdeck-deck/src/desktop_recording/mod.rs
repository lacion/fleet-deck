//! The owner recording on the deck (R13 P5b, design `omarchy-p5b.md`).
//! Task F: only the stub of `desktop.capture_lease`; the start, stop, active
//! discovery, policy and lease bodies follow in task K1.

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::text::RECORDING_UNSUPPORTED;
use fleetdeck_proto::coordinator_desktop_text::code::DESKTOP_UNSUPPORTED;
use fleetdeck_proto::link::{Outcome, WireError};

use crate::link::DeckLink;

/// `desktop.capture_lease` of a station: answers the wire code
/// `desktop_unsupported` until task K1.
#[allow(clippy::unused_async, reason = "a stub until task K1")]
pub(crate) async fn lease(
    _ctx: &DeckLink,
    _control: &Control,
    _station: StationId,
    _req: &InboundRequest,
) -> Outcome {
    Outcome::Err(WireError::new(DESKTOP_UNSUPPORTED, RECORDING_UNSUPPORTED))
}
