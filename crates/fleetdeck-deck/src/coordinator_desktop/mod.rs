//! The deck handlers of the P6 coordinator desktop methods (feature
//! `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §5, §6). Task F1 only
//! routes them: each handler refuses with `desktop_unsupported` until the deck
//! core task (T1) gives it behavior. No P6 tool works yet.

use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::{code, text};
use fleetdeck_proto::link::{Outcome, WireError};

use crate::link::DeckLink;

/// The refusal of every stub: the code and the fixed text of §11.2.
fn unsupported() -> Outcome {
    Outcome::Err(WireError::new(
        code::DESKTOP_UNSUPPORTED,
        text(code::DESKTOP_UNSUPPORTED).unwrap_or(code::DESKTOP_UNSUPPORTED),
    ))
}

/// `coordinator.desktop_call` (§5.3): a stub.
pub(crate) async fn desktop_call(
    _ctx: &DeckLink,
    _station: StationId,
    _req: &InboundRequest,
) -> Outcome {
    unsupported()
}

/// `coordinator.desktop_query` (§5.3): a stub.
pub(crate) async fn desktop_query(
    _ctx: &DeckLink,
    _station: StationId,
    _req: &InboundRequest,
) -> Outcome {
    unsupported()
}

/// `desktop.authority_check` (§5.6): a stub.
pub(crate) async fn authority_check(
    _ctx: &DeckLink,
    _station: StationId,
    _req: &InboundRequest,
) -> Outcome {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_has_the_code_and_the_fixed_text() {
        let Outcome::Err(e) = unsupported() else {
            panic!("a stub must refuse");
        };
        assert_eq!(e.code, "desktop_unsupported");
        assert_eq!(e.message, "This station's build has no desktop control.");
    }
}
