//! MCP Apps on the station (B15, design `docs/v2/design/mcp-apps.md` §4.3):
//! the capture service that reads the app document after a tool call and
//! stores it with its `tool.app` event, and the app tool calls with their
//! workspace protection ([`calls`], design §4.6.1).

pub mod calls;
mod capture;
pub mod sanitize;

pub use capture::{Capture, CaptureStats};

#[cfg(test)]
mod capture_tests;
