//! The consistent checkpoint boundary of one session (PLAN §6.6, Revision 3
//! design review; DL-078). Pure bookkeeping, no I/O: the session manager
//! feeds it the dispatch of each workspace-modifying input and the session's
//! events, and it says when a checkpoint can be captured and why not.
//!
//! Rules:
//! - **Start.** The start checkpoint of an input is captured under the
//!   session's workspace fence, before the input is written, and only when
//!   the session is quiet: no running turn, no other dispatched input that
//!   has not joined a turn, no background work. Otherwise it is unavailable
//!   (`turn_running`, `queued_input`, `background_writers`).
//! - **Turn start.** A turn takes the start checkpoint of the oldest input
//!   that was written and is still waiting. A turn with no such input was
//!   started by the provider itself: its start is unavailable
//!   (`provider_initiated`).
//! - **Completion.** At `turn.completed` the completion checkpoint is due
//!   only if no dispatched input waits (Claude queues turns natively) and no
//!   background work runs; otherwise it is unavailable (`queued_input`,
//!   `background_writers`). A due capture runs later under the fence; the
//!   next input's dispatch takes the fence and runs a due capture first, so
//!   writes of a later turn never enter an earlier checkpoint. Nothing here
//!   delays `turn.completed`.
//! - **Invalidation.** Work that the station did not dispatch cannot be
//!   fenced. When a turn starts or background work starts while a capture
//!   is due or running, that capture is invalid: the completion side becomes
//!   unavailable (`provider_initiated`, `queued_input`, `background_writers`)
//!   at once, and the later capture result is dropped
//!   ([`Boundary::end_captured`] returns nothing). A start capture of an
//!   input that is not written yet becomes unavailable in the same way
//!   (`turn_running`, `background_writers`), and its late result is ignored.
//!   Residual window: a provider event that is still in the stdout pipe when
//!   a capture result is recorded cannot be seen (DL-079).
//! - **Process end.** A turn that never completed has no completion
//!   checkpoint (`session_ended`).

use std::collections::{BTreeMap, VecDeque};

use fleetdeck_proto::event::InputState;
use fleetdeck_proto::{Event, InputId, TurnId};

/// The session ran a turn when the input was dispatched.
pub const REASON_TURN_RUNNING: &str = "turn_running";
/// Another dispatched input had not joined a turn yet.
pub const REASON_QUEUED_INPUT: &str = "queued_input";
/// Provider background work could still write.
pub const REASON_BACKGROUND_WRITERS: &str = "background_writers";
/// The provider started the turn without an input of this station.
pub const REASON_PROVIDER_INITIATED: &str = "provider_initiated";
/// The process ended before the turn completed.
pub const REASON_SESSION_ENDED: &str = "session_ended";
/// The station restarted before the capture was published.
pub const REASON_STATION_RESTART: &str = "station_restart";
/// The session directory is not a Git worktree.
pub const REASON_NOT_A_WORKTREE: &str = "not_a_git_worktree";
/// A Git command failed during the capture.
pub const REASON_CAPTURE_FAILED: &str = "capture_failed";

/// The state of one checkpoint side inside the station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    /// The capture is decided but its result is not recorded yet.
    Capturing,
    /// A published checkpoint commit.
    Available(String),
    /// No checkpoint, with a stable reason code.
    Unavailable(String),
}

impl Side {
    fn unavailable(reason: &str) -> Self {
        Self::Unavailable(reason.to_owned())
    }
}

/// What the dispatch of an input must do before the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPlan {
    /// Capture the start checkpoint now (the session is quiet).
    Capture,
    /// Record no start checkpoint for this input.
    Unavailable(&'static str),
}

/// What happens after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Nothing to do.
    Nothing,
    /// A turn started; bind its start side (`app turn` keys are minted by
    /// the caller).
    TurnStarted { turn: TurnId, start: Side },
    /// The completion checkpoint of `turn` is due: capture it under the
    /// fence ([`Boundary::take_due`]).
    CaptureEnd { turn: TurnId },
    /// The turn is complete and its completion side is decided.
    Finalize { turn: TurnId, end: Side },
}

