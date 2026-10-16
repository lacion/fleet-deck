//! The turn checkpoints of one live session (PLAN §6.6, L13; DL-084): the
//! wiring of the pure [`Boundary`] bookkeeping, the Git checkpoint layer and
//! the durable `turn_checkpoints` rows into dispatch and the event pump.
//!
//! - **Fence.** Each session has one workspace fence. The dispatch of an
//!   input takes it ([`SessionTurns::fence`]), and a due completion capture
//!   of an earlier turn runs first under it, so writes of a later turn never
//!   enter an earlier checkpoint. A due capture without a later input runs in
//!   its own task under the same fence ([`SessionTurns::capture_due`]).
//! - **Start before the write.** For a prompt or a follow-up the dispatch
//!   stores the row (start side `capturing`), publishes the start ref when
//!   the session is quiet, records the side, and only then writes the input
//!   ([`SessionTurns::begin_input`]). A busy session gives an unavailable
//!   start with its reason; a directory outside Git gives
//!   `not_a_git_worktree`.
//! - **Events.** The pump feeds every stored event to
//!   [`SessionTurns::observe`]: `turn.started` binds the provider turn to the
//!   app turn (or stores a provider-initiated row), `turn.completed` makes a
//!   completion capture due or decides it unavailable, and an input that
//!   leaves the queue without a turn abandons its row. Nothing here delays
//!   `turn.completed`: it is in the log before the capture starts.
//! - **Finalization.** `turn.finalized` goes through
//!   [`crate::workspace::settle::finalize_row`]: the refs are checked again
//!   just before the event is committed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdGuard, Weak};

use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::{Event, InputId, SessionId, TurnId};
use tokio::sync::{Mutex, MutexGuard};

use crate::store::{
    AppTurnId, NewTurnCheckpoint, ProcessIncarnation, RowSide, Store, StoreError, Which,
};
use crate::workspace::boundary::{
    Boundary, Next, REASON_CAPTURE_FAILED, REASON_NO_TURN, REASON_NOT_A_WORKTREE,
    REASON_NOT_DISPATCHED, REASON_SESSION_ENDED, Side, StartPlan,
};
use crate::workspace::{
    Captured, CheckpointKey, GitOptions, Limits, Phase, PrivatePaths, SCOPE_TRACKED, Worktree,
    settle,
};

/// The workspace fence, held over an input write, a completion capture and
/// a whole revert.
pub type Fence<'a> = MutexGuard<'a, ()>;

/// The workspace fences of the station, one per canonical worktree root
/// (DL-093): every session in the same worktree takes the same fence, so a
/// revert of one session excludes the input writes and the checkpoint
/// captures of all of them. A session outside Git has its own fence.
#[derive(Debug, Clone, Default)]
pub struct Fences(Arc<StdMutex<HashMap<PathBuf, Weak<Mutex<()>>>>>);

impl Fences {
    /// The fence of the worktree at `root`; one shared value while any
    /// session holds it.
    #[must_use]
    pub fn for_worktree(&self, root: &Path) -> Arc<Mutex<()>> {
        let mut map = self.0.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|_, fence| fence.strong_count() > 0);
        if let Some(fence) = map.get(root).and_then(Weak::upgrade) {
            return fence;
        }
        let fence = Arc::new(Mutex::new(()));
        map.insert(root.to_path_buf(), Arc::downgrade(&fence));
        fence
    }
}

#[derive(Debug, Default)]
struct State {
    process: Option<ProcessIncarnation>,
    boundary: Boundary,
    by_input: HashMap<InputId, AppTurnId>,
    by_turn: HashMap<TurnId, AppTurnId>,
}

/// The checkpoint state of one live session.
#[derive(Debug)]
pub struct SessionTurns {
    store: Store,
    ids: SharedIds,
    options: GitOptions,
    limits: Limits,
    /// The private paths of the station: a checkpoint never holds the
    /// bytes of a private file (FDV2-003).
    private: Arc<PrivatePaths>,
    session_id: SessionId,
    worktree: Option<Worktree>,
    fence: Arc<Mutex<()>>,
    state: StdMutex<State>,
}

