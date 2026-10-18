//! The stop latch of one recording (design §4.4, §7.4, §7.5). Every trigger
//! goes through [`Latch`]: it signals the recorder first and writes the durable
//! latch after, so a store that blocks never delays a kill (§7.1). The first
//! discard reason wins, in memory and in the row.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_proto::recording::{RecordingPhase, StopReason};

use crate::media::recorder::RecorderCtl;
use crate::store::Store;
use crate::store::capture_ops::StopIntent;

#[derive(Default)]
struct State {
    ctl: Option<RecorderCtl>,
    saved: bool,
    discard: Option<StopReason>,
}

/// The stop latch and the control handle of one recording.
pub(crate) struct Latch {
    store: Store,
    deck_fp: String,
    capture_id: String,
    state: Mutex<State>,
    /// The size reached the file ceiling (the code of `output_limit`).
    ceiling: AtomicBool,
    /// The phase that was written last (0 none, 1 stopping, 2 finalizing);
    /// the lock orders the writes.
    phase: tokio::sync::Mutex<u8>,
}

impl Latch {
    pub(crate) fn new(store: Store, deck_fp: &str, capture_id: &str) -> Arc<Self> {
        Arc::new(Self {
            store,
            deck_fp: deck_fp.to_owned(),
            capture_id: capture_id.to_owned(),
            state: Mutex::new(State::default()),
            ceiling: AtomicBool::new(false),
            phase: tokio::sync::Mutex::new(0),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn deck_fp(&self) -> &str {
        &self.deck_fp
    }

    pub(crate) fn capture_id(&self) -> &str {
        &self.capture_id
    }

    /// Hands over the control handle of the spawned recorder. A stop that was
    /// asked for before it existed is applied now.
    pub(crate) fn attach(&self, ctl: RecorderCtl) {
        let (saved, discard) = {
            let mut state = self.state();
            state.ctl = Some(ctl.clone());
            (state.saved, state.discard)
        };
        if discard.is_some() {
            ctl.discard();
        } else if saved {
            ctl.owner_stop();
        }
    }

    /// The first discard reason asked for, if any.
    pub(crate) fn first_discard(&self) -> Option<StopReason> {
        self.state().discard
    }

    pub(crate) fn mark_ceiling(&self) {
        self.ceiling.store(true, Ordering::Release);
    }

    pub(crate) fn ceiling(&self) -> bool {
        self.ceiling.load(Ordering::Acquire)
    }

    /// Discard at once: remembers the first reason and sends the kill. It
    /// does not wait for anything. Returns the reason that wins.
    pub(crate) fn discard_now(&self, reason: StopReason) -> StopReason {
        let (ctl, winner) = {
            let mut state = self.state();
            let winner = *state.discard.get_or_insert(reason);
            (state.ctl.clone(), winner)
        };
        if let Some(ctl) = ctl {
            ctl.discard();
        }
        winner
    }

    /// Save: sends the graceful stop unless a discard was asked for.
    fn save_now(&self) {
        let ctl = {
            let mut state = self.state();
            let first = !state.saved;
            state.saved = true;
            if first && state.discard.is_none() {
                state.ctl.clone()
            } else {
                None
            }
        };
        if let Some(ctl) = ctl {
            ctl.owner_stop();
        }
    }

    /// One stop trigger: the signal first, then the durable latch. `save`
    /// must fit `reason` (a save reason for a save). Answers what the row
    /// holds afterwards; `None` when the row is not a running recording or
    /// the store failed (the signal went out anyway).
    pub(crate) async fn stop(
        &self,
        save: bool,
        reason: StopReason,
    ) -> Option<(StopIntent, StopReason)> {
        if save {
            self.save_now();
        } else {
            self.discard_now(reason);
        }
        let held = match self
            .store
            .recording_latch(&self.deck_fp, &self.capture_id, save, reason)
            .await
        {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!(error = %e, "cannot write the recording stop latch");
                None
            }
        };
        self.mark_stopping().await;
        held
    }

    /// The shutdown stop: the kill goes out now; the durable latch follows in
    /// a task when a runtime exists.
    pub(crate) fn discard_in_background(self: &Arc<Self>, reason: StopReason) {
        self.discard_now(reason);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let latch = Arc::clone(self);
            drop(runtime.spawn(async move {
                latch.stop(false, reason).await;
            }));
        }
    }

    /// Records the phase `stopping` once, and never after `finalizing`.
    pub(crate) async fn mark_stopping(&self) {
        let mut phase = self.phase.lock().await;
        if *phase >= 1 {
            return;
        }
        *phase = 1;
        self.write_phase(RecordingPhase::Stopping).await;
    }

    /// Records the phase `finalizing`.
    pub(crate) async fn mark_finalizing(&self) {
        let mut phase = self.phase.lock().await;
        *phase = 2;
        self.write_phase(RecordingPhase::Finalizing).await;
    }

    /// Records the phase `recording` with the actual start of the recorder,
    /// unless a later phase was already written (then only the start time).
    pub(crate) async fn mark_recording(&self, started_at: i64) {
        let phase = self.phase.lock().await;
        let now = match *phase {
            0 => RecordingPhase::Recording,
            1 => RecordingPhase::Stopping,
            _ => RecordingPhase::Finalizing,
        };
        if let Err(e) = self
            .store
            .recording_phase(&self.deck_fp, &self.capture_id, now, Some(started_at))
            .await
        {
            tracing::warn!(error = %e, "cannot record the recording phase");
        }
    }

    async fn write_phase(&self, phase: RecordingPhase) {
        if let Err(e) = self
            .store
            .recording_phase(&self.deck_fp, &self.capture_id, phase, None)
            .await
        {
            tracing::warn!(error = %e, "cannot record the recording phase");
        }
    }
}
