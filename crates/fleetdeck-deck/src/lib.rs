//! Fleet Deck v2 deck (PLAN §7): the station registry, the read model, the
//! station links, the command router and outbox, auth and audit, and the
//! board API.

pub mod artifact_deletions;
pub mod cert_task;
mod coordinator_desktop;
pub mod desktop_capture;
mod desktop_recording;
pub mod desktop_relay;
pub mod elevation;
pub mod fleet_git_homes;
pub mod fleet_git_land;
pub mod fleet_git_land_abandon;
pub mod fleet_git_land_outbox;
pub mod fleet_git_move;
pub mod fleet_git_move_after;
pub mod fleet_git_move_pipe;
pub mod fleet_git_move_send;
pub mod fleet_git_radar;
pub mod fleet_summary;
pub mod http;
pub mod link;
pub mod live;
pub mod local;
pub mod local_auth;
pub mod local_provision;
pub mod maintenance;
pub mod oauth;
pub mod peers;
pub mod routing;
pub mod routing_place;
pub mod scheduler;
pub mod serve;
pub mod store;
pub mod tailnet_auto;
pub mod wake;

pub use link::{DeckLink, DeckNode, NodeError};
pub use peers::{DeckPeers, Expected};
pub use store::{DeckError, DeckSession, DeckStore, Direction, StationRecord};
