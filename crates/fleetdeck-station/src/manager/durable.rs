//! The durable driver of a shim session (ADR-005 §2.1, §4.1, §4.3;
//! DL-110, DL-112).
//!
//! The driver runs a sans-IO adapter core ([`DurableCore`]: Codex in
//! [`super::shim_codex`], Claude in [`super::shim_claude`]) over a
//! [`ShimProcess`] and makes every adapter transition recoverable:
//! - **Output.** For each complete stdout line the pump feeds the core and
//!   commits, in one transaction, the events (with the shim's `read_at` as
//!   the proven request time), the new checkpoint, the frames that the line
//!   generated, and the cursor after the line. It acknowledges the cursor
//!   only after that commit, then sends the frames.
//! - **Commands.** A command effect is recorded as a transition (with its
//!   events and its frame count) before its frames are claimed; each frame
//!   is claimed before it leaves ([`super::shim_stdin::send_input`]). A
//!   frame that the shim proves unwritten undoes the transition.
//! - **Frames.** A generated frame is claimed before it leaves and marked
//!   done after the shim reported it written (fenced by the owner
//!   generation and the claim, DL-111).
//! - **Restore.** [`DurableDriver::restore`] restores the latest checkpoint
//!   into a new core, applies the transitions that may have reached the
//!   provider, settles claimed frames with `write_status`, and replays the
//!   output from the committed cursor. It never repeats a handshake or a
//!   native resume.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use fleetdeck_providers::adapter::{HostInput, ProviderError};
use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::checkpoint::CheckpointError;
use fleetdeck_providers::host::shim::host::{Attached, ShimProcess};
use fleetdeck_providers::host::shim::stdin::WriteOutcome;
use fleetdeck_providers::host::shim::wire::{StatusAnswer, WriteState};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::{Mutex, MutexGuard, RwLock, RwLockWriteGuard, watch};
use tokio::task::JoinHandle;

use fleetdeck_providers::host::shim::wire::GapReason;

use super::shim_stdin::{self, Delivery, Gate, frame_count, no_gate};
use crate::store::shim_gaps::REASON_ADAPTER_STATE_GAP;
use crate::store::{
    Dispatch, Effect, EffectPayload, FrameState, IdentitySignal, InputRecord, NewEvent,
    OutputCommit, OutputMeta, RequestTime, Store, StoreError,
};

/// The `input_blocked` reason of a generated frame whose fate the shim
/// cannot prove.
pub const REASON_FRAME_UNKNOWN: &str = "internal_frame_unknown";
/// The `not_dispatched` reason of a coordinator input that the provider
/// would queue behind a running turn.
pub const REASON_PROVIDER_BUSY: &str = "provider_busy";
/// How long a handshake step waits.
const OPEN_WAIT: Duration = Duration::from_secs(15);
/// How long the pump waits for the shim's exit state after EOF.
const EXIT_WAIT: Duration = Duration::from_secs(10);
/// The `error` code of an overlong provider line (ADR-005 §4.2).
pub const REASON_OUTPUT_FRAME_TOO_LARGE: &str = "output_frame_too_large";

/// Why the driver cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the adapter checkpoint cannot be restored: {0}")]
    Checkpoint(String),
    #[error("the provider refused the command: {0}")]
    Provider(#[from] ProviderError),
    #[error("the handshake did not finish: {0}")]
    Handshake(&'static str),
}

/// The durable driver of one shim.
pub struct DurableDriver<C: DurableCore> {
    store: Store,
    shim_id: String,
    process: Arc<ShimProcess>,
    config: C::Config,
    core: Mutex<C>,
    /// Taken for reading per output line; a test holds it to stop the pump
    /// between two lines (a crash point).
    gate: RwLock<()>,
    changed: watch::Sender<u64>,
    pump: StdMutex<Option<JoinHandle<()>>>,
    /// Set after a committed `below_floor` gap: the adapter state cannot
    /// follow the provider any more (ADR-005 §4.2; DL-122).
    adapter_gap: std::sync::atomic::AtomicBool,
    /// The offset after the last committed output (DL-194 §9.7).
    committed: std::sync::atomic::AtomicU64,
    /// Set after the commit of the provider's exit.
    exit_committed: std::sync::atomic::AtomicBool,
    /// The held bindings of this shim, released when the driver caught up
    /// ([`Self::set_release`]).
    release: StdMutex<Option<release::Held>>,
}

impl<C: DurableCore> std::fmt::Debug for DurableDriver<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableDriver")
            .field("shim_id", &self.shim_id)
            .finish_non_exhaustive()
    }
}