fn row_side(side: &Side) -> RowSide {
    match side {
        Side::Capturing => RowSide::Capturing,
        Side::Available(commit) => RowSide::Available(commit.clone()),
        Side::Unavailable(reason) => RowSide::Unavailable(reason.clone()),
    }
}

fn log_store(what: &str, result: Result<impl Sized, StoreError>) {
    if let Err(e) = result {
        tracing::error!(error = %e, "turn checkpoint: {what}");
    }
}

impl SessionTurns {
    /// Opens the checkpoint state of a session in `cwd`. A directory that
    /// is not inside a Git worktree is valid: every side is then
    /// `unavailable {not_a_git_worktree}`. The fence comes from `fences`
    /// by the worktree root.
    #[allow(clippy::too_many_arguments)]
    pub async fn open(
        store: Store,
        ids: SharedIds,
        options: GitOptions,
        limits: Limits,
        fences: &Fences,
        session_id: SessionId,
        cwd: &Path,
        private: Arc<PrivatePaths>,
    ) -> Arc<Self> {
        let worktree = Worktree::open(cwd, options.clone()).await.ok();
        let fence = worktree.as_ref().map_or_else(
            || Arc::new(Mutex::new(())),
            |w| fences.for_worktree(w.root()),
        );
        Arc::new(Self {
            store,
            ids,
            options,
            limits,
            private,
            session_id,
            worktree,
            fence,
            state: StdMutex::new(State::default()),
        })
    }

    fn lock(&self) -> StdGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// True when the session directory is a Git worktree.
    #[must_use]
    pub fn has_worktree(&self) -> bool {
        self.worktree.is_some()
    }

    /// The process that runs the session's turns now.
    pub fn set_process(&self, process: ProcessIncarnation) {
        self.lock().process = Some(process);
    }

