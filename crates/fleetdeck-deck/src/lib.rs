//! Fleet Deck v2 deck (PLAN §7): the station registry, the read model, the
//! command router and outbox, auth and audit, and the board API.

pub mod peers;
pub mod store;

pub use peers::{DeckPeers, Expected};
pub use store::{DeckError, DeckSession, DeckStore, Direction, StationRecord};
