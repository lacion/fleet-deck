//! Fleet Deck v2 foundations (PLAN §12): injected clock and id source,
//! [`Secret<T>`](secret::Secret), node identity keys, the D13 network scope,
//! data and config directories, `station.toml` / `deck.toml`, SQLite on a
//! dedicated thread with migrations, and logging.

pub mod childenv;
pub mod clock;
pub mod config;
pub mod db;
pub mod identity;
pub mod ids;
pub mod logging;
pub mod netscope;
pub mod paths;
pub mod secret;

pub use childenv::ChildEnv;
pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use ids::{IdSource, SeqIds, SharedIds, UlidSource, new_id};
pub use secret::Secret;