#[derive(Debug, Clone)]
struct Waiting {
    input: InputId,
    start: Side,
    /// The input was written to the provider.
    sent: bool,
}

/// The boundary state of one session.
#[derive(Debug, Default)]
pub struct Boundary {
    running: Option<TurnId>,
    waiting: VecDeque<Waiting>,
    background: bool,
    due: Option<TurnId>,
    /// The completion capture that a caller took and has not recorded.
    capturing: Option<TurnId>,
    /// Turns that started and are not finalized: the start side, and true
    /// once the completion side is decided.
    open: BTreeMap<TurnId, (Side, bool)>,
}

impl Boundary {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The completed turn whose completion capture is due, if any. The
    /// caller holds the fence, captures it before anything else and records
    /// the result with [`Boundary::end_captured`].
    pub fn take_due(&mut self) -> Option<TurnId> {
        let turn = self.due.take()?;
        self.capturing = Some(turn.clone());
        Some(turn)
    }

    /// True while a completion capture is due and no caller took it.
    #[must_use]
    pub fn is_due(&self, turn: &TurnId) -> bool {
        self.due.as_ref() == Some(turn)
    }

    /// True while a caller runs the completion capture of `turn`.
    #[must_use]
    pub fn is_capturing(&self, turn: &TurnId) -> bool {
        self.capturing.as_ref() == Some(turn)
    }

    /// The result of a completion capture taken with
    /// [`Boundary::take_due`]. Returns the finalization, or nothing when
    /// the capture was invalidated meanwhile (the turn was already
    /// finalized with an unavailable completion side).
    pub fn end_captured(&mut self, turn: &TurnId, end: Side) -> Option<Next> {
        if self.capturing.as_ref() != Some(turn) {
            return None;
        }
        self.capturing = None;
        Some(Next::Finalize {
            turn: turn.clone(),
            end,
        })
    }

    /// Makes a due or running completion capture invalid.
    fn invalidate_end(&mut self, reason: &str) -> Vec<Next> {
        [self.due.take(), self.capturing.take()]
            .into_iter()
            .flatten()
            .map(|turn| Next::Finalize {
                turn,
                end: Side::unavailable(reason),
            })
            .collect()
    }

    /// Makes running start captures of inputs that are not written invalid.
    fn invalidate_starts(&mut self, reason: &str) {
        for w in self.waiting.iter_mut().filter(|w| !w.sent) {
            if w.start == Side::Capturing {
                w.start = Side::unavailable(reason);
            }
        }
    }

    /// The dispatch of a workspace-modifying input, under the fence and
    /// after any due completion capture.
    pub fn begin_input(&mut self, input: InputId) -> StartPlan {
        let plan = if self.running.is_some() {
            StartPlan::Unavailable(REASON_TURN_RUNNING)
        } else if !self.waiting.is_empty() {
            StartPlan::Unavailable(REASON_QUEUED_INPUT)
        } else if self.background {
            StartPlan::Unavailable(REASON_BACKGROUND_WRITERS)
        } else {
            StartPlan::Capture
        };
        let start = match plan {
            StartPlan::Capture => Side::Capturing,
            StartPlan::Unavailable(reason) => Side::unavailable(reason),
        };
        self.waiting.push_back(Waiting {
            input,
            start,
            sent: false,
        });
        plan
    }

    /// The result of a start capture of [`StartPlan::Capture`]. A capture
    /// that was invalidated meanwhile keeps its unavailable side.
    pub fn record_start(&mut self, input: &InputId, start: Side) {
        if let Some(w) = self.waiting.iter_mut().find(|w| &w.input == input)
            && w.start == Side::Capturing
        {
            w.start = start;
        }
    }

