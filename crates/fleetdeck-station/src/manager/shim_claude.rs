//! The durable Claude driver of a shim session (ADR-005 §2.1; DL-112):
//! [`ClaudeCore`] as a [`DurableCore`] and the Claude handshake
//! (`initialize`) of a newly launched shim. A restored driver never repeats
//! the handshake ([`DurableDriver::restore`]).
//!
//! Prompt, steer and follow-up are one user message for the CLI (a message
//! written during a turn joins it, one written while a turn waits queues;
//! ADR-001 §2.3), the same as the direct driver.

use std::sync::Arc;

use fleetdeck_providers::adapter::{Answer, ProviderError};
use fleetdeck_providers::checkpoint::{AdapterCheckpoint, CheckpointError};
use fleetdeck_providers::claude::frames::parse_line;
use fleetdeck_providers::claude::{ClaudeCore, CoreConfig, Output};
use fleetdeck_providers::host::shim::host::Attached;

use super::durable::{DriverError, DurableCore, DurableDriver, Steps, host_input, not_a_command};
use crate::store::{Effect, EffectPayload, NewEvent, Store};

/// The durable Claude driver of one shim.
pub type DurableClaude = DurableDriver<ClaudeCore>;

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

impl DurableCore for ClaudeCore {
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
        match parse_line(line) {
            Ok(frame) => split(self.on_frame(frame)),
            Err(_) => (Vec::new(), Vec::new()),
        }
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        split(self.on_exit(code))
    }

    fn command(&mut self, effect: &Effect) -> Result<Steps, ProviderError> {
        let out = match &effect.payload {
            EffectPayload::Prompt { input }
            | EffectPayload::FollowUp { input }
            | EffectPayload::Steer { input } => self.prompt(&host_input(input)),
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
            // The first input of a launch (the start effect is claimed
            // while it is written, DL-113).
            EffectPayload::Start {
                prompt: Some(input),
                ..
            } => self.prompt(&host_input(input)),
            EffectPayload::Start { prompt: None, .. }
            | EffectPayload::Resume { .. }
            | EffectPayload::Stop { .. }
            | EffectPayload::Revert { .. }
            | EffectPayload::Worktree { .. } => Err(not_a_command()),
        }?;
        Ok(split(out))
    }

    fn ended(&self) -> bool {
        self.has_ended()
    }

    fn is_idle(&self) -> bool {
        Self::is_idle(self)
    }
}

impl DurableDriver<ClaudeCore> {
    /// Drives a newly launched shim: commits the first checkpoint, then
    /// `initialize`, and waits for its response (the advertised models).
    ///
    /// # Errors
    /// A store error, or a handshake that does not finish.
    pub async fn open(
        store: &Store,
        attached: Attached,
        config: CoreConfig,
    ) -> Result<Arc<Self>, DriverError> {
        let this = Self::new(store, &attached, config.clone(), ClaudeCore::new(config));
        this.start_pump(attached);
        this.internal(|c| c.initialize().map(split)).await?;
        this.wait(|c| c.models().is_some(), "initialize").await?;
        Ok(this)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use fleetdeck_proto::InputId;
    use fleetdeck_proto::command::UserInput;
    use fleetdeck_proto::event::RunOrigin;
    use fleetdeck_providers::adapter::HostInput;
    use fleetdeck_providers::claude::capabilities;

    use super::*;

    #[test]
    fn the_durable_claude_core_is_idle_until_an_input_waits() {
        let mut core = <ClaudeCore as DurableCore>::create(CoreConfig {
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            capabilities: capabilities(false),
            resumed: None,
        });
        assert!(DurableCore::is_idle(&core));
        core.prompt(&HostInput {
            id: InputId::from("01J00000000000000000000001"),
            origin: RunOrigin::Board,
            input: UserInput::new("hello".to_owned()),
        })
        .unwrap();
        assert!(!DurableCore::is_idle(&core));
    }
}
