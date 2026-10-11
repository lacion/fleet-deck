//! Fleet Deck v2 station (PLAN §6): the session host, the event log with
//! command receipts and effects, the workspace service, and the local policy.

/// The session host boundary (PLAN §6.1, A9). The trait and the MVP host live
/// in `fleetdeck-providers`, because the adapter drivers use them (DL-031);
/// the station chooses the host.
pub mod host {
    pub use fleetdeck_providers::host::*;
}
