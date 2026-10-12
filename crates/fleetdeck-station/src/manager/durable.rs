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
use fleetdeck_providers::host::ProcessControl as _;
use fleetdeck_providers::host::shim::host::{Attached, ShimProcess};
use fleetdeck_providers::host::shim::stdin::WriteOutcome;
use fleetdeck_providers::host::shim::wire::{StatusAnswer, WriteState};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::sync::{Mutex, MutexGuard, RwLock, RwLockWriteGuard, watch};
use tokio::task::JoinHandle;

use super::shim_stdin::{self, send_input};
use crate::store::{
    Dispatch, Effect, FrameState, InputRecord, NewEvent, OutputCommit, OutputMeta, RequestTime,
    Store, StoreError,
};

/// The `input_blocked` reason of a generated frame whose fate the shim
/// cannot prove.
pub const REASON_FRAME_UNKNOWN: &str = "internal_frame_unknown";
/// How long a handshake step waits.
const OPEN_WAIT: Duration = Duration::from_secs(15);
/// The pause between two `write_status` questions about an in-flight frame.
const ASK_AGAIN: Duration = Duration::from_millis(100);

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
}

impl<C: DurableCore> std::fmt::Debug for DurableDriver<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableDriver")
            .field("shim_id", &self.shim_id)
            .finish_non_exhaustive()
    }
}

