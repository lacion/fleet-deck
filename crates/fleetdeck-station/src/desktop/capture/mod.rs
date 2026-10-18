//! The station building blocks of one owner screenshot (R13 P5a task D1,
//! design `omarchy-p5a.md` §4 to §5) and the operation that wires them
//! together (task D2).
//!
//! - [`run`]: `grim -t png -s 1 -l 6 -o <monitor> -` through `shim-exec`.
//! - [`png`]: the structural check of the output.
//! - [`stage`]: `<captures>/<capture_id>/capture.png` (or `capture.mp4`), mode
//!   0600, and its durable removal.
//! - [`cleanup`]: the durable cleanup of a failed or committed operation.
//! - [`budget`]: the one-capture slot, the free-space check, the monitor
//!   limits.
//! - [`op`]: `desktop.act` with `screen_capture`, from the slot to the
//!   answer.

use std::time::Duration;

mod budget;
mod cleanup;
mod coordinator;
mod op;
mod png;
mod run;
mod stage;

pub use op::{ADMISSION, Allowed, Params, Request};
pub use run::{Launch, capture_png};

pub(crate) use budget::{CaptureSlot, check_monitor, room};
pub(crate) use cleanup::{cleanup_failed, finish_staging};
pub(crate) use coordinator::capture_gate;
#[cfg(test)]
pub(crate) use png::PngInfo;
pub(crate) use png::check_png;
pub(crate) use stage::Staging;

/// The most bytes of a PNG.
pub const MAX_PNG_BYTES: u64 = 33_554_432;
/// The most pixels of one side of a screenshot.
pub const MAX_DIMENSION: u32 = 8192;
/// The most pixels of a screenshot.
pub const MAX_PIXELS: u64 = 16_777_216;
/// The station capture quota (the store sums it).
pub const CAPTURE_QUOTA: u64 = 4_294_967_296;
/// The free bytes that a capture leaves on the disk.
pub const FREE_RESERVE: u64 = 2_147_483_648;
/// The most bytes of one read or write of the copy.
pub const COPY_BUFFER: usize = 65_536;
/// The most bytes of stderr that the runner keeps.
pub const STDERR_KEEP: usize = 16_384;
/// The time limit of one `grim` run.
pub const GRIM_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod coordinator_tests;
#[cfg(test)]
mod op_tests;
#[cfg(test)]
mod png_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod stage_tests;
