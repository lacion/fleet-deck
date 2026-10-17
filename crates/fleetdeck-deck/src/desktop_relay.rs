//! The deck relay of desktop actions (R13 §6D items 6 to 10): the interface
//! that the owner routes ([`crate::http::desktop_routes`]) and the wake
//! ([`crate::wake`]) share.
//!
//! STEP 1 STUB. [`act`] and [`info`] answer [`RelayError::Unsupported`]
//! (`409 desktop_unsupported`) until the P3b relay task replaces the bodies.
//! The relay task builds, in `link/desktop_authority.rs`, the authority of
//! each action (`fleetdeck_link::desktop_authority::issue`, the station node
//! fingerprint of the live link, the `peer_desktop_epoch` of
//! `Control::params()`), checks `desktop_control` live before it makes the
//! authority, relays only to a station that negotiated `desktop_authority.v1`,
//! applies the deadlines of §6D item 9, and orchestrates the binding with
//! `DeckStore::display_begin_open` and `display_commit_binding`.
//!
//! The messages of [`RelayError::message`] are fixed and redacted: they hold
//! no URL, no notification text and no request content, and they never claim
//! that `forbidden` means `allow_desktop = false` (§6D item 8).

use std::sync::Arc;

use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopInfo};

use crate::link::DeckLink;
use crate::live::LiveCell;
use crate::store::DeckStore;

/// Who asks for an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The owner, from the board (audit action `desktop.<action>`, actor
    /// `owner`).
    Owner,
    /// The wake rule of a bound display (audit action `desktop.wake`, actor
    /// `wake`).
    Wake { display_id: String },
}

/// What the relay uses.
#[derive(Debug, Clone)]
pub struct RelayCtx {
    pub store: DeckStore,
    pub link: Arc<DeckLink>,
    /// The live settings: `desktop_control` is read here at each decision.
    pub live: LiveCell,
}

/// Why the relay refused or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayError {
    /// The station has no desktop control, or did not negotiate
    /// `desktop.v1` and `desktop_authority.v1`.
    Unsupported,
    /// The station answered `forbidden`. The cause is ambiguous: the link
    /// scope refusal uses the same code.
    Forbidden,
    /// Invalid parameters.
    Invalid,
    /// A missing target.
    NotFound,
    /// A conflict (a bound display, a superseded launch).
    Conflict,
    /// A failed or malformed answer.
    Failed,
    /// An overload.
    Overloaded,
    /// A timeout. The action may have run on the station.
    Timeout,
    /// No live link to the station.
    Unreachable,
    /// The deck setting `desktop_control` is off.
    ControlOff,
}

impl RelayError {
    /// The HTTP status and error code (§6D item 8).
    #[must_use]
    pub fn http(self) -> (StatusCode, &'static str) {
        match self {
            Self::Unsupported => (StatusCode::CONFLICT, "desktop_unsupported"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "desktop_forbidden"),
            Self::Invalid => (StatusCode::BAD_REQUEST, "invalid_desktop_action"),
            Self::NotFound => (StatusCode::NOT_FOUND, "desktop_not_found"),
            Self::Conflict => (StatusCode::CONFLICT, "desktop_conflict"),
            Self::Failed => (StatusCode::BAD_GATEWAY, "desktop_failed"),
            Self::Overloaded => (StatusCode::SERVICE_UNAVAILABLE, "desktop_overloaded"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, "desktop_timeout"),
            Self::Unreachable => (StatusCode::CONFLICT, "station_unreachable"),
            Self::ControlOff => (StatusCode::CONFLICT, "desktop_control_off"),
        }
    }

    /// The fixed, redacted text.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Unsupported => "This station's build has no desktop control.",
            Self::Forbidden => "The station refused the desktop action.",
            Self::Invalid => "The desktop action is not valid.",
            Self::NotFound => "The target of the desktop action was not found.",
            Self::Conflict => "The desktop action conflicts with the current state.",
            Self::Failed => "The desktop action failed.",
            Self::Overloaded => "The station is busy; try again later.",
            Self::Timeout => "The station did not answer in time; the action may have run.",
            Self::Unreachable => "The station is not connected.",
            Self::ControlOff => "Desktop control is off on this deck.",
        }
    }
}

/// Runs one desktop action on `station`.
///
/// # Errors
/// [`RelayError`]; the step 1 stub always answers [`RelayError::Unsupported`].
pub async fn act(
    _ctx: &RelayCtx,
    _station: StationId,
    _action: DesktopAction,
    _actor: Actor,
) -> Result<DesktopActResult, RelayError> {
    Err(RelayError::Unsupported)
}

/// Reads the desktop info of `station`; `fresh` bypasses the station cache
/// of 10 seconds (the wake rule asks for it).
///
/// # Errors
/// [`RelayError`]; the step 1 stub always answers [`RelayError::Unsupported`].
pub async fn info(
    _ctx: &RelayCtx,
    _station: StationId,
    _fresh: bool,
) -> Result<DesktopInfo, RelayError> {
    Err(RelayError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [RelayError; 10] = [
        RelayError::Unsupported,
        RelayError::Forbidden,
        RelayError::Invalid,
        RelayError::NotFound,
        RelayError::Conflict,
        RelayError::Failed,
        RelayError::Overloaded,
        RelayError::Timeout,
        RelayError::Unreachable,
        RelayError::ControlOff,
    ];

    #[test]
    fn the_http_mapping_follows_the_design() {
        let status = |e: RelayError| e.http().0.as_u16();
        assert_eq!(
            RelayError::Unsupported.http(),
            (StatusCode::CONFLICT, "desktop_unsupported")
        );
        assert_eq!(
            RelayError::Forbidden.http(),
            (StatusCode::FORBIDDEN, "desktop_forbidden")
        );
        assert_eq!(
            RelayError::Unreachable.http(),
            (StatusCode::CONFLICT, "station_unreachable")
        );
        assert_eq!(
            RelayError::ControlOff.http(),
            (StatusCode::CONFLICT, "desktop_control_off")
        );
        assert_eq!(status(RelayError::Invalid), 400);
        assert_eq!(status(RelayError::NotFound), 404);
        assert_eq!(status(RelayError::Conflict), 409);
        assert_eq!(status(RelayError::Failed), 502);
        assert_eq!(status(RelayError::Overloaded), 503);
        assert_eq!(status(RelayError::Timeout), 504);
    }

    #[test]
    fn the_codes_are_unique_and_the_messages_are_fixed_and_do_not_blame_a_setting() {
        let mut codes: Vec<&str> = ALL.iter().map(|e| e.http().1).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ALL.len());
        for e in ALL {
            let m = e.message();
            assert!(!m.contains("allow_desktop"), "{m}");
            assert!(!m.contains("http"), "{m}");
        }
    }
}
