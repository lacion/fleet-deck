//! The plain types of the Codex core: ordinal ids, tool nodes, input entries
//! and the purpose of a host request.

use fleetdeck_proto::event::{InputState, ModelChoice, RunOrigin};
use fleetdeck_proto::{InputId, NodeId, PlanId, RunId, TurnId};

/// Ordinal ids such as `turn-3`. Turns and nodes share one counter, the same
/// as the spike, so recorded host commands name the same ids.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Ordinals {
    shared: u64,
    runs: u64,
    plans: u64,
}

impl Ordinals {
    pub(super) fn next(&mut self, prefix: &str) -> String {
        self.shared += 1;
        format!("{prefix}-{}", self.shared)
    }

    pub(super) fn run(&mut self) -> RunId {
        self.runs += 1;
        RunId(format!("run-{}", self.runs))
    }

    pub(super) fn plan(&mut self) -> PlanId {
        self.plans += 1;
        PlanId(format!("plan-{}", self.plans))
    }
}

/// A tool item of the session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct ToolNode {
    pub(super) node_id: NodeId,
    /// Creation order, so tools close in the order they started.
    pub(super) order: u64,
    /// Native item type (`commandExecution`, `fileChange`, ...).
    pub(super) item_type: String,
    pub(super) done: bool,
    pub(super) denied: bool,
    /// The turn completed while this command ran: it is a background terminal
    /// and stays open until its own `item/completed`.
    pub(super) background: bool,
}

/// A host input and its fate as the app-server reported it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct InputEntry {
    pub(super) id: InputId,
    pub(super) origin: RunOrigin,
    pub(super) summary: String,
    /// The bounded full text, until `run.started` carries it (B10).
    #[serde(default)]
    pub(super) input_text: Option<String>,
    /// Who typed the input (R11.5, DL-166); a checkpoint of an older build has none.
    #[serde(default)]
    pub(super) author: Option<String>,
    /// `None` until the app-server accepted the request.
    pub(super) state: Option<InputState>,
    pub(super) turn: Option<TurnId>,
}

impl InputEntry {
    pub(super) fn is_open(&self) -> bool {
        matches!(
            self.state,
            None | Some(InputState::Received | InputState::Started)
        )
    }
}

/// An effort choice that waits for the next turn, or that a `turn/start`
/// carries (B18 Phase 2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct EffortPick {
    pub(super) effort: String,
    /// The station's effect id, echoed in `effort.set_result`.
    pub(super) control_id: Option<String>,
}

/// What a host request was for, so its reply can be mapped.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum HostRequest {
    Initialize,
    ThreadStart,
    ThreadResume,
    ThreadFork,
    BackgroundList,
    TurnStart {
        input_id: InputId,
        /// The effort pick that this request carries (B18 Phase 2); a
        /// checkpoint of an older build has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<EffortPick>,
    },
    TurnSteer {
        input_id: InputId,
    },
    /// With the host's control id, echoed in `interrupt.result`.
    Interrupt(Option<String>),
    /// `thread/rollback` (DL-087).
    Rollback,
    /// `model/list` after the thread opened (DL-151): the follow-up pages
    /// requested so far, and the models of the pages received so far.
    ModelList {
        pages: u8,
        models: Vec<ModelChoice>,
    },
    /// `thread/shellCommand` of the shell effect `effect_id` (B8).
    ShellCommand {
        effect_id: String,
    },
}
