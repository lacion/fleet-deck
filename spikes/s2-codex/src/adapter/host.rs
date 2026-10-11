//! Serializable host commands (logged as `host` fixture records and re-applied
//! by the replay harness).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AdapterError, Answer, CodexCore, Output, SessionSpec};

/// One command of the host (Fleet Deck) to the adapter.
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
    SetCollaboration {
        mode: String,
    },
    ListBackground,
}

impl HostCmd {
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

impl CodexCore {
    /// Applies a host command.
    pub fn apply(&mut self, cmd: &HostCmd) -> Result<Vec<Output>, AdapterError> {
        match cmd {
            HostCmd::Initialize => Ok(self.initialize()),
            HostCmd::OpenThread {
                model,
                effort,
                mode,
                cwd,
                resume,
                fork,
            } => Ok(self.open_thread(&SessionSpec {
                model: model.clone(),
                effort: effort.clone(),
                mode: mode.clone(),
                cwd: cwd.clone(),
                resume: resume.clone(),
                fork: *fork,
            })),
            HostCmd::Prompt { text, input_id } => self.prompt(text, input_id),
            HostCmd::Steer { text, input_id } => self.steer(text, input_id),
            HostCmd::Answer {
                request_id,
                option_id,
                updated_input,
                message,
            } => self
                .answer(
                    request_id,
                    &Answer {
                        option_id: option_id.clone(),
                        updated_input: updated_input.clone(),
                        message: message.clone(),
                    },
                )
                .map(|(_, out)| out),
            HostCmd::Interrupt => self.interrupt(),
            HostCmd::SetMode { mode } => self.set_mode(mode),
            HostCmd::SetCollaboration { mode } => self.set_collaboration(mode),
            HostCmd::ListBackground => self.list_background(),
        }
    }
}
