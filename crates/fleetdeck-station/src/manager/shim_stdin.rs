//! Station stdin writes through a shim and their settlement after a
//! reattach (ADR-005 §4.3; slice 5, DL-105).
//!
//! - [`send_input`] splits the bytes of one claimed effect into frames of
//!   at most [`MAX_PAYLOAD`] bytes. Each frame's identity is committed
//!   ([`Store::shim_claim_write`]) before the frame leaves; a stale
//!   generation sends nothing.
//! - [`settle_writes`] asks `write_status` for every frame of every
//!   `claimed` effect of the shim and applies the §4.3 table per effect,
//!   before the generic recovery turns the remaining claims into
//!   `outcome_unknown`. An effect is retired only when every frame is
//!   proven unwritten; one `partial` frame blocks the session's input.

use std::time::{Duration, Instant};

use fleetdeck_providers::host::shim::host::ShimProcess;
use fleetdeck_providers::host::shim::settle::{self, EffectResult, Facts};
use fleetdeck_providers::host::shim::stdin::WriteOutcome;
use fleetdeck_providers::host::shim::wire::{MAX_PAYLOAD, StatusAnswer, WriteState};

use crate::store::{EffectId, Result, Store};

/// How long [`settle_writes`] waits between questions about a write in
/// flight.
const ASK_AGAIN: Duration = Duration::from_millis(100);
/// The longest single wait for one `write_status` answer.
const ANSWER_WAIT: Duration = Duration::from_secs(5);

/// The live result of [`send_input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// Every frame was written (`Dispatch::Sent`).
    Written,
    /// No frame left: the generation is stale, the row ended, or the
    /// effect is not `claimed`.
    NotSent,
    /// A frame ended in this state; the caller settles the effect with
    /// [`settle_live`].
    Ended(WriteState),
    /// The shim refused a frame (`forgotten`, `mismatch`).
    Refused(StatusAnswer),
}

/// Sends the bytes of the `claimed` effect `effect_id` to `process`.
///
/// # Errors
/// A database error. A transport failure after a committed frame identity
/// is not an error: it is `Sent::Ended(Writing)`, which settles as
/// `outcome_unknown`.
pub async fn send_input(
    store: &Store,
    process: &ShimProcess,
    shim_id: &str,
    effect_id: &EffectId,
    bytes: &[u8],
) -> Result<Sent> {
    for (ordinal, chunk) in (0u32..).zip(bytes.chunks(MAX_PAYLOAD)) {
        let Some(tag) = store
            .shim_claim_write(
                shim_id.to_owned(),
                process.generation(),
                Some(effect_id.clone()),
                ordinal,
            )
            .await?
        else {
            return Ok(if ordinal == 0 {
                Sent::NotSent
            } else {
                Sent::Ended(WriteState::Writing { bytes_done: 0 })
            });
        };
        match process.write_tagged(&tag, chunk).await {
            Ok(WriteOutcome::Done(WriteState::Written)) => {}
            Ok(WriteOutcome::Done(state)) => return Ok(Sent::Ended(state)),
            Ok(WriteOutcome::Refused(answer)) => return Ok(Sent::Refused(answer)),
            Err(_) => return Ok(Sent::Ended(WriteState::Writing { bytes_done: 0 })),
        }
    }
    Ok(Sent::Written)
}

/// The effect result of a live [`Sent`] outcome (the §4.3 table with
/// `live = true`).
#[must_use]
pub fn settle_live(sent: &Sent) -> EffectResult {
    let answer = match sent {
        Sent::Written => return EffectResult::Sent,
        Sent::NotSent => return EffectResult::Retired(settle::SHIM_NOT_RECEIVED),
        Sent::Ended(state) => StatusAnswer::Known {
            state: state.clone(),
        },
        Sent::Refused(answer) => answer.clone(),
    };
    settle::settle(Facts {
        answer: Some(&answer),
        already_unknown: false,
        wait_over: true,
        live: true,
        retire_enabled: settle::RETIRE_ROWS_ENABLED,
    })
}

/// What [`settle_writes`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Settled {
    pub retired: Vec<EffectId>,
    pub unknown: Vec<EffectId>,
    pub blocked: Vec<EffectId>,
}

/// Asks about one frame until the table gives a final result or the
/// in-flight wait ends.
async fn settle_frame(
    process: &ShimProcess,
    tag: &fleetdeck_providers::host::shim::stdin::WriteTag,
    deadline: Instant,
) -> EffectResult {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let answer = process.write_status(tag, left.min(ANSWER_WAIT)).await;
        let result = settle::settle(Facts {
            answer: answer.as_ref(),
            already_unknown: false,
            wait_over: Instant::now() >= deadline,
            live: false,
            retire_enabled: settle::RETIRE_ROWS_ENABLED,
        });
        if result != EffectResult::AskAgain {
            return result;
        }
        tokio::time::sleep(ASK_AGAIN.min(left)).await;
    }
}

/// Combines the frame results of one effect: retired only when every
/// frame is retired; a partial frame blocks input.
fn combine(results: &[EffectResult]) -> EffectResult {
    if results
        .iter()
        .any(|r| matches!(r, EffectResult::Unknown { block_input: true }))
    {
        return EffectResult::Unknown { block_input: true };
    }
    match results.first() {
        Some(first @ EffectResult::Retired(_))
            if results
                .iter()
                .all(|r| matches!(r, EffectResult::Retired(_))) =>
        {
            *first
        }
        _ => EffectResult::Unknown { block_input: false },
    }
}

/// Settles every `claimed` stdin effect of `shim_id` on the freshly
/// attached `process` (§4.3 "Settlement order"). `in_flight_wait` bounds
/// the questions about a write in flight ([`settle::IN_FLIGHT_WAIT_MS`] in
/// production).
///
/// # Errors
/// A database error; the effects then stay `claimed` and the generic
/// recovery makes them `outcome_unknown`.
pub async fn settle_writes(
    store: &Store,
    process: &ShimProcess,
    shim_id: &str,
    in_flight_wait: Duration,
) -> Result<Settled> {
    let mut done = Settled::default();
    for claimed in store.shim_writes_to_settle(shim_id.to_owned()).await? {
        let deadline = Instant::now() + in_flight_wait;
        let mut results = Vec::with_capacity(claimed.tags.len());
        for tag in &claimed.tags {
            results.push(settle_frame(process, tag, deadline).await);
        }
        let result = combine(&results);
        let id = claimed.effect.effect_id.clone();
        if store
            .shim_settle_write(shim_id.to_owned(), id.clone(), result)
            .await?
        {
            match result {
                EffectResult::Retired(_) => done.retired.push(id),
                EffectResult::Unknown { block_input: true } => done.blocked.push(id),
                _ => done.unknown.push(id),
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_effect_is_retired_only_when_every_frame_is_unwritten() {
        let retired = EffectResult::Retired(settle::SHIM_NOT_RECEIVED);
        let failed = EffectResult::Retired(settle::SHIM_WRITE_FAILED);
        let unknown = EffectResult::Unknown { block_input: false };
        let partial = EffectResult::Unknown { block_input: true };
        assert_eq!(combine(&[retired, failed]), retired);
        assert_eq!(combine(&[retired, unknown]), unknown);
        assert_eq!(combine(&[unknown, retired]), unknown);
        assert_eq!(combine(&[retired, partial]), partial);
        assert_eq!(combine(&[]), unknown);
    }
}

#[cfg(test)]
#[path = "shim_stdin_tests.rs"]
mod decide_tests;
