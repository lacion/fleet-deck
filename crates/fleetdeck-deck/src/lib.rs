//! Fleet Deck v2 deck (PLAN §7): the station registry, the read model, the
//! station links, the command router and outbox, auth and audit, and the
//! board API.

pub mod link;
pub mod local;
pub mod peers;
pub mod serve;
pub mod store;

pub use link::{DeckLink, DeckNode, NodeError};
pub use peers::{DeckPeers, Expected};
pub use store::{DeckError, DeckSession, DeckStore, Direction, StationRecord};
