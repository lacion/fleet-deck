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
//!   proven unwritten.
//! - An effect whose input may have reached the provider only in part
//!   blocks the session's input (`stdin_partial_write`): a `partial`
//!   frame, or frames of one effect with different fates (an earlier
//!   frame written or possibly written and a later frame not written or of
//!   unknown fate). A later unwritten frame never retires an effect whose
//!   earlier frames were written (DL-106).

use std::time::{Duration, Instant};

use fleetdeck_providers::host::shim::host::ShimProcess;
use fleetdeck_providers::host::shim::settle::{self, EffectResult, Facts};
use fleetdeck_providers::host::shim::stdin::WriteOutcome;
use fleetdeck_providers::host::shim::wire::{MAX_PAYLOAD, StatusAnswer, WriteState};

use crate::store::{EffectId, Result, Store, StoreError};

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
    /// A frame ended in `state` after `frames_written` earlier frames of
    /// the effect were written; the caller settles the effect with
    /// [`settle_live`].
    Ended {
        frames_written: u32,
        state: WriteState,
    },
    /// The shim refused a frame (`forgotten`, `mismatch`) after
    /// `frames_written` earlier frames were written.
    Refused {
        frames_written: u32,
        answer: StatusAnswer,
    },
}

/// A transport failure, or a later frame that could not claim its
/// identity: its fate is unknown.
const LOST_FRAME: WriteState = WriteState::Writing { bytes_done: 0 };

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
    let frames = u32::try_from(bytes.len().div_ceil(MAX_PAYLOAD))
        .map_err(|_| StoreError::Corrupt("a stdin effect with too many frames".to_owned()))?;
    for (ordinal, chunk) in (0u32..).zip(bytes.chunks(MAX_PAYLOAD)) {
        let Some(tag) = store
            .shim_claim_write(
                shim_id.to_owned(),
                process.generation(),
                Some(effect_id.clone()),
                ordinal,
                frames,
            )
            .await?
        else {
            return Ok(if ordinal == 0 {
                Sent::NotSent
            } else {
                Sent::Ended {
                    frames_written: ordinal,
                    state: LOST_FRAME,
                }
            });
        };
        let frames_written = ordinal;
        match process.write_tagged(&tag, chunk).await {
            Ok(WriteOutcome::Done(WriteState::Written)) => {}
            Ok(WriteOutcome::Done(state)) => {
                return Ok(Sent::Ended {
                    frames_written,
                    state,
                });
            }
            Ok(WriteOutcome::Refused(answer)) => {
                return Ok(Sent::Refused {
                    frames_written,
                    answer,
                });
            }
            Err(_) => {
                return Ok(Sent::Ended {
                    frames_written,
                    state: LOST_FRAME,
                });
            }
        }
    }
    Ok(Sent::Written)
}

