//! What a station needs to run a session under a durable host (ADR-005;
//! DL-113): the process to launch under a shim and the configuration of the
//! sans-IO core that the station drives itself.
//!
//! [`crate::ProviderAdapter::durable_launch`] builds it with the same
//! workspace, capability, mode and argument rules as the adapter's direct
//! start, but starts nothing. The station launches the process under a shim
//! and drives the core with its durable driver; a restart restores the core
//! from the stored checkpoint with the same configuration.

use crate::host::{ProcessSpec, StopPolicy};
use crate::{claude, codex};

/// The core configuration of a durable session.
#[derive(Debug, Clone, PartialEq)]
pub enum DurableConfig {
    Claude(claude::CoreConfig),
    /// The core configuration and the thread that the handshake opens.
    Codex {
        config: codex::CoreConfig,
        thread: codex::SessionSpec,
    },
}

/// A durable launch: the process, the core configuration and the stop
/// policy of a graceful stop.
#[derive(Debug, Clone)]
pub struct DurableLaunch {
    pub process: ProcessSpec,
    pub core: DurableConfig,
    pub stop: StopPolicy,
}