    /// The start side recorded for an input that waits for its turn.
    #[must_use]
    pub fn waiting_start(&self, input: &InputId) -> Option<&Side> {
        self.waiting
            .iter()
            .find(|w| &w.input == input)
            .map(|w| &w.start)
    }

    /// The input was written to the provider (after its start side was
    /// recorded). Only written inputs can start a turn.
    pub fn input_sent(&mut self, input: &InputId) {
        if let Some(w) = self.waiting.iter_mut().find(|w| &w.input == input) {
            w.sent = true;
        }
    }

    /// The input was not written (the dispatch failed before any I/O).
    pub fn input_not_sent(&mut self, input: &InputId) {
        self.waiting.retain(|w| &w.input != input);
    }

    /// The start side of an open turn.
    #[must_use]
    pub fn start_of(&self, turn: &TurnId) -> Option<&Side> {
        self.open.get(turn).map(|(start, _)| start)
    }

    /// Removes a finalized turn.
    pub fn finalized(&mut self, turn: &TurnId) -> Option<Side> {
        if self.due.as_ref() == Some(turn) {
            self.due = None;
        }
        if self.capturing.as_ref() == Some(turn) {
            self.capturing = None;
        }
        self.open.remove(turn).map(|(start, _)| start)
    }

    /// Feeds one session event in log order.
    pub fn on_event(&mut self, event: &Event) -> Vec<Next> {
        match event {
            Event::TurnStarted { turn_id, .. } => {
                let sent = self.waiting.iter().position(|w| w.sent);
                let mut next = self.invalidate_end(if sent.is_some() {
                    REASON_QUEUED_INPUT
                } else {
                    REASON_PROVIDER_INITIATED
                });
                self.invalidate_starts(REASON_TURN_RUNNING);
                self.running = Some(turn_id.clone());
                let start = match sent.and_then(|i| self.waiting.remove(i)) {
                    Some(w) => w.start,
                    None => Side::unavailable(REASON_PROVIDER_INITIATED),
                };
                self.open.insert(turn_id.clone(), (start.clone(), false));
                next.push(Next::TurnStarted {
                    turn: turn_id.clone(),
                    start,
                });
                next
            }
            Event::InputUpdated {
                input_id, state, ..
            } => {
                if !matches!(state, InputState::Received) {
                    self.waiting.retain(|w| &w.input != input_id);
                }
                Vec::new()
            }
            Event::BackgroundUpdated { tasks } => {
                self.background = !tasks.is_empty();
                if !self.background {
                    return Vec::new();
                }
                self.invalidate_starts(REASON_BACKGROUND_WRITERS);
                self.invalidate_end(REASON_BACKGROUND_WRITERS)
            }
            Event::TurnCompleted { turn_id, .. } => {
                if self.running.as_ref() == Some(turn_id) {
                    self.running = None;
                }
                match self.open.get_mut(turn_id) {
                    Some((_, decided)) if !*decided => *decided = true,
                    _ => return Vec::new(),
                }
                let end = if !self.waiting.is_empty() {
                    Side::unavailable(REASON_QUEUED_INPUT)
                } else if self.background {
                    Side::unavailable(REASON_BACKGROUND_WRITERS)
                } else {
                    self.due = Some(turn_id.clone());
                    return vec![Next::CaptureEnd {
                        turn: turn_id.clone(),
                    }];
                };
                vec![Next::Finalize {
                    turn: turn_id.clone(),
                    end,
                }]
            }
            Event::SessionEnded { .. } => {
                self.running = None;
                self.waiting.clear();
                self.background = false;
                self.open
                    .iter_mut()
                    .filter(|(_, (_, decided))| !*decided)
                    .map(|(t, (_, decided))| {
                        *decided = true;
                        Next::Finalize {
                            turn: t.clone(),
                            end: Side::unavailable(REASON_SESSION_ENDED),
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}