/// Events, frames and root identities of one core step. The identities are
/// in order with the events (`IdentitySignal::after`; DL-194).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Steps {
    pub events: Vec<NewEvent>,
    pub frames: Vec<String>,
    pub identities: Vec<IdentitySignal>,
}

/// A sans-IO adapter core that a [`DurableDriver`] can run.
pub trait DurableCore: Send + Sync + Sized + 'static {
    type Config: Clone + Send + Sync + 'static;
    /// The core of a new provider process (no output).
    fn create(config: Self::Config) -> Self;
    /// The checkpoint of the core.
    ///
    /// # Errors
    /// A state that does not serialize.
    fn save(&self) -> Result<AdapterCheckpoint, CheckpointError>;
    /// A core in the state of `checkpoint`; writes no frame.
    ///
    /// # Errors
    /// An unsupported or corrupt checkpoint.
    fn load(config: Self::Config, checkpoint: &AdapterCheckpoint) -> Result<Self, CheckpointError>;
    /// Feeds one complete stdout line (without its newline).
    fn line(&mut self, line: &str) -> Steps;
    /// Reports the provider's exit.
    fn exit(&mut self, code: Option<i32>) -> Steps;
    /// Applies one command effect.
    ///
    /// # Errors
    /// The core's refusal; the core state is restored by the caller.
    fn command(&mut self, effect: &Effect) -> Result<Steps, ProviderError>;
    /// The provider process ended.
    fn ended(&self) -> bool;
    /// No turn runs, no input waits, no background work runs (a graceful
    /// stop then uses the short idle grace, DL-141).
    fn is_idle(&self) -> bool {
        false
    }
}

/// The host input of a stored input record.
#[must_use]
pub fn host_input(record: &InputRecord) -> HostInput {
    HostInput {
        id: record.input_id.clone(),
        origin: record.origin,
        input: record.input.clone(),
    }
}

/// The refusal of an effect that is not a provider command.
#[must_use]
pub fn not_a_command() -> ProviderError {
    ProviderError::NotReady("not_a_shim_command")
}

fn snapshot<C: DurableCore>(core: &C) -> Result<AdapterCheckpoint, DriverError> {
    core.save()
        .map_err(|e| DriverError::Checkpoint(e.to_string()))
}

fn rebuild<C: DurableCore>(
    config: &C::Config,
    checkpoint: &AdapterCheckpoint,
) -> Result<C, DriverError> {
    C::load(config.clone(), checkpoint).map_err(|e| DriverError::Checkpoint(e.to_string()))
}

impl<C: DurableCore> DurableDriver<C> {
    pub(super) fn new(store: &Store, attached: &Attached, config: C::Config, core: C) -> Arc<Self> {
        Arc::new(Self {
            store: store.clone(),
            shim_id: attached.record.shim_id.clone(),
            process: Arc::clone(&attached.process),
            config,
            core: Mutex::new(core),
            gate: RwLock::new(()),
            changed: watch::channel(0).0,
            pump: StdMutex::new(None),
            adapter_gap: std::sync::atomic::AtomicBool::new(false),
            committed: std::sync::atomic::AtomicU64::new(
                attached.record.cursor.max(attached.floor),
            ),
            exit_committed: std::sync::atomic::AtomicBool::new(false),
            release: StdMutex::new(None),
        })
    }

