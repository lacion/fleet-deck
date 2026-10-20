//! Fleet Deck v2 station (PLAN §6): the session host, the event log with
//! command receipts and effects, the workspace service, and the local policy.

/// The session host boundary (PLAN §6.1, A9). The trait and the MVP host live
/// in `fleetdeck-providers`, because the adapter drivers use them (DL-031);
/// the station chooses the host.
pub mod host {
    pub use fleetdeck_providers::host::*;
}

/// The connected GitHub, GitLab, Jira and Linear accounts (DL-173).
pub mod accounts;
/// Artifacts: versioned files that a session or a person publishes (R12).
pub mod artifacts;
/// Model help on the station (R10, DL-167).
pub mod assist;

/// Crash injection at the effect boundaries (M2 P2-8).
/// The coordinator MCP endpoint (M3 6-5, ADR-006).
pub mod coordinator;

pub mod fault;

/// The config drift report (FG-11).
pub mod drift;

/// The runtime state of Fleet Git: traits, config digest, deck control (FG-5, FG-6, FG-11).
pub mod fleet_git_runtime;

/// The artifact share of a project: the authorization, the folder and the
/// variables of a session (FG-12).
pub mod artifact_share;

/// The session history from the captured transcript (DL-189).
pub mod history;

/// The fleet skill tree that every provider session gets (MCP P3 §4).
pub mod fleet_skill;

/// The last fleet summary of the deck, in memory (R13 P4).
pub mod fleet_summary;

/// GitHub, GitLab, Jira and Linear (R3, DL-164).
pub mod integrations;

/// Desktop control: Hyprland and Omarchy (R13 §6, DL-201).
pub mod desktop;

/// The reads of the desktop tools and the install plan (BG-1).
pub mod desktop_tools;

/// The station side of a deck link (M2 P3-5).
pub mod link;

/// The station-local control socket (M2 P2-7).
pub mod local;

/// The local socket op `station.fingerprint` (R13 P8).
pub mod local_fingerprint;

/// The local socket op `palette_set` (R13 P4 task F1).
pub mod local_palette;

/// The local socket op `fleet_summary` (R13 P4).
pub mod local_summary;

/// The local socket op `wall_directive` (R13 P4 task E).
pub mod local_wall;

/// The SHA-256 of the running binary (R13 P8).
pub mod binary_hash;

/// Structural validation of recorded media files (R13 P5b).
pub mod media;

/// The maintenance runner of the desktop tool installation (BG-1).
pub mod maintenance;

/// The session manager (PLAN §6.4, M2 P2-6).
pub mod manager;

/// Omarchy management: `omarchy.status`, `omarchy.act` and the screensaver
/// tick (design `omarchy-admin.md`).
pub mod omarchy_admin;

/// Disk, memory and CPU for `station.info` (DL-193).
pub mod resources;

/// `fleetdeck station serve` (M2 P2-7).
pub mod serve;
/// The settings holder and the station handlers of `settings.*` and
/// `station.restart` (DL-172).
pub mod settings_svc;
/// The human shell service (PLAN §6.8, D10).
pub mod shell;

pub mod store;

/// The Taildrive report and the exposure check of its shares (FG-12).
pub mod taildrive;

/// The traits of the station for placement (FG-6).
pub mod traits;

/// Station self-upgrade: snapshot, trial, launcher, rollback (M3 7-3).
pub mod upgrade;

/// The effect worker (PLAN §6.4).
pub mod worker;

/// The workspace service (PLAN §6.6): checkpoints, diffs, restore.
pub mod workspace;
