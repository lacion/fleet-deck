//! Serializable host commands. The recorder logs each command as a `host`
//! fixture record; the replay harness applies the same commands to a fresh
//! core at the same positions (PLAN §13, L20).

use fleetdeck_proto::command::{ApprovalDecision, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{InputId, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::SessionSpec;
use super::core::{CodexCore, Output};
use crate::adapter::{Answer, HostInput, Result};

/// One command of the host (Fleet Deck) to the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostCmd {
    Initialize,
    OpenThread {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        fork: bool,
    },
    Prompt {
        text: String,
        input_id: String,
    },
    Steer {
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
        model: String,
    },
    SetCollaboration {
        mode: String,
    },
    ListBackground,
}

impl HostCmd {
    /// The `open_thread` command of a session spec.
    #[must_use]
    pub fn open_thread(spec: &SessionSpec) -> Self {
        Self::OpenThread {
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            mode: spec.mode.clone(),
            cwd: spec.cwd.clone(),
            resume: spec.resume.clone(),
            fork: spec.fork,
        }
    }
}

fn board_input(text: &str, input_id: &str) -> HostInput {
    HostInput {
        id: InputId::new(input_id),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
        context: Vec::new(),
    }
}

impl CodexCore {
    /// Applies a recorded host command. Recorded inputs and answers came from
    /// the board.
    ///
    /// # Errors
    /// The error of the command (see the per-command methods).
    pub fn apply(&mut self, cmd: &HostCmd) -> Result<Vec<Output>> {
        match cmd {
            HostCmd::Initialize => self.initialize(),
            HostCmd::OpenThread {
                model,
                effort,
                mode,
                cwd,
                resume,
                fork,
            } => self.open_thread(&SessionSpec {
                model: model.clone(),
                effort: effort.clone(),
                mode: mode.clone(),
                cwd: cwd.clone(),
                resume: resume.clone(),
                fork: *fork,
            }),
            HostCmd::Prompt { text, input_id } => self.prompt(&board_input(text, input_id)),
            HostCmd::Steer { text, input_id } => self.steer(&board_input(text, input_id)),
            HostCmd::Answer {
                request_id,
                option_id,
                updated_input,
                message,
            } => self.answer(&Answer {
                request: RequestId::new(request_id.as_str()),
                decision: ApprovalDecision {
                    answered_by: None,
                    option_id: option_id.clone(),
                    updated_input: updated_input.clone(),
                    text: message.clone(),
                },
                by: ResolvedBy::Board,
            }),
            HostCmd::Interrupt => self.interrupt(None),
            HostCmd::SetMode { mode } => self.set_mode(mode),
            HostCmd::SetModel { model } => self.set_model(model),
            HostCmd::SetCollaboration { mode } => self.set_collaboration(mode),
            HostCmd::ListBackground => self.list_background(),
        }
    }
}
