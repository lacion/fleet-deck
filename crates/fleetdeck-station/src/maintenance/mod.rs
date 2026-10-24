//! The maintenance channel (`board-gaps.md` §2.9, BG-1).
//!
//! The runner: a separate process, `fleetdeck maintenance-run --id <id>
//! --dir <dir>`, that reads its spec, runs the fixed `sudo pacman`
//! transaction in an echo-off PTY, serves one authenticated client on a unix
//! socket with a 256 KiB replay ring, and records its exit, its output and a
//! post-check.
//!
//! The station side (BG1-3b): `maintenance.open` admits one durable run at a
//! time and launches the runner with `systemd-run` (`service.rs`, `open.rs`);
//! the states and the recovery after a restart (`records.rs`); the list, the
//! resize and the attach (`calls.rs`); the connection to a runner
//! (`runner_client.rs`); and the relay of the data stream `maintenance:<id>`
//! (`relay.rs`). The station never stops, kills or restarts a runner.
//!
//! Nothing here logs input bytes, output bytes or tokens. No value from the
//! wire reaches an argv: the packages come from the compiled mapping
//! (`crate::desktop_tools::mapping`).
//!
//! Files: `files.rs` (the folder, the spec, the state and the output),
//! `frame.rs` (the socket frames), `ring.rs` (the replay ring), `runner.rs`
//! (the start, the PTY and the exit), `client.rs` (the runner's socket),
//! `postcheck.rs` (the versions after the exit).

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::Value;

mod calls;
mod client;
pub mod files;
pub mod frame;
mod open;
pub mod postcheck;
mod records;
pub mod relay;
pub mod ring;
pub mod runner;
pub mod runner_client;
pub mod service;

pub use calls::attach_call;
pub use service::start;

#[cfg(test)]
mod calls_tests;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod open_tests;
#[cfg(test)]
mod postcheck_tests;
#[cfg(test)]
mod records_tests;
#[cfg(test)]
mod relay_tests;
#[cfg(test)]
mod ring_tests;
#[cfg(test)]
mod runner_client_tests;
#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod service_support;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod test_support;

impl service::MaintenanceService {
    /// The link handler of `maintenance.open`, `maintenance.list` and
    /// `maintenance.resize`.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        peer: Fingerprint,
        allow_shell: bool,
    ) -> Outcome {
        match method {
            "maintenance.open" => self.open(params, peer, allow_shell).await,
            "maintenance.list" => self.list(params).await,
            "maintenance.resize" => self.resize(params).await,
            other => Outcome::Err(WireError::new(
                crate::link::NOT_IMPLEMENTED,
                format!("`{other}` is not served by this station build"),
            )),
        }
    }
}

/// The link handler of the maintenance methods. `peer` is the fingerprint of
/// the deck link of the request: a plan is bound to it.
pub async fn call(method: &str, params: Value, peer: Fingerprint, allow_shell: bool) -> Outcome {
    let Some(service) = service::service() else {
        return Outcome::Err(WireError::new("unavailable", calls::TEXT_UNAVAILABLE));
    };
    service.call(method, params, peer, allow_shell).await
}