    /// Drives a launched or reattached shim with a new core and no
    /// handshake (for a core whose provider needs none, and for tests).
    #[must_use]
    pub fn attach(store: &Store, attached: Attached, config: C::Config) -> Arc<Self> {
        let this = Self::new(store, &attached, config.clone(), C::create(config));
        this.start_pump(attached);
        this
    }

    pub(super) fn start_pump(self: &Arc<Self>, attached: Attached) {
        let start = attached.record.cursor.max(attached.floor);
        let this = Arc::clone(self);
        let task = tokio::spawn(async move { this.run_pump(attached, start).await });
        *self
            .pump
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }

    /// Restores the driver of a reattached shim (see the module
    /// documentation). Call it after [`super::shims::recover_shims`]
    /// settled the claimed stdin effects of the shim.
    ///
    /// # Errors
    /// A missing, unsupported or corrupt checkpoint, or a store error.
    pub async fn restore(
        store: &Store,
        attached: Attached,
        config: C::Config,
        in_flight_wait: Duration,
    ) -> Result<Arc<Self>, DriverError> {
        let shim_id = attached.record.shim_id.clone();
        let text = store
            .shim_checkpoint(shim_id.clone())
            .await?
            .ok_or_else(|| DriverError::Checkpoint("no checkpoint was committed".into()))?;
        let checkpoint = AdapterCheckpoint::from_json(&text)
            .map_err(|e| DriverError::Checkpoint(e.to_string()))?;
        let mut core: C = rebuild(&config, &checkpoint)?;
        for t in store.shim_transitions(shim_id.clone()).await? {
            let unsent = t.frames > 0 && t.claimed == 0;
            let retired = t.effect.state == crate::store::EffectState::RetiredNotDispatched;
            if unsent || retired {
                // The command never reached the provider; generic recovery
                // settles a still claimed effect.
                store.shim_drop_transition(shim_id.clone(), t.seq).await?;
                continue;
            }
            // The provider may have read it: the adapter state includes it.
            drop(core.command(&t.effect));
        }
        let this = Self::new(store, &attached, config, core);
        if this.below_floor(&attached).await? {
            // No frame goes out from an adapter state that missed output.
            this.start_pump(attached);
            return Ok(this);
        }
        shim_stdin::settle_open_frames(
            &this.store,
            &this.process,
            &this.shim_id,
            in_flight_wait,
            REASON_FRAME_UNKNOWN,
        )
        .await?;
        let guard = this.core.lock().await;
        this.flush(&guard).await?;
        drop(guard);
        this.start_pump(attached);
        Ok(this)
    }

    /// Runs an adapter-internal step (a handshake step): commits its events,
    /// checkpoint and frames like an output transaction without new output,
    /// then sends the frames.
    pub(super) async fn internal(
        &self,
        op: impl FnOnce(&mut C) -> Result<Steps, ProviderError>,
    ) -> Result<(), DriverError> {
        self.internal_gated(op, &no_gate).await.map(|_| ())
    }

    /// [`Self::internal`] with the authority `gate` (the first input of a
    /// launch). The commit appends the events to the session log and cannot
    /// be undone, so the gate runs after the core lock and right before the
    /// core step and the commit (no await in between); only local database
    /// steps and the frame writes follow. `Some` is the refusal: nothing
    /// was changed or committed.
    pub(super) async fn internal_gated(
        &self,
        op: impl FnOnce(&mut C) -> Result<Steps, ProviderError>,
        gate: &Gate<'_>,
    ) -> Result<Option<Dispatch>, DriverError> {
        let mut core = self.core.lock().await;
        // Checked under the core lock: a gap commit holds the same lock, so
        // a gap committed while this call waited is seen here.
        if self.has_adapter_gap() {
            return Err(ProviderError::NotReady(REASON_ADAPTER_STATE_GAP).into());
        }
        if let Some(refused) = gate().await {
            return Ok(Some(refused));
        }
        let steps = op(&mut core)?;
        let checkpoint = snapshot(&*core)?;
        let cursor = self.process.acked();
        self.commit(cursor, steps, RequestTime::Now, checkpoint, false)
            .await?;
        self.flush(&core).await?;
        Ok(None)
    }