/// The effect result of a live [`Sent`] outcome (the §4.3 table with
/// `live = true`). When an earlier frame of the effect was written, the
/// provider read part of the input: the effect is `outcome_unknown` and
/// blocks input, whatever the later frame's state is.
#[must_use]
pub fn settle_live(sent: &Sent) -> EffectResult {
    let answer = match sent {
        Sent::Written => return EffectResult::Sent,
        Sent::NotSent => return EffectResult::Retired(settle::SHIM_NOT_RECEIVED),
        Sent::Ended { frames_written, .. } | Sent::Refused { frames_written, .. }
            if *frames_written > 0 =>
        {
            return EffectResult::Unknown { block_input: true };
        }
        Sent::Ended { state, .. } => StatusAnswer::Known {
            state: state.clone(),
        },
        Sent::Refused { answer, .. } => answer.clone(),
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

/// The fate of one frame after a reattach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// The shim reported `written`.
    Written,
    /// Proven unwritten (a retirement row of the table, with its reason).
    Unwritten(&'static str),
    /// The shim reported `partial`.
    Partial,
    /// Any other answer: possibly written.
    Unknown,
}

/// Asks about one frame until the table gives a final result or the
/// in-flight wait ends.
async fn settle_frame(
    process: &ShimProcess,
    tag: &fleetdeck_providers::host::shim::stdin::WriteTag,
    deadline: Instant,
) -> Frame {
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
        let written = matches!(
            answer,
            Some(StatusAnswer::Known {
                state: WriteState::Written
            })
        );
        match result {
            EffectResult::AskAgain => {}
            EffectResult::Retired(reason) => return Frame::Unwritten(reason),
            EffectResult::Unknown { block_input: true } => return Frame::Partial,
            EffectResult::Unknown { .. } | EffectResult::Sent if written => {
                return Frame::Written;
            }
            EffectResult::Unknown { .. } | EffectResult::Sent => return Frame::Unknown,
        }
        tokio::time::sleep(ASK_AGAIN.min(left)).await;
    }
}

/// Combines the frame fates of one effect (see the module documentation):
/// - every frame unwritten → retired with the first frame's reason;
/// - every frame written, or a single frame of unknown fate →
///   `outcome_unknown`;
/// - a partial frame, or frames with different or unknown fates in a
///   multi-frame effect (the provider may hold a prefix of the input) →
///   `outcome_unknown` that blocks input.
fn combine(frames: &[Frame]) -> EffectResult {
    let unknown = EffectResult::Unknown { block_input: false };
    let partial = EffectResult::Unknown { block_input: true };
    let Some(first) = frames.first() else {
        return unknown;
    };
    if frames.contains(&Frame::Partial) {
        return partial;
    }
    if let Frame::Unwritten(reason) = first
        && frames.iter().all(|f| matches!(f, Frame::Unwritten(_)))
    {
        return EffectResult::Retired(reason);
    }
    if frames.iter().all(|f| *f == Frame::Written) || frames.len() == 1 {
        return unknown;
    }
    partial
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
        let mut frames = Vec::with_capacity(claimed.tags.len());
        for tag in &claimed.tags {
            frames.push(settle_frame(process, tag, deadline).await);
        }
        // Frames without a row were never claimed, so no byte of them left
        // (DL-107). They count as unwritten: with a written frame 0 the
        // effect is a partial input and blocks the session.
        let missing = usize::try_from(claimed.frames)
            .unwrap_or(usize::MAX)
            .saturating_sub(claimed.tags.len());
        frames.extend(std::iter::repeat_n(
            Frame::Unwritten(settle::SHIM_NOT_RECEIVED),
            missing,
        ));
        let result = combine(&frames);
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

    const RETIRED: EffectResult = EffectResult::Retired(settle::SHIM_NOT_RECEIVED);
    const UNKNOWN: EffectResult = EffectResult::Unknown { block_input: false };
    const BLOCKED: EffectResult = EffectResult::Unknown { block_input: true };

    #[test]
    fn an_effect_is_retired_only_when_every_frame_is_unwritten() {
        let not_received = Frame::Unwritten(settle::SHIM_NOT_RECEIVED);
        let failed = Frame::Unwritten(settle::SHIM_WRITE_FAILED);
        assert_eq!(combine(&[not_received, failed]), RETIRED);
        assert_eq!(
            combine(&[failed, not_received]),
            EffectResult::Retired(settle::SHIM_WRITE_FAILED)
        );
        assert_eq!(combine(&[Frame::Written]), UNKNOWN);
        assert_eq!(combine(&[Frame::Unknown]), UNKNOWN);
        assert_eq!(combine(&[Frame::Written, Frame::Written]), UNKNOWN);
        assert_eq!(combine(&[Frame::Partial]), BLOCKED);
        assert_eq!(combine(&[]), UNKNOWN);
    }

    #[test]
    fn a_later_unwritten_frame_never_retires_an_effect_whose_earlier_frames_were_written() {
        // Recovery: the provider holds a prefix of the input.
        let unwritten = Frame::Unwritten(settle::SHIM_NOT_RECEIVED);
        let failed = Frame::Unwritten(settle::SHIM_WRITE_FAILED);
        assert_eq!(combine(&[Frame::Written, unwritten]), BLOCKED);
        assert_eq!(combine(&[Frame::Written, failed]), BLOCKED);
        assert_eq!(
            combine(&[Frame::Written, Frame::Written, unwritten]),
            BLOCKED
        );
        assert_eq!(combine(&[Frame::Unknown, unwritten]), BLOCKED);
        assert_eq!(combine(&[Frame::Written, Frame::Unknown]), BLOCKED);
        assert_eq!(combine(&[unwritten, Frame::Written]), BLOCKED);
        // Live: every terminal state of a later frame blocks input once an
        // earlier frame was written.
        for state in [
            WriteState::FailedUnwritten {
                error: "EPIPE".to_owned(),
            },
            WriteState::ClosedUnwritten,
            WriteState::Partial {
                bytes_done: 1,
                error: "EPIPE".to_owned(),
            },
            WriteState::Writing { bytes_done: 0 },
        ] {
            let sent = Sent::Ended {
                frames_written: 1,
                state: state.clone(),
            };
            assert_eq!(settle_live(&sent), BLOCKED, "{state:?}");
            // The same state on the first frame keeps the §4.3 row.
            let first = Sent::Ended {
                frames_written: 0,
                state: state.clone(),
            };
            if !matches!(state, WriteState::Partial { .. }) {
                assert_ne!(settle_live(&first), BLOCKED, "{state:?}");
            }
        }
        for answer in [StatusAnswer::Forgotten, StatusAnswer::Mismatch] {
            let sent = Sent::Refused {
                frames_written: 2,
                answer,
            };
            assert_eq!(settle_live(&sent), BLOCKED);
        }
        assert_eq!(
            settle_live(&Sent::Ended {
                frames_written: 0,
                state: WriteState::FailedUnwritten {
                    error: "EPIPE".to_owned()
                }
            }),
            EffectResult::Retired(settle::SHIM_WRITE_FAILED)
        );
    }
}

#[cfg(test)]
#[path = "shim_stdin_tests.rs"]
mod decide_tests;
