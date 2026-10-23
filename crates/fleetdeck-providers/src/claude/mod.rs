//! The Claude adapter (ADR-001): the frame parser, the frame builders, and the
//! sans-IO core that maps `claude -p` stream-json frames to canonical events,
//! and the process driver that wraps the core in a [`crate::LiveSession`].

/// The oldest `claude` version that the recorded fixtures cover (their
/// `init` frames report it; R8.2, DL-162).
pub const TESTED_MIN_VERSION: &str = "2.1.289";
/// `claude` versions that are known to break the protocol (none yet).
pub const KNOWN_BROKEN: &[&str] = &[];

pub mod args;
pub mod control;
pub mod cost_state;
pub mod driver;
pub mod frames;
mod host_cmd;
mod map;
pub mod process_usage;
mod requests;
mod usage;

mod checkpoint;
pub use checkpoint::{CHECKPOINT_READS, CHECKPOINT_VERSION};
mod core;

pub use self::core::{ClaudeCore, CoreConfig, Output};
pub use args::{
    BYPASS_MODE, Launch, NATIVE_MODES, SAFEST_MODE, capabilities, cli_args, input_uuid,
    parse_auth_status, parse_version,
};
pub use driver::{ClaudeAdapter, ClaudeAdapterConfig, ClaudeSession, Origin, RawLogSettings};
pub use host_cmd::HostCmd;
pub use map::{input_summary, tool_kind};
pub use process_usage::UsageBase;
pub use requests::request_kind;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_models;
#[cfg(test)]
mod tests_options;
#[cfg(test)]
mod tests_process_usage;
#[cfg(test)]
mod tests_traps;
#[cfg(test)]
mod tests_usage;
