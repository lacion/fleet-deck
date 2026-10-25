//! The `allow_fleet_control` key of `station.toml` (ADR-008 §3): whether
//! coordinator sessions with a fleet-control grant may act on the sessions
//! of this station. The default is off. The station reads the live value
//! at intake, at the decision and at the dispatch of each action.

/// The value of `allow_fleet_control`; an absent key is off.
pub(super) fn parse(raw: Option<bool>) -> bool {
    raw.unwrap_or(false)
}