    /// Takes the workspace fence and runs a due completion capture first.
    /// The caller holds the fence over its input write.
    pub async fn fence(&self) -> Fence<'_> {
        // An async (tokio) guard held on purpose over the capture: it is
        // the workspace fence itself.
        let guard = self.fence.lock().await;
        self.run_due().await;
        guard
    }

    /// Runs a due completion capture under the fence (the pump spawns this
    /// when `turn.completed` made a capture due).
    pub async fn capture_due(self: Arc<Self>) {
        // The async fence guard is held on purpose over the capture.
        let _guard = self.fence.lock().await;
        self.run_due().await;
    }

    async fn capture(&self, app_turn: &AppTurnId, phase: Phase) -> Side {
        let Some(worktree) = &self.worktree else {
            return Side::Unavailable(REASON_NOT_A_WORKTREE.to_owned());
        };
        let key = CheckpointKey {
            incarnation: self.store.incarnation().as_str().to_owned(),
            session: self.session_id.as_str().to_owned(),
            turn: app_turn.as_str().to_owned(),
            phase,
        };
        match worktree.capture(&key, self.limits, &self.private).await {
            Ok(Captured::Published(c) | Captured::Existing(c)) => Side::Available(c.commit),
            Ok(Captured::Unavailable(why)) => Side::Unavailable(why.code().to_owned()),
            Err(e) => {
                tracing::warn!(error = %e, session = %self.session_id, "checkpoint capture failed");
                Side::Unavailable(REASON_CAPTURE_FAILED.to_owned())
            }
        }
    }

    /// Under the fence: plans and captures the start side of a prompt or a
    /// follow-up before its write. Then the caller calls
    /// [`SessionTurns::input_sent`] and writes.
    pub async fn begin_input(&self, _fence: &Fence<'_>, input: &InputId) {
        let plan = self.lock().boundary.begin_input(input.clone());
        let app_turn: AppTurnId = new_id(self.ids.as_ref());
        let start = match plan {
            StartPlan::Capture if self.worktree.is_some() => Side::Capturing,
            StartPlan::Capture => Side::Unavailable(REASON_NOT_A_WORKTREE.to_owned()),
            StartPlan::Unavailable(reason) => Side::Unavailable(reason.to_owned()),
        };
        let process = {
            let mut st = self.lock();
            if start != Side::Capturing {
                st.boundary.record_start(input, start.clone());
            }
            st.by_input.insert(input.clone(), app_turn.clone());
            st.process.clone()
        };
        // B8: the folder generation of this start checkpoint, read under the
        // fence. A failed read makes the start unavailable, so the revert
        // barrier is never weaker than the stored value.
        let generation = match self.start_generation().await {
            Ok(generation) => generation,
            Err(e) => {
                tracing::error!(error = %e, "folder generation not read; start unavailable");
                let mut st = self.lock();
                st.boundary
                    .record_start(input, Side::Unavailable(REASON_CAPTURE_FAILED.to_owned()));
                st.by_input.remove(input);
                return;
            }
        };
        let planned = self
            .store
            .plan_turn_checkpoint(self.new_row(
                &app_turn,
                process,
                Some(input),
                None,
                &start,
                generation,
            ))
            .await;
        if let Err(e) = planned {
            tracing::error!(error = %e, "turn checkpoint row not stored; start unavailable");
            let mut st = self.lock();
            st.boundary
                .record_start(input, Side::Unavailable(REASON_CAPTURE_FAILED.to_owned()));
            st.by_input.remove(input);
            return;
        }
        if start != Side::Capturing {
            return;
        }
        let captured = self.capture(&app_turn, Phase::Start).await;
        // The boundary keeps an invalidation that came meanwhile.
        let effective = {
            let mut st = self.lock();
            st.boundary.record_start(input, captured);
            st.boundary.waiting_start(input).cloned()
        };
        let side = effective.unwrap_or_else(|| Side::Unavailable(REASON_CAPTURE_FAILED.into()));
        log_store(
            "start side",
            self.store
                .record_checkpoint_side(app_turn, Which::Start, row_side(&side))
                .await,
        );
    }

    /// The generation of the session folder (B8); `None` outside Git.
    async fn start_generation(&self) -> Result<Option<i64>, StoreError> {
        match &self.worktree {
            Some(w) => Ok(Some(
                self.store
                    .root_generation(w.root().to_string_lossy().into_owned())
                    .await?,
            )),
            None => Ok(None),
        }
    }

    fn new_row(
        &self,
        app_turn: &AppTurnId,
        process: Option<ProcessIncarnation>,
        input: Option<&InputId>,
        turn: Option<&TurnId>,
        start: &Side,
        generation: Option<i64>,
    ) -> NewTurnCheckpoint {
        NewTurnCheckpoint {
            app_turn: app_turn.clone(),
            session_id: self.session_id.clone(),
            process,
            input_id: input.cloned(),
            turn_id: turn.cloned(),
            worktree_id: self.worktree.as_ref().map(|w| w.id().to_owned()),
            worktree_root: self
                .worktree
                .as_ref()
                .map(|w| w.root().to_string_lossy().into_owned()),
            scope: SCOPE_TRACKED.to_owned(),
            start: row_side(start),
            generation,
        }
    }

    /// The input is about to be written (its start side is recorded).
    pub fn input_sent(&self, input: &InputId) {
        self.lock().boundary.input_sent(input);
    }

    /// The input was not written: its row is abandoned without an event.
    pub async fn input_not_sent(&self, input: &InputId) {
        let app_turn = {
            let mut st = self.lock();
            st.boundary.input_not_sent(input);
            st.by_input.remove(input)
        };
        if let Some(app_turn) = app_turn {
            log_store(
                "abandon",
                self.store
                    .abandon_turn_checkpoint(app_turn, REASON_NOT_DISPATCHED.to_owned())
                    .await,
            );
        }
    }

    /// Feeds stored session events in log order. Returns true when a
    /// completion capture became due; the caller then runs
    /// [`SessionTurns::capture_due`] in its own task.
    pub async fn observe(&self, events: &[Event]) -> bool {
        let mut due = false;
        for event in events {
            let (nexts, gone, process) = {
                let mut st = self.lock();
                let before = st.boundary.waiting_inputs();
                let nexts = st.boundary.on_event(event);
                let after = st.boundary.waiting_inputs();
                let gone: Vec<InputId> =
                    before.into_iter().filter(|i| !after.contains(i)).collect();
                (nexts, gone, st.process.clone())
            };
            let mut joined = None;
            for next in nexts {
                match next {
                    Next::Nothing => {}
                    Next::TurnStarted { turn, start } => {
                        // `turn.started` takes the oldest written input.
                        joined = gone.first().cloned();
                        self.turn_started(&turn, joined.as_ref(), &start, process.clone())
                            .await;
                    }
                    Next::CaptureEnd { .. } => due = true,
                    Next::Finalize { turn, end } => self.finalize(&turn, &end).await,
                }
            }
            let reason = if matches!(event, Event::SessionEnded { .. }) {
                REASON_SESSION_ENDED
            } else {
                REASON_NO_TURN
            };
            for input in gone.iter().filter(|i| Some(*i) != joined.as_ref()) {
                let app_turn = self.lock().by_input.remove(input);
                if let Some(app_turn) = app_turn {
                    log_store(
                        "abandon",
                        self.store
                            .abandon_turn_checkpoint(app_turn, reason.to_owned())
                            .await,
                    );
                }
            }
        }
        due
    }

    async fn turn_started(
        &self,
        turn: &TurnId,
        input: Option<&InputId>,
        start: &Side,
        process: Option<ProcessIncarnation>,
    ) {
        let planned = input.and_then(|i| self.lock().by_input.remove(i));
        let app_turn = match planned {
            Some(app_turn) => {
                log_store(
                    "bind turn",
                    self.store
                        .bind_checkpoint_turn(app_turn.clone(), turn.clone())
                        .await,
                );
                app_turn
            }
            None => {
                let app_turn: AppTurnId = new_id(self.ids.as_ref());
                let row = self.new_row(&app_turn, process, None, Some(turn), start, None);
                if let Err(e) = self.store.plan_turn_checkpoint(row).await {
                    tracing::error!(error = %e, "provider turn checkpoint row not stored");
                    return;
                }
                app_turn
            }
        };
        self.lock().by_turn.insert(turn.clone(), app_turn);
    }

    async fn run_due(&self) {
        let (turn, app_turn) = {
            let mut st = self.lock();
            let Some(turn) = st.boundary.take_due() else {
                return;
            };
            let app_turn = st.by_turn.get(&turn).cloned();
            (turn, app_turn)
        };
        let side = match &app_turn {
            Some(app_turn) => {
                // The row says `capturing` before the ref can exist.
                log_store(
                    "end capturing",
                    self.store
                        .record_checkpoint_side(app_turn.clone(), Which::End, RowSide::Capturing)
                        .await,
                );
                self.capture(app_turn, Phase::Completion).await
            }
            None => Side::Unavailable(REASON_CAPTURE_FAILED.to_owned()),
        };
        let next = self.lock().boundary.end_captured(&turn, side);
        if let Some(Next::Finalize { turn, end }) = next {
            self.finalize(&turn, &end).await;
        }
    }

    async fn finalize(&self, turn: &TurnId, end: &Side) {
        let app_turn = {
            let mut st = self.lock();
            st.boundary.finalized(turn);
            st.by_turn.remove(turn)
        };
        let Some(app_turn) = app_turn else {
            tracing::warn!(turn = %turn, "a finalized turn without a checkpoint row");
            return;
        };
        log_store(
            "end side",
            self.store
                .record_checkpoint_side(app_turn.clone(), Which::End, row_side(end))
                .await,
        );
        let row = match self.store.turn_checkpoint(app_turn).await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(error = %e, "turn checkpoint row not read");
                return;
            }
        };
        log_store(
            "turn.finalized",
            settle::finalize_row(&self.store, &row, self.options.clone()).await,
        );
    }
}

#[cfg(test)]
#[path = "turns_tests.rs"]
pub(super) mod turns_tests;
