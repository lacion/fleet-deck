//! The Fleet Git coordinator tools (FG-10, design §13.2, O32): thin forwards
//! of `CoordinatorQuery::{ProjectBranches, ProjectFetch}` and
//! `CoordinatorOp::Land`, and the forward of `CoordinatorOp::Move` (FG-9). The
//! deck decides everything. `fleet_tools.rs` lists these tools.

use serde_json::{Value, json};

use super::bindings::ToolClass;
use super::tools::Tool;

fn project() -> Value {
    json!({"type": "object", "required": ["project_id"],
           "properties": {"project_id": {"type": "string"}}})
}

fn target() -> Value {
    json!({"type": "object", "required": ["station", "session_id"],
           "properties": {"station": {"type": "string"}, "session_id": {"type": "string"}}})
}

/// `project_branches`: a `coordinator.query`.
pub(super) const PROJECT_BRANCHES: Tool = Tool {
    name: "project_branches",
    classes: &[ToolClass::Inspect],
    description: "List the fd branches that the stations pushed for a project, with their sessions and conflict state.",
    schema: project,
};

/// `project_fetch`: a `coordinator.query`.
pub(super) const PROJECT_FETCH: Tool = Tool {
    name: "project_fetch",
    classes: &[ToolClass::Inspect],
    description: "Fetch the remote of a project on its radar home; it changes only remote-tracking refs.",
    schema: project,
};

/// `land_session`: a `coordinator.call` op.
pub(super) const LAND_SESSION: Tool = Tool {
    name: "land_session",
    classes: &[ToolClass::Manage],
    description: "Land a session of a synced project into its default branch; the deck queues it and checks the grant again at each step.",
    schema: target,
};

fn move_target() -> Value {
    json!({"type": "object", "required": ["station", "session_id", "to_station"],
           "properties": {"station": {"type": "string"}, "session_id": {"type": "string"},
                          "to_station": {"type": "string"}}})
}

/// `move_session`: a `coordinator.call` op (FG-9, design §13.2).
pub(super) const MOVE_SESSION: Tool = Tool {
    name: "move_session",
    classes: &[ToolClass::Manage],
    description: "Move a session of a synced project to another station of the grant; the deck drives the move and checks the grant again at each step.",
    schema: move_target,
};
