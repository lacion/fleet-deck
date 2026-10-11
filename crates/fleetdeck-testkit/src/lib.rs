//! Fleet Deck v2 test kit (PLAN §13): deterministic clock and id source,
//! temporary node directories, and the entry points of the fake agents.
//! Test code only; it never ships in a release build.
//!
//! - [`det`]: a manual clock plus a sequential id source that share one time
//!   line, so replays and fault tests are deterministic.
//! - [`node`]: a temporary home and data directory for one station or deck,
//!   with a `station.toml` writer.
//! - [`fake`]: how a test finds and starts `fake-claude` and `fake-codex`.
//!   The binaries are skeletons now; their scenarios come with the adapters
//!   (Phase 2 P2-3 for Claude, Phase 5 for Codex).

pub mod det;
pub mod fake;
pub mod node;

pub use det::Det;
pub use fake::{FakeAgent, FakeError};
pub use node::TempNode;
