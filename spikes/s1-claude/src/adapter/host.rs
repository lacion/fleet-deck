//! Serializable host commands.
//!
//! The recorder logs each command as a `host` fixture record, and the replay
//! harness applies the same commands to a fresh core at the same positions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AdapterError, Answer, ClaudeCore, Output};

/// One command of the host (Fleet Deck) to the adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostCmd {
    Initialize,
    Prompt {
        text: String,
        /// Client uuid of the input (see [`ClaudeCore::prompt`]).
        input_id: String,
    },
    Answer {
        request_id: String,
        option_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Interrupt,
    SetMode {
        mode: String,
    },
    SetModel {
        model: Option<String>,
    },
}

impl ClaudeCore {
    /// Applies a host command.
    ///
    /// # Errors
    /// The error of the command (see the per-command methods).
    pub fn apply(&mut self, cmd: &HostCmd) -> Result<Vec<Output>, AdapterError> {
        Ok(match cmd {
            HostCmd::Initialize => self.initialize().1,
            HostCmd::Prompt { text, input_id } => self.prompt(text, input_id),
            HostCmd::Answer {
                request_id,
                option_id,
                updated_input,
                message,
            } => {
                let answer = Answer {
                    option_id: option_id.clone(),
                    updated_input: updated_input.clone(),
                    message: message.clone(),
                };
                self.answer(request_id, &answer)?.1
            }
            HostCmd::Interrupt => self.interrupt()?.1,
            HostCmd::SetMode { mode } => self.set_mode(mode)?.1,
            HostCmd::SetModel { model } => self.set_model(model.as_deref()).1,
        })
    }
}