    /// Commits one output transaction. `exited` marks the commit of the
    /// provider's exit (DL-194).
    async fn commit(
        &self,
        cursor: u64,
        steps: Steps,
        read_at: RequestTime,
        checkpoint: AdapterCheckpoint,
        exited: bool,
    ) -> Result<bool, StoreError> {
        let result = self
            .store
            .shim_commit_output_with(
                self.shim_id.clone(),
                self.process.generation(),
                cursor,
                steps.events,
                OutputMeta {
                    read_at,
                    // After an adapter state gap no checkpoint is stored:
                    // the state is incomplete (§4.2).
                    checkpoint: (!self.has_adapter_gap()).then_some(checkpoint),
                    frames: steps.frames,
                    identities: steps.identities,
                    exited,
                },
            )
            .await?;
        self.changed.send_modify(|n| *n += 1);
        let committed = matches!(result, OutputCommit::Committed(_));
        if committed {
            self.note_committed(cursor, exited);
        }
        Ok(committed)
    }

    /// Sends the pending generated frames in order (the caller holds the
    /// core lock, so frames and command frames keep their order).
    async fn flush(&self, _core: &MutexGuard<'_, C>) -> Result<(), StoreError> {
        for frame in self.store.shim_open_frames(self.shim_id.clone()).await? {
            if frame.state != FrameState::Pending {
                // A claimed frame of an earlier attachment: settled first.
                return Ok(());
            }
            let Some(tag) = self
                .store
                .shim_claim_frame(
                    self.shim_id.clone(),
                    self.process.generation(),
                    frame.frame_seq,
                )
                .await?
            else {
                return Ok(());
            };
            let mut line = frame.payload.into_bytes();
            line.push(b'\n');
            let next = match self.process.write_tagged(&tag, &line).await {
                Ok(WriteOutcome::Done(WriteState::Written)) => Some(FrameState::Done),
                Ok(
                    WriteOutcome::Done(
                        WriteState::FailedUnwritten { .. } | WriteState::ClosedUnwritten,
                    )
                    | WriteOutcome::Refused(StatusAnswer::NotReceived),
                ) => Some(FrameState::Pending),
                _ => None,
            };
            match next {
                Some(state) => {
                    let fenced = self
                        .store
                        .shim_frame_state(
                            self.shim_id.clone(),
                            self.process.generation(),
                            frame.frame_seq,
                            tag,
                            state,
                        )
                        .await?;
                    if !fenced || state != FrameState::Done {
                        // A newer owner took over (DL-111), or stdin is
                        // gone and the frame stays for a later attachment.
                        return Ok(());
                    }
                }
                None => {
                    // The frame may have left: it stays claimed for recovery.
                    self.store
                        .shim_block_input(self.shim_id.clone(), REASON_FRAME_UNKNOWN)
                        .await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub(super) async fn wait(
        &self,
        ready: impl Fn(&C) -> bool,
        what: &'static str,
    ) -> Result<(), DriverError> {
        self.wait_until(ready, tokio::time::Instant::now() + OPEN_WAIT, what)
            .await
    }

    /// Waits until `ready` holds for the core after a commit, until
    /// `deadline`. A core that ended or a passed deadline is
    /// [`DriverError::Handshake`] with `what`.
    pub(super) async fn wait_until(
        &self,
        ready: impl Fn(&C) -> bool,
        deadline: tokio::time::Instant,
        what: &'static str,
    ) -> Result<(), DriverError> {
        let mut changed = self.changed.subscribe();
        loop {
            {
                let core = self.core.lock().await;
                if ready(&core) {
                    return Ok(());
                }
                if core.ended() {
                    return Err(DriverError::Handshake(what));
                }
            }
            if tokio::time::timeout_at(deadline, changed.changed())
                .await
                .is_err()
            {
                return Err(DriverError::Handshake(what));
            }
        }
    }

    /// Dispatches one claimed command effect that carries no authority (see
    /// the module documentation).
    pub async fn apply(&self, effect: &Effect) -> Dispatch {
        self.apply_gated(effect, &no_gate).await
    }

    /// [`Self::apply`] with the authority `gate`, which runs once per effect
    /// that has an authority, under the core lock:
    /// - an answer: before the core step, so that a refusal produces no
    ///   `approval.resolved` and the request stays open;
    /// - an effect without frames (a pending mode, an input that the core
    ///   would queue): after the core step and before the transition. A
    ///   coordinator input is never queued (`provider_busy`);
    /// - any other effect: after the transition and the claim of the first
    ///   frame, right before the first byte. Later frames are not gated
    ///   again (the write has begun). A refusal undoes the transition like
    ///   `Retired`.
    pub async fn apply_gated(&self, effect: &Effect, gate: &Gate<'_>) -> Dispatch {
        match self.apply_inner(effect, gate).await {
            Ok(dispatch) => dispatch,
            Err(DriverError::Provider(e)) => Dispatch::NotDispatched(e.to_string()),
            Err(e) => Dispatch::Unknown(e.to_string()),
        }
    }

    async fn apply_inner(&self, effect: &Effect, gate: &Gate<'_>) -> Result<Dispatch, DriverError> {
        let mut core = self.core.lock().await;
        // Checked under the core lock (see `internal`); the store also
        // refuses the transition and every frame claim after a gap.
        if self.has_adapter_gap() {
            // Proven not sent: nothing goes to the provider from an
            // incomplete adapter state.
            return Ok(Dispatch::NotDispatched(REASON_ADAPTER_STATE_GAP.to_owned()));
        }
        // An answer is gated before the core step: the step emits
        // `approval.resolved`, which the transition appends to the log.
        let has_authority = effect.payload.authority().is_some();
        let answer_gated = has_authority && matches!(effect.payload, EffectPayload::Answer { .. });
        if answer_gated && let Some(refused) = gate().await {
            return Ok(refused);
        }
        let before = snapshot(&*core)?;
        // A command states no root identity: `identities` stays unused.
        let Steps { events, frames, .. } = match core.command(effect) {
            Ok(steps) => steps,
            Err(e) => {
                *core = rebuild(&self.config, &before)?;
                return Err(e.into());
            }
        };
        if has_authority && frames.is_empty() {
            // No frame: the state change alone would reach the provider
            // later (a pending mode, a queued input) with no gate.
            if matches!(
                effect.payload,
                EffectPayload::Prompt { .. }
                    | EffectPayload::FollowUp { .. }
                    | EffectPayload::Steer { .. }
            ) {
                *core = rebuild(&self.config, &before)?;
                return Ok(Dispatch::NotDispatched(REASON_PROVIDER_BUSY.to_owned()));
            }
            if !answer_gated && let Some(refused) = gate().await {
                *core = rebuild(&self.config, &before)?;
                return Ok(refused);
            }
        }
        let bytes: Vec<u8> = frames
            .iter()
            .flat_map(|f| f.bytes().chain(std::iter::once(b'\n')))
            .collect();
        let count = frame_count(&bytes)?;
        let Some(seq) = self
            .store
            .shim_begin_transition(
                self.shim_id.clone(),
                self.process.generation(),
                effect.effect_id.clone(),
                count,
                events,
            )
            .await?
        else {
            *core = rebuild(&self.config, &before)?;
            if self
                .store
                .shim_has_adapter_gap(self.shim_id.clone())
                .await?
            {
                self.note_adapter_gap();
                return Ok(Dispatch::NotDispatched(REASON_ADAPTER_STATE_GAP.to_owned()));
            }
            return Ok(Dispatch::NotDispatched("stale_shim_generation".into()));
        };
        if bytes.is_empty() {
            return Ok(Dispatch::Sent);
        }
        // `None`: an answer was gated before the core step.
        let gate = (!answer_gated).then_some(gate);
        let undone = match shim_stdin::deliver(
            &self.store,
            &self.process,
            &self.shim_id,
            &effect.effect_id,
            &bytes,
            gate,
        )
        .await?
        {
            Delivery::Done(dispatch) => return Ok(dispatch),
            Delivery::Undone(dispatch) => dispatch,
        };
        *core = rebuild(&self.config, &before)?;
        self.store
            .shim_drop_transition(self.shim_id.clone(), seq)
            .await?;
        Ok(undone)
    }

    async fn run_pump(self: Arc<Self>, attached: Attached, start: u64) {
        let mut reader = BufReader::new(attached.stdout);
        let mut offset = start;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line).await;
            let line_start = offset;
            let line_end = line_start + line.len() as u64;
            let gaps = self.process.gaps();
            if !self.has_adapter_gap()
                && let Some((from, to, _)) = gaps
                    .iter()
                    .find(|(_, _, reason)| *reason == GapReason::BelowFloor)
                && !self.adapter_gap_at(*from, *to).await
            {
                return;
            }
            if self.has_adapter_gap() {
                // Read and acknowledge without parsing, so the provider is
                // never blocked on a full pipe (§4.2).
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                offset = line_end;
                if !self.skip_to(offset).await {
                    return;
                }
                continue;
            }
            // A gap reported by the shim ends the parsing at its start: the
            // bytes around it are not one provider line (ADR-005 §4.2).
            if gaps.iter().any(|(from, _, _)| *from < line_end) {
                match self.commit_gaps(&mut reader, offset, &gaps).await {
                    Some(cursor) => offset = cursor,
                    None => return,
                }
                break;
            }
            match read {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.last() != Some(&b'\n') {
                // The provider ended inside a line (EOF).
                match self.commit_unterminated(line_start, line_end).await {
                    Some(cursor) => offset = cursor,
                    None => return,
                }
                break;
            }
            offset = line_end;
            let _gate = self.gate.read().await;
            let mut core = self.core.lock().await;
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let steps = core.line(&text);
            let read_at = self
                .process
                .read_at(line_start)
                .and_then(|t| i64::try_from(t).ok())
                .map_or(RequestTime::Unprovable, RequestTime::Read);
            let Ok(checkpoint) = snapshot(&*core) else {
                return;
            };
            match self.commit(offset, steps, read_at, checkpoint, false).await {
                Ok(true) => {}
                // Fenced by a newer owner, or a database error: this
                // driver stops without touching the provider.
                Ok(false) | Err(_) => return,
            }
            if !self.process.ack(offset).await || self.flush(&core).await.is_err() {
                return;
            }
        }
        self.commit_exit(offset).await;
    }

    /// Stops the pump between two lines until the guard drops (a test hook
    /// for a crash point: committed output stays committed, later output
    /// stays unacknowledged at the shim).
    pub async fn hold_output(&self) -> RwLockWriteGuard<'_, ()> {
        self.gate.write().await
    }

    /// Ends this driver without any provider I/O, as a station crash does:
    /// the pump stops and the attachment drops; the shim keeps the provider.
    pub async fn abandon(&self) {
        let task = self
            .pump
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
            // A cancelled pump never runs again; the error is the abort.
            let _aborted = task.await;
        }
    }

    /// Reads the core under its lock.
    pub async fn with_core<T>(&self, read: impl FnOnce(&C) -> T) -> T {
        read(&*self.core.lock().await)
    }

    /// The shim process.
    #[must_use]
    pub fn process(&self) -> &Arc<ShimProcess> {
        &self.process
    }

    /// True when the core is idle. A core that a step holds (or a poisoned
    /// one) counts as not idle.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.core.try_lock().is_ok_and(|core| core.is_idle())
    }
}

#[path = "durable_gap.rs"]
mod gap;
#[path = "durable_output.rs"]
mod output;
#[path = "durable_release.rs"]
mod release;
