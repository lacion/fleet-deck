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
use crate::store::{Effect, EffectPayload, IdentitySignal, NewEvent, Store};

/// The durable Claude driver of one shim.
pub type DurableClaude = DurableDriver<ClaudeCore>;

fn split(out: Vec<Output>) -> Steps {
    let mut steps = Steps::default();
    for o in out {
        match o {
            Output::Emit(event) => steps.events.push(NewEvent::from(event)),
            Output::Write(frame) => steps.frames.push(frame.to_string()),
            Output::NativeIdentity { native_id } => steps.identities.push(IdentitySignal {
                native_id,
                after: steps.events.len(),
            }),
        }
    }
    steps
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
            Err(_) => Steps::default(),
        }
    }

    fn exit(&mut self, code: Option<i32>) -> Steps {
        split(self.on_exit(code))
    }

    fn command(&mut self, effect: &Effect) -> Result<Steps, ProviderError> {
        let out = match &effect.payload {
            EffectPayload::Prompt { input, .. }
            | EffectPayload::FollowUp { input, .. }
            | EffectPayload::Steer { input, .. } => self.prompt(&host_input(input)),
            EffectPayload::Interrupt { .. } => {
                self.interrupt(Some(effect.effect_id.as_str().to_owned()))
            }
            EffectPayload::Answer {
                request_id,
                decision,
                by,
                ..
            } => self.answer(&Answer {
                request: request_id.clone(),
                decision: decision.clone(),
                by: *by,
            }),
            EffectPayload::SetMode { mode, .. } => self.set_mode(mode),
            EffectPayload::SetModel { model } => self.set_model(model),
            EffectPayload::SetEffort { effort } => {
                self.set_effort(effort, Some(effect.effect_id.as_str().to_owned()))
            }
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
            | EffectPayload::Worktree { .. }
            | EffectPayload::Land { .. }
            | EffectPayload::MoveIn { .. }
            | EffectPayload::StopExecution { .. } => Err(not_a_command()),
            // B8: the execution of `!` commands is a later task.
            EffectPayload::ShellCommand { .. } => {
                Err(ProviderError::NotReady("shell_command_not_built"))
            }
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
            usage_base: fleetdeck_providers::claude::UsageBase::Zero,
        });
        assert!(DurableCore::is_idle(&core));
        core.prompt(&HostInput {
            id: InputId::from("01J00000000000000000000001"),
            origin: RunOrigin::Board,
            input: UserInput::new("hello".to_owned()),
            context: Vec::new(),
        })
        .unwrap();
        assert!(!DurableCore::is_idle(&core));
    }

    /// The durable replay rebuilds the input from the stored effect: the
    /// `!` results of the record reach the CLI frame as before (B8).
    #[test]
    fn the_durable_replay_writes_the_shell_context_of_the_stored_effect() {
        use fleetdeck_providers::ShellContext;

        use crate::store::{EffectState, InputRecord};

        let mut core = <ClaudeCore as DurableCore>::create(CoreConfig {
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            capabilities: capabilities(false),
            resumed: None,
            usage_base: fleetdeck_providers::claude::UsageBase::Zero,
        });
        let record = InputRecord {
            input_id: InputId::from("01J00000000000000000000002"),
            origin: RunOrigin::Board,
            input: UserInput::new("go".to_owned()),
            shell_context: vec![ShellContext {
                command: "ls".to_owned(),
                end: "exit 0".to_owned(),
                stdout: "a<b\n".to_owned(),
                stderr: String::new(),
            }],
        };
        // The stored JSON is what the replay reads.
        let stored: InputRecord =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        let effect = Effect {
            effect_id: "e1".into(),
            command_id: None,
            session_id: "s1".into(),
            process_incarnation: None,
            payload: EffectPayload::Prompt {
                input: stored,
                authority: None,
            },
            state: EffectState::Claimed,
            fence_token: None,
            reason: None,
        };
        let steps = DurableCore::command(&mut core, &effect).unwrap();
        let frame: serde_json::Value = serde_json::from_str(&steps.frames[0]).unwrap();
        let blocks = frame["message"]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["text"], "<bash-input>ls</bash-input>");
        assert_eq!(
            blocks[1]["text"],
            "<bash-stdout>a&lt;b\n</bash-stdout><bash-stderr></bash-stderr>"
        );
        assert_eq!(blocks[2]["text"], "go");
    }
}