/// Events and frames of one core step.
pub type Steps = (Vec<NewEvent>, Vec<String>);

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
        })
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
        this.settle_frames(in_flight_wait).await?;
        let guard = this.core.lock().await;
        this.flush(&guard).await?;
        drop(guard);
        this.start_pump(attached);
        Ok(this)
    }

    /// Settles generated frames that an earlier attachment claimed.
    async fn settle_frames(&self, wait: Duration) -> Result<(), StoreError> {
        for frame in self.store.shim_open_frames(self.shim_id.clone()).await? {
            let (FrameState::Claimed, Some(tag)) = (frame.state, &frame.tag) else {
                continue;
            };
            let deadline = tokio::time::Instant::now() + wait;
            let state = loop {
                let answer = self.process.write_status(tag, wait).await;
                match answer {
                    Some(StatusAnswer::Known {
                        state: WriteState::Received | WriteState::Writing { .. },
                    }) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(ASK_AGAIN).await;
                    }
                    Some(StatusAnswer::Known {
                        state: WriteState::Written,
                    }) => break Some(FrameState::Done),
                    Some(
                        StatusAnswer::NotReceived
                        | StatusAnswer::Known {
                            state: WriteState::FailedUnwritten { .. } | WriteState::ClosedUnwritten,
                        },
                    ) => break Some(FrameState::Pending),
                    _ => break None,
                }
            };
            match state {
                Some(state) => {
                    let fenced = self
                        .store
                        .shim_frame_state(
                            self.shim_id.clone(),
                            self.process.generation(),
                            frame.frame_seq,
                            tag.clone(),
                            state,
                        )
                        .await?;
                    if !fenced {
                        // A newer owner took over (DL-111): it settles.
                        return Ok(());
                    }
                }
                None => {
                    self.store
                        .shim_block_input(self.shim_id.clone(), REASON_FRAME_UNKNOWN)
                        .await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Runs an adapter-internal step (a handshake step): commits its events,
    /// checkpoint and frames like an output transaction without new output,
    /// then sends the frames.
    pub(super) async fn internal(
        &self,
        op: impl FnOnce(&mut C) -> Result<Steps, ProviderError>,
    ) -> Result<(), DriverError> {
        let mut core = self.core.lock().await;
        let (events, frames) = op(&mut core)?;
        let checkpoint = snapshot(&*core)?;
        let cursor = self.process.acked();
        self.commit(cursor, events, RequestTime::Now, checkpoint, frames)
            .await?;
        self.flush(&core).await?;
        Ok(())
    }

    async fn commit(
        &self,
        cursor: u64,
        events: Vec<NewEvent>,
        read_at: RequestTime,
        checkpoint: AdapterCheckpoint,
        frames: Vec<String>,
    ) -> Result<bool, StoreError> {
        let result = self
            .store
            .shim_commit_output_with(
                self.shim_id.clone(),
                self.process.generation(),
                cursor,
                events,
                OutputMeta {
                    read_at,
                    checkpoint: Some(checkpoint),
                    frames,
                },
            )
            .await?;
        self.changed.send_modify(|n| *n += 1);
        Ok(matches!(result, OutputCommit::Committed(_)))
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
        let mut changed = self.changed.subscribe();
        let deadline = tokio::time::Instant::now() + OPEN_WAIT;
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

    /// Dispatches one claimed command effect (see the module
    /// documentation).
    pub async fn apply(&self, effect: &Effect) -> Dispatch {
        match self.apply_inner(effect).await {
            Ok(dispatch) => dispatch,
            Err(DriverError::Provider(e)) => Dispatch::NotDispatched(e.to_string()),
            Err(e) => Dispatch::Unknown(e.to_string()),
        }
    }

    async fn apply_inner(&self, effect: &Effect) -> Result<Dispatch, DriverError> {
        let mut core = self.core.lock().await;
        let before = snapshot(&*core)?;
        let (events, frames) = match core.command(effect) {
            Ok(steps) => steps,
            Err(e) => {
                *core = rebuild(&self.config, &before)?;
                return Err(e.into());
            }
        };
        let bytes: Vec<u8> = frames
            .iter()
            .flat_map(|f| f.bytes().chain(std::iter::once(b'\n')))
            .collect();
        let count = u32::try_from(
            bytes
                .len()
                .div_ceil(fleetdeck_providers::host::shim::wire::MAX_PAYLOAD),
        )
        .map_err(|_| DriverError::Checkpoint("too many frames".into()))?;
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
            return Ok(Dispatch::NotDispatched("stale_shim_generation".into()));
        };
        if bytes.is_empty() {
            return Ok(Dispatch::Sent);
        }
        let sent = send_input(
            &self.store,
            &self.process,
            &self.shim_id,
            &effect.effect_id,
            &bytes,
        )
        .await?;
        Ok(match shim_stdin::settle_live(&sent) {
            fleetdeck_providers::host::shim::settle::EffectResult::Sent => Dispatch::Sent,
            fleetdeck_providers::host::shim::settle::EffectResult::Retired(reason) => {
                *core = rebuild(&self.config, &before)?;
                self.store
                    .shim_drop_transition(self.shim_id.clone(), seq)
                    .await?;
                Dispatch::NotDispatched(reason.to_owned())
            }
            fleetdeck_providers::host::shim::settle::EffectResult::Unknown { block_input } => {
                if block_input {
                    self.store
                        .shim_block_input(
                            self.shim_id.clone(),
                            crate::store::shim_writes::REASON_STDIN_PARTIAL,
                        )
                        .await?;
                }
                Dispatch::Unknown(crate::store::shim_writes::REASON_SHIM_UNKNOWN.to_owned())
            }
            fleetdeck_providers::host::shim::settle::EffectResult::AskAgain => {
                Dispatch::Unknown(crate::store::shim_writes::REASON_SHIM_UNKNOWN.to_owned())
            }
        })
    }

    async fn run_pump(self: Arc<Self>, attached: Attached, start: u64) {
        let mut reader = BufReader::new(attached.stdout);
        let mut offset = start;
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.last() != Some(&b'\n') {
                break;
            }
            let line_start = offset;
            offset += line.len() as u64;
            let _gate = self.gate.read().await;
            let mut core = self.core.lock().await;
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let (events, frames) = core.line(&text);
            let read_at = self
                .process
                .read_at(line_start)
                .and_then(|t| i64::try_from(t).ok())
                .map_or(RequestTime::Unprovable, RequestTime::Read);
            let Ok(checkpoint) = snapshot(&*core) else {
                break;
            };
            match self
                .commit(offset, events, read_at, checkpoint, frames)
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(_) => break,
            }
            if !self.process.ack(offset).await {
                break;
            }
            if self.flush(&core).await.is_err() {
                break;
            }
        }
        if let Some(report) = self.process.exit()
            && !matches!(
                report.termination,
                fleetdeck_providers::host::Termination::Unknown
            )
        {
            let mut core = self.core.lock().await;
            let code = match report.termination {
                fleetdeck_providers::host::Termination::Code(c) => Some(c),
                _ => None,
            };
            let (events, _) = core.exit(code);
            if let Ok(checkpoint) = snapshot(&*core)
                && matches!(
                    self.commit(offset, events, RequestTime::Now, checkpoint, Vec::new())
                        .await,
                    Ok(true)
                )
            {
                let _acked = self.process.ack_exit().await;
            }
        }
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
}
