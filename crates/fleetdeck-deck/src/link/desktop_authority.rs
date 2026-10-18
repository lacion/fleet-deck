//! The deck side of the dispatch authority of `desktop.act` (R13 §5E item 1,
//! §5F, §5G item 1, §6D items 7 to 9).
//!
//! This module picks the link, makes the authority of one action and sends
//! it, and it maps the answer of the station to a [`RelayError`]. The relay
//! ([`crate::desktop_relay`]) decides who may ask and what the deck records.
//!
//! - The link is one live [`Control`] that negotiated `desktop.v1` and
//!   `desktop_authority.v1`. The hash binds the authenticated node
//!   fingerprint of that link (`ChannelParams::peer`). The `desktop_epoch` in
//!   the authority is the `peer_desktop_epoch` of the same link.
//! - The deck wait is the station deadline plus 5 s: 95 s for a wall open, a
//!   wall reload and a browser launch, and 35 s for the rest and for
//!   `desktop.info`. A deck timeout means the outcome is unknown. The deck
//!   never retries by itself.
//! - The station text never goes to an answer: the mapping reads the code
//!   (and, for the station timeout only, the fixed text) and gives a fixed
//!   [`RelayError`].

use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::issue;
use fleetdeck_proto::StationId;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopInfo, DesktopInfoQuery};
use fleetdeck_proto::desktop_authority::with_authority;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;

use super::DeckLink;
use crate::desktop_relay::RelayError;
use crate::store::{DeckStore, LinkFeature};

/// The deck wait for a wall open, a wall reload and a browser launch: the
/// station deadline (90 s) plus 5 s.
pub const LONG_WAIT: Duration = Duration::from_secs(95);
/// The deck wait for every other action and for `desktop.info`: the station
/// deadline (30 s) plus 5 s.
pub const SHORT_WAIT: Duration = Duration::from_secs(35);

/// The station text of an action that passed its deadline.
pub const STATION_ACT_TIMEOUT: &str = "The desktop action did not finish in time.";
/// The station text of an info call that passed its deadline.
pub const STATION_INFO_TIMEOUT: &str = "The desktop info did not finish in time.";

/// The two deck waits. A test makes them short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    pub long: Duration,
    pub short: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            long: LONG_WAIT,
            short: SHORT_WAIT,
        }
    }
}

impl Waits {
    /// The wait for `action`.
    #[must_use]
    pub fn of(self, action: &DesktopAction) -> Duration {
        match action {
            DesktopAction::WallOpen { .. }
            | DesktopAction::WallReload { .. }
            | DesktopAction::BrowserOpen { .. } => self.long,
            _ => self.short,
        }
    }
}

/// The live link that carries a desktop call.
pub(crate) struct Route {
    control: Control,
    /// The authenticated node fingerprint of the station on this link.
    station_node: String,
    /// The `Hello.desktop_epoch` of the station on this link.
    epoch: String,
}

impl Route {
    /// The authenticated node fingerprint of the station on this link.
    pub(crate) fn station_node(&self) -> &str {
        &self.station_node
    }

    /// True when this link negotiated `feature`.
    pub(crate) fn negotiated(&self, feature: &str) -> bool {
        self.control.params().negotiated.has(feature)
    }
}

/// Why a call failed, before the mapping to a [`RelayError`]: the capture
/// path reads the wire code itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallError {
    /// A failure of the link or a malformed answer.
    Relay(RelayError),
    /// The error answer of the station.
    Wire { code: String, message: String },
}

impl CallError {
    /// The relay error of [`map_wire`] for a wire answer.
    pub(crate) fn into_relay(self) -> RelayError {
        match self {
            Self::Relay(e) => e,
            Self::Wire { code, message } => map_wire(&code, &message),
        }
    }
}

/// Picks the link of `station` for a desktop call.
///
/// # Errors
/// [`RelayError::Unreachable`] without a live and current link;
/// [`RelayError::Unsupported`] when a live link did not negotiate both
/// features, or the station sent no `desktop_epoch`.
pub(crate) fn resolve(
    link: &DeckLink,
    store: &DeckStore,
    station: &StationId,
) -> Result<Route, RelayError> {
    for f in [feature::DESKTOP, feature::DESKTOP_AUTHORITY] {
        match store.link_feature(station, f) {
            LinkFeature::Down => return Err(RelayError::Unreachable),
            LinkFeature::Without => return Err(RelayError::Unsupported),
            LinkFeature::Negotiated => {}
        }
    }
    let control = link.control_for(station).ok_or(RelayError::Unreachable)?;
    let params = control.params();
    let negotiated = &params.negotiated;
    if !(negotiated.has(feature::DESKTOP) && negotiated.has(feature::DESKTOP_AUTHORITY)) {
        return Err(RelayError::Unsupported);
    }
    let epoch = params
        .peer_desktop_epoch
        .clone()
        .ok_or(RelayError::Unsupported)?;
    if !control.is_current() {
        return Err(RelayError::Unreachable);
    }
    let station_node = params.peer.to_string();
    Ok(Route {
        control,
        station_node,
        epoch,
    })
}

/// Makes the authority of `action` and sends `desktop.act`.
///
/// `quiet_hours` is the interval of the display of a wake; the station
/// refuses when its own local time is inside it. `action_id` is the id of the
/// authority; `None` mints a new one (only a screenshot passes its capture
/// id).
///
/// # Errors
/// See [`map_wire`]; [`RelayError::Timeout`] after the deck wait;
/// [`RelayError::Unreachable`] when the link ends.
pub(crate) async fn send_act(
    store: &DeckStore,
    route: &Route,
    action: &DesktopAction,
    quiet_hours: Option<String>,
    waits: Waits,
    action_id: Option<String>,
) -> Result<DesktopActResult, RelayError> {
    send_act_raw(store, route, action, quiet_hours, waits, action_id)
        .await
        .map_err(CallError::into_relay)
}

