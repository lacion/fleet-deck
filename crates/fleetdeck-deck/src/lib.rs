//! Fleet Deck v2 deck (PLAN §7): the station registry, the read model, the
//! station links, the command router and outbox, auth and audit, and the
//! board API.

pub mod cert_task;
pub mod elevation;
pub mod http;
pub mod link;
pub mod live;
pub mod local;
pub mod local_auth;
pub mod maintenance;
pub mod oauth;
pub mod peers;
pub mod routing;
pub mod scheduler;
pub mod serve;
pub mod store;
pub mod tailnet_auto;

pub use link::{DeckLink, DeckNode, NodeError};
pub use peers::{DeckPeers, Expected};
pub use store::{DeckError, DeckSession, DeckStore, Direction, StationRecord};
