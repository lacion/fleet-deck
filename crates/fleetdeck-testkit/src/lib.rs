//! Fleet Deck v2 test kit (PLAN §13): deterministic clock and id source,
//! temporary node directories, and the entry points of the fake agents.
//! Test code only; it never ships in a release build.
//!
//! - [`det`]: a manual clock plus a sequential id source that share one time
//!   line, so replays and fault tests are deterministic.
//! - [`node`]: a temporary home and data directory for one station or deck,
//!   with a `station.toml` writer.
//! - [`fake`]: how a test finds and starts `fake-claude` and `fake-codex`.
//! - [`script`]: the fixture player of the fake agents.
//! - [`sim_claude`]: scenario `sim`, a reactive model of the Claude CLI.

pub mod det;
pub mod fake;
pub mod node;
pub mod script;
pub mod sim_claude;

pub use det::Det;
pub use fake::{FakeAgent, FakeError};
pub use node::TempNode;
