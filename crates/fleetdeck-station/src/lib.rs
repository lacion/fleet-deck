//! Fleet Deck v2 station (PLAN §6): the session host, the event log with
//! command receipts and effects, the workspace service, and the local policy.

/// The session host boundary (PLAN §6.1, A9). The trait and the MVP host live
/// in `fleetdeck-providers`, because the adapter drivers use them (DL-031);
/// the station chooses the host.
pub mod host {
    pub use fleetdeck_providers::host::*;
}

/// Crash injection at the effect boundaries (M2 P2-8).
/// The coordinator MCP endpoint (M3 6-5, ADR-006).
pub mod coordinator;

pub mod fault;

/// The station side of a deck link (M2 P3-5).
pub mod link;

/// The station-local control socket (M2 P2-7).
pub mod local;

/// The session manager (PLAN §6.4, M2 P2-6).
pub mod manager;

/// `fleetdeck station serve` (M2 P2-7).
pub mod serve;

pub mod store;

/// The effect worker (PLAN §6.4).
pub mod worker;

/// The workspace service (PLAN §6.6): checkpoints, diffs, restore.
pub mod workspace;
