//! The durable Codex driver of a shim session (ADR-005 §2.1; DL-110,
//! DL-112): [`CodexCore`] as a [`DurableCore`] and the Codex handshake
//! (`initialize`, `thread/start`) of a newly launched shim. A restored
//! driver never repeats the handshake ([`DurableDriver::restore`]).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_providers::adapter::{Answer, ProviderError};
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, CheckpointError};
use fleetdeck_providers::codex::{CodexCore, CoreConfig, Output, SessionSpec, rpc};
use fleetdeck_providers::host::shim::host::Attached;

pub use super::durable::{DriverError, REASON_FRAME_UNKNOWN};
use super::durable::{DurableCore, DurableDriver, Steps, host_input, not_a_command};
use crate::store::{Effect, EffectPayload, NewEvent, Store};

/// The durable Codex driver of one shim.
pub type DurableCodex = DurableDriver<CodexCore>;

fn split(out: Vec<Output>) -> Steps {
    let mut events = Vec::new();
    let mut frames = Vec::new();
    for o in out {
        match o {
            Output::Emit(event) => events.push(NewEvent::from(event)),
            Output::Write(frame) => frames.push(frame.to_string()),
        }
    }
    (events, frames)
}

impl DurableCore for CodexCore {
    type Config = CoreConfig;

    fn create(config: CoreConfig) -> Self {
        Self::new(config)
    }

    fn save(&self) -> Result<AdapterCheckpoint, CheckpointError> {
        self.checkpoint()
    }

    fn load(config: CoreConfig, checkpoint: &AdapterCheckpoint) -> Result<Self, CheckpointError> {
        Self::restore(config, checkpoint)
    }

    fn line(&mut self, line: &str) -> Steps {
        match rpc::parse_line(line) {
            Ok(frame) => split(self.on_frame(frame)),
            Err(_) => (Vec::new(), Vec::new()),
        }
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        split(self.on_exit(code))
    }

    fn command(&mut self, effect: &Effect) -> Result<Steps, ProviderError> {
        let out = match &effect.payload {
            EffectPayload::Prompt { input } | EffectPayload::FollowUp { input } => {
                self.prompt(&host_input(input))
            }
            EffectPayload::Steer { input } => self.steer(&host_input(input)),
            EffectPayload::Interrupt => self.interrupt(Some(effect.effect_id.as_str().to_owned())),
            EffectPayload::Answer {
                request_id,
                decision,
                by,
            } => self.answer(&Answer {
                request: request_id.clone(),
                decision: decision.clone(),
                by: *by,
            }),
            EffectPayload::SetMode { mode } => self.set_mode(mode),
            EffectPayload::SetModel { model } => self.set_model(model),
            // The first input of a launch (the start or resume effect is
            // claimed while it is written, DL-113, DL-182).
            EffectPayload::Start {
                prompt: Some(input),
                ..
            }
            | EffectPayload::Resume {
                prompt: Some(input),
                ..
            } => self.prompt(&host_input(input)),
            EffectPayload::Start { prompt: None, .. }
            | EffectPayload::Resume { prompt: None, .. }
            | EffectPayload::Stop { .. }
            | EffectPayload::Revert { .. }
            | EffectPayload::Worktree { .. } => Err(not_a_command()),
        }?;
        Ok(split(out))
    }

    fn ended(&self) -> bool {
        self.has_ended()
    }
}

impl DurableDriver<CodexCore> {
    /// Rolls the conversation back by `turns` turns (`thread/rollback`;
    /// DL-116), the shim-host form of the direct driver's rollback. A
    /// refusal before the frame is committed is definite; once the
    /// `thread/rollback` state and frame are committed, every failure is
    /// [`ProviderError::Uncertain`] except the provider's own refusal. The
    /// committed `Sent` state survives a station restart, so the restored
    /// core still refuses conversation input until the answer arrives.
    ///
    /// # Errors
    /// See above.
    pub async fn rollback(&self, turns: u32, timeout: Duration) -> Result<(), ProviderError> {
        match self.internal(|c| c.rollback(turns).map(split)).await {
            Ok(()) => {}
            Err(DriverError::Provider(e)) => return Err(e),
            Err(e) => return Err(ProviderError::Uncertain(e.to_string())),
        }
        let deadline = tokio::time::Instant::now() + timeout;
        if self
            .wait_until(|c| c.rollback_answer().is_some(), deadline, "rollback")
            .await
            .is_err()
        {
            return Err(ProviderError::Uncertain(
                "no rollback answer before the deadline or the provider ended".to_owned(),
            ));
        }
        match self.with_core(CodexCore::rollback_answer).await {
            Some(Ok(())) => Ok(()),
            Some(Err(refusal)) => Err(ProviderError::Refused(refusal)),
            None => Err(ProviderError::Uncertain(
                "the rollback answer went missing".to_owned(),
            )),
        }
    }

    /// Drives a newly launched shim: commits the first checkpoint, then the
    /// handshake and the thread (`initialize`, `thread/start`).
    ///
    /// # Errors
    /// A store error, or a handshake that does not finish.
    pub async fn open(
        store: &Store,
        attached: Attached,
        config: CoreConfig,
        thread: &SessionSpec,
    ) -> Result<Arc<Self>, DriverError> {
        let this = Self::new(store, &attached, config.clone(), CodexCore::new(config));
        this.start_pump(attached);
        this.internal(|c| c.initialize().map(split)).await?;
        this.wait(CodexCore::is_initialized, "initialize").await?;
        this.internal(|c| c.open_thread(thread).map(split)).await?;
        this.wait(|c| !c.is_opening() && c.thread_id().is_some(), "thread")
            .await?;
        Ok(this)
    }
}
