//! The artifact service of a station (R12, DL-165): publish a file (and the
//! local files that an HTML page names) as an immutable version, list and
//! read the versions, and suggest an HTML file that a session wrote.
//!
//! The bytes live under `<station data>/artifacts/<artifact_id>/<version>/`;
//! the index is in `station.db` ([`crate::store::artifacts`]). The same
//! service answers the link methods (`link/artifacts.rs`) and the
//! session's own `publish_artifact` MCP tool ([`fleet`]).

pub mod bundle;
pub mod fleet;
pub mod service;
pub mod stage;
pub mod suggest;

pub use service::ArtifactService;

/// Why the service refuses or fails a call. `code` is the link error code
/// ([`fleetdeck_proto::artifacts::code`], or `internal`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ArtifactError {
    pub code: &'static str,
    pub message: String,
}

impl ArtifactError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
