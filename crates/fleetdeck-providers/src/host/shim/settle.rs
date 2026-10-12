//! The effect result of a `write_status` answer or a `write_done` frame
//! (ADR-005 §4.3 table). The station applies it to a `claimed` stdin effect.
//!
//! **Gate.** The two `retired_not_dispatched` rows are enabled only together
//! with the crash-boundary tests 26–35 and only while they pass (ADR-005
//! §4.3 "Gate in the implementation"). The real-process tests are in
//! `crates/fleetdeck-cli/tests/shim_stdin.rs` (slice 5, DL-105), so
//! [`RETIRE_ROWS_ENABLED`] is true. With the gate closed every answer maps
//! to `outcome_unknown`.

use super::wire::{StatusAnswer, WriteState};

/// See the module documentation. It changes only together with the real-
/// process tests 26–35.
pub const RETIRE_ROWS_ENABLED: bool = true;
/// How long the station asks again about a write in flight (ADR-005 §4.3).
pub const IN_FLIGHT_WAIT_MS: u64 = 30_000;

/// The reasons of the two `retired_not_dispatched` rows.
pub const SHIM_NOT_RECEIVED: &str = "shim_not_received";
pub const SHIM_WRITE_FAILED: &str = "shim_write_failed";

/// What the station does with the effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectResult {
    /// `retired_not_dispatched` with this reason; a later dispatch needs a
    /// new command and a new effect identity.
    Retired(&'static str),
    /// `outcome_unknown`; only provider evidence can confirm it. With
    /// `block_input` the session gets `input_blocked { stdin_partial_write }`.
    Unknown { block_input: bool },
    /// The write is in flight: ask again (at most [`IN_FLIGHT_WAIT_MS`]).
    AskAgain,
    /// Live `write_done { written }`: `Dispatch::Sent`.
    Sent,
}

/// The facts the station has about one claimed stdin write.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The answer on an attachment that passed the full handshake. `None`
    /// is no answer, a lost shim or an `identity_mismatch`.
    pub answer: Option<&'a StatusAnswer>,
    /// The effect is `outcome_unknown` already (an earlier recovery).
    pub already_unknown: bool,
    /// The bounded wait for a write in flight is over.
    pub wait_over: bool,
    /// The answer came from a live `write_done`, not from recovery.
    pub live: bool,
    /// [`RETIRE_ROWS_ENABLED`], a parameter so that the full table is
    /// testable.
    pub retire_enabled: bool,
}

/// The ADR-005 §4.3 table.
#[must_use]
pub fn settle(f: Facts<'_>) -> EffectResult {
    const UNKNOWN: EffectResult = EffectResult::Unknown { block_input: false };
    let Some(answer) = f.answer else {
        return UNKNOWN;
    };
    let retire = |reason| {
        if f.retire_enabled && !f.already_unknown {
            EffectResult::Retired(reason)
        } else {
            UNKNOWN
        }
    };
    match answer {
        StatusAnswer::NotReceived => retire(SHIM_NOT_RECEIVED),
        StatusAnswer::Forgotten | StatusAnswer::Mismatch => UNKNOWN,
        StatusAnswer::Known { state } => match state {
            WriteState::ClosedUnwritten => retire(SHIM_NOT_RECEIVED),
            WriteState::FailedUnwritten { .. } => retire(SHIM_WRITE_FAILED),
            WriteState::Written if f.live && !f.already_unknown => EffectResult::Sent,
            WriteState::Written => UNKNOWN,
            WriteState::Partial { .. } => EffectResult::Unknown { block_input: true },
            WriteState::Received | WriteState::Writing { .. } => {
                if f.wait_over || f.already_unknown || f.live {
                    UNKNOWN
                } else {
                    EffectResult::AskAgain
                }
            }
        },
    }
}
