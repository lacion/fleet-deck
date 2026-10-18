//! The station side of an owner monitor recording (R13 P5b task S1, design
//! `omarchy-p5b.md` §3.2, §4, §5.3, §6, §7, §8, §9, §10).
//!
//! - [`op`]: `desktop.act` with `screen_record`, from the slot to the answer:
//!   admit, reserve, spawn the owned recorder with the retained descriptor,
//!   finalize, validate, import and commit, or discard and clean up.
//! - [`supervisor`]: the one-second watchdog of a running recording (live
//!   gates, lock, monitor, size, free space, link).
//! - [`lease`]: the continuation lease client (`desktop.capture_lease`).
//! - [`guard`]: the lease expiry of the operation (alive until the commit)
//!   and the launch guard (the last checks, on the thread of the recorder).
//! - [`latch`]: the stop latch of one recording and its stop actions.
//! - [`probe`]: the light desktop probe of the watchdog and of
//!   `desktop.capture_policy_now`.
//! - [`registry`]: the live recordings of the station.
//!
//! The recorder itself, with its timers, runs on the E3 thread
//! ([`crate::media::recorder`]); nothing here delays those timers.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::coordinator_desktop_text::text as p6_text;
use fleetdeck_proto::recording::StopReason;

use super::capture::{Allowed, Launch};
use super::{Fail, code as desktop_code};
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::DurableFs;
use crate::desktop::authority::Admitted;
use crate::media::clock::Clocks;

mod finish;
mod guard;
mod latch;
mod lease;
mod op;
mod probe;
mod registry;
mod supervisor;

pub use lease::{ControlLink, DeckLink};
pub(crate) use registry::Registry;

/// Who holds the capture slot (R13 P5b: `desktop.capture_active` tells a
/// recording of the calling deck from every other holder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A screenshot, owner or coordinator.
    Screenshot,
    /// A recording of the deck with this fingerprint.
    Recording { deck_fp: String, capture_id: String },
}

/// The time that the station operation keeps after the requested duration
/// (design §10: `seconds + 30`).
pub const OPERATION_SLACK: Duration = Duration::from_secs(30);

/// The free-space check of a staging or destination folder (a test replaces
/// it): it must keep the reserve free after `need` more bytes.
pub type RoomCheck = Arc<dyn Fn(&OwnedFd, u64) -> Result<(), Fail> + Send + Sync>;

/// The timings of one recording. The defaults are the design values; a test
/// shortens them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The watchdog period and the lease renewal period (1 s).
    pub tick: Duration,
    /// The total deadline of one lease request (5 s).
    pub request: Duration,
    /// The validity of a successful lease from its request start (6 s).
    pub validity: Duration,
    /// The time between `SIGINT` and `SIGKILL` (3 s).
    pub grace: Duration,
    /// The deadline of one watchdog probe (1 s).
    pub probe: Duration,
    /// The time that the operation keeps after the requested duration (30 s).
    pub slack: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            request: Duration::from_secs(5),
            validity: Duration::from_secs(6),
            grace: Duration::from_secs(3),
            probe: Duration::from_secs(1),
            slack: OPERATION_SLACK,
        }
    }
}

/// What a test replaces: the launch of `wf-recorder`, the syncs, the clocks,
/// the admission limit and the timings.
pub struct RecordParams {
    pub launch: Launch,
    pub fs: Arc<dyn DurableFs>,
    pub admission: Duration,
    pub clocks: Arc<dyn Clocks>,
    pub timing: Timing,
    pub room: RoomCheck,
}

/// One recording request.
pub struct RecordRequest {
    pub service: ArtifactService,
    /// The live gates; it is read at the start and then every second.
    pub allowed: Arc<dyn Fn() -> Allowed + Send + Sync>,
    pub admitted: Admitted,
    /// The authenticated deck fingerprint (64 hex).
    pub deck_fp: String,
    pub capture_id: String,
    pub action_hash: String,
    pub monitor: String,
    pub seconds: u32,
    /// The station's own id (the deck replaces it with its stable id).
    pub station_id: String,
    pub station_name: String,
    /// The deck link of the start: the lease goes over it.
    pub link: Arc<dyn DeckLink>,
    /// The `local_desktop_epoch` of that link.
    pub link_epoch: String,
    pub params: RecordParams,
}

/// The text of `forbidden` when recordings are off.
pub(crate) const OFF: &str = "Desktop recordings are off on this station.";
/// The text of `forbidden` for a monitor outside the allowlist.
pub(crate) const MONITOR_NOT_ALLOWED: &str = "This monitor is not allowed for recordings.";

/// The live gates of the station policy for a recording.
///
/// # Errors
/// `forbidden` when a gate is off or the monitor is not allowed.
pub(crate) fn check_gates(allowed: &Allowed, monitor: &str) -> Result<(), Fail> {
    if !(allowed.desktop && allowed.capture) {
        return Err(Fail::new(desktop_code::FORBIDDEN, OFF));
    }
    if !allowed.monitors.is_empty() && !allowed.monitors.iter().any(|m| m == monitor) {
        return Err(Fail::new(desktop_code::FORBIDDEN, MONITOR_NOT_ALLOWED));
    }
    Ok(())
}

/// The exact text of a terminal code of a recording (design §16).
pub(crate) fn code_text(c: &str) -> &'static str {
    match c {
        code::SCREEN_LOCKED => text::SCREEN_LOCKED,
        code::DISK_FULL => text::DISK_FULL,
        code::RECORDING_TOO_LARGE => text::RECORDING_TOO_LARGE,
        code::RECORDING_FAILED => text::RECORDING_FAILED,
        code::RECORDING_INVALID => text::RECORDING_INVALID,
        code::RECORDING_PUBLISH_FAILED => text::RECORDING_PUBLISH_FAILED,
        code::RECORDING_CANCELLED => text::RECORDING_CANCELLED,
        code::RECORDING_INTERRUPTED => text::RECORDING_INTERRUPTED,
        other => p6_text(other).unwrap_or(text::RECORDING_INTERRUPTED),
    }
}

/// The failure of an automatic stop: the owner terminal code of the reason
/// (design §7.5) with its exact text. `ceiling` tells the two causes of
/// `output_limit` apart.
pub(crate) fn fail_for(reason: StopReason, ceiling: bool) -> Fail {
    let c = reason.owner_code(ceiling);
    Fail::new(c, code_text(c))
}

#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod latch_tests;
#[cfg(test)]
mod lease_tests;
#[cfg(test)]
mod op_tests;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
pub(crate) mod rig_tests;
#[cfg(test)]
mod supervisor_tests;
