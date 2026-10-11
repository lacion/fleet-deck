//! Serializable host commands. The recorder logs each command as a `host`
//! fixture record; the replay harness applies the same commands to a fresh
//! core at the same positions (PLAN §13, L20).

use fleetdeck_proto::command::{ApprovalDecision, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{InputId, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::core::{ClaudeCore, Output};
use crate::adapter::{Answer, HostInput, Result};

/// One command of the host (Fleet Deck) to the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostCmd {
    Initialize,
    Prompt {
        text: String,
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
        /// `None` is the default-model sentinel (trap 2).
        model: Option<String>,
    },
}

impl ClaudeCore {
    /// Applies a recorded host command. Recorded inputs and answers came from
    /// the board.
    ///
    /// # Errors
    /// The error of the command (see the per-command methods).
    pub fn apply(&mut self, cmd: &HostCmd) -> Result<Vec<Output>> {
        match cmd {
            HostCmd::Initialize => self.initialize(),
            HostCmd::Prompt { text, input_id } => self.prompt(&HostInput {
                id: InputId::new(input_id.as_str()),
                origin: RunOrigin::Board,
                input: UserInput { text: text.clone() },
            }),
            HostCmd::Answer {
                request_id,
                option_id,
                updated_input,
                message,
            } => self.answer(&Answer {
                request: RequestId::new(request_id.as_str()),
                decision: ApprovalDecision {
                    option_id: option_id.clone(),
                    updated_input: updated_input.clone(),
                    text: message.clone(),
                },
                by: ResolvedBy::Board,
            }),
            HostCmd::Interrupt => self.interrupt(),
            HostCmd::SetMode { mode } => self.set_mode(mode),
            HostCmd::SetModel { model } => self.set_model(model.as_deref().unwrap_or("default")),
        }
    }
}
