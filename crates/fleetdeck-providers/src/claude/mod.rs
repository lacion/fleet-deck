//! The Claude adapter (ADR-001): the frame parser, the frame builders, and the
//! sans-IO core that maps `claude -p` stream-json frames to canonical events.
//! The process driver that wraps the core in a [`crate::LiveSession`] follows
//! in Phase 2 step P2-3.

pub mod args;
pub mod control;
pub mod frames;
mod host_cmd;
mod map;
mod requests;

mod core;

pub use self::core::{ClaudeCore, CoreConfig, Output};
pub use args::{
    BYPASS_MODE, Launch, NATIVE_MODES, SAFEST_MODE, capabilities, cli_args, input_uuid,
    parse_auth_status, parse_version,
};
pub use host_cmd::HostCmd;
pub use map::{input_summary, tool_kind};
pub use requests::request_kind;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_traps;