/// [`send_act`] with the unmapped error: the screenshot path reads the code
/// of an error answer.
pub(crate) async fn send_act_raw(
    store: &DeckStore,
    route: &Route,
    action: &DesktopAction,
    quiet_hours: Option<String>,
    waits: Waits,
    action_id: Option<String>,
) -> Result<DesktopActResult, CallError> {
    let authority = issue(
        action,
        &route.station_node,
        action_id.unwrap_or_else(|| store.new_command_id().as_str().to_owned()),
        route.epoch.clone(),
        store.now_ms(),
        quiet_hours,
    );
    let params = with_authority(action, &authority);
    request_raw(&route.control, "desktop.act", params, waits.of(action)).await
}

/// Sends `desktop.info`.
///
/// # Errors
/// As [`send_act`].
pub(crate) async fn send_info(
    route: &Route,
    fresh: bool,
    waits: Waits,
) -> Result<DesktopInfo, RelayError> {
    let params =
        serde_json::to_value(DesktopInfoQuery { fresh }).map_err(|_| RelayError::Failed)?;
    request(&route.control, "desktop.info", params, waits.short).await
}

async fn request<T: serde::de::DeserializeOwned>(
    control: &Control,
    method: &str,
    params: serde_json::Value,
    wait: Duration,
) -> Result<T, RelayError> {
    request_raw(control, method, params, wait)
        .await
        .map_err(CallError::into_relay)
}

async fn request_raw<T: serde::de::DeserializeOwned>(
    control: &Control,
    method: &str,
    params: serde_json::Value,
    wait: Duration,
) -> Result<T, CallError> {
    let outcome = tokio::time::timeout(wait, control.request(method, None, params))
        .await
        .map_err(|_| CallError::Relay(RelayError::Timeout))?
        .map_err(|_| CallError::Relay(RelayError::Unreachable))?;
    match outcome {
        // A parse error is not logged: the answer could hold desktop data.
        Outcome::Ok(v) => {
            serde_json::from_value(v).map_err(|_| CallError::Relay(RelayError::Failed))
        }
        Outcome::Err(e) => Err(CallError::Wire {
            code: e.code,
            message: e.message,
        }),
    }
}

/// The relay error of an error answer of the station or of the link layer.
///
/// `forbidden` stays ambiguous: the link scope refusal uses the same code
/// (§6D item 8). Only the two fixed timeout texts of the station turn a
/// `failed` into a timeout.
#[must_use]
pub fn map_wire(code: &str, message: &str) -> RelayError {
    match code {
        "unsupported" | "feature_not_negotiated" => RelayError::Unsupported,
        "forbidden" => RelayError::Forbidden,
        "invalid_params" => RelayError::Invalid,
        "not_found" => RelayError::NotFound,
        "conflict" => RelayError::Conflict,
        "too_many_requests" => RelayError::Overloaded,
        "timeout" => RelayError::Timeout,
        "failed" if message == STATION_ACT_TIMEOUT || message == STATION_INFO_TIMEOUT => {
            RelayError::Timeout
        }
        _ => RelayError::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waits_are_the_station_deadlines_plus_five_seconds() {
        let w = Waits::default();
        let open = DesktopAction::WallClose {
            display_id: "d".into(),
        };
        assert_eq!(w.of(&open), SHORT_WAIT);
        for long in [
            DesktopAction::WallOpen {
                display_id: "d".into(),
                monitor: "DP-1".into(),
                workspace: None,
                url: "https://h/display".into(),
            },
            DesktopAction::WallReload {
                display_id: "d".into(),
                url: "https://h/display".into(),
            },
            DesktopAction::BrowserOpen {
                url: "https://h".into(),
                monitor: None,
                session: None,
            },
        ] {
            assert_eq!(w.of(&long), LONG_WAIT);
        }
        assert_eq!(LONG_WAIT, Duration::from_secs(90 + 5));
        assert_eq!(SHORT_WAIT, Duration::from_secs(30 + 5));
        let short = Waits {
            long: Duration::from_millis(2),
            short: Duration::from_millis(1),
        };
        assert_eq!(
            short.of(&DesktopAction::ScreenLock),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn the_station_codes_map_to_fixed_relay_errors() {
        let cases = [
            ("unsupported", RelayError::Unsupported),
            ("feature_not_negotiated", RelayError::Unsupported),
            ("forbidden", RelayError::Forbidden),
            ("invalid_params", RelayError::Invalid),
            ("not_found", RelayError::NotFound),
            ("conflict", RelayError::Conflict),
            ("too_many_requests", RelayError::Overloaded),
            ("timeout", RelayError::Timeout),
            ("failed", RelayError::Failed),
            ("protocol_error", RelayError::Failed),
            ("something_new", RelayError::Failed),
        ];
        for (code, want) in cases {
            assert_eq!(map_wire(code, "x"), want, "{code}");
        }
    }

    #[test]
    fn only_the_two_station_timeout_texts_make_failed_a_timeout() {
        assert_eq!(map_wire("failed", STATION_ACT_TIMEOUT), RelayError::Timeout);
        assert_eq!(
            map_wire("failed", STATION_INFO_TIMEOUT),
            RelayError::Timeout
        );
        for text in [
            "The authorization of this desktop action expired.",
            "Quiet hours are active on this station.",
            "Too many desktop actions on this link; it reconnects.",
            "The desktop action did not finish in time",
        ] {
            assert_eq!(map_wire("failed", text), RelayError::Failed, "{text}");
        }
        // The text of another code never counts.
        assert_eq!(
            map_wire("conflict", STATION_ACT_TIMEOUT),
            RelayError::Conflict
        );
    }
}
