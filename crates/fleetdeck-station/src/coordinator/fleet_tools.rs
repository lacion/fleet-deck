//! The fleet-control tools (ADR-008 §4.1): the tools that a grant with the
//! classes `inspect`, `manage` or `answer` gets. A tool is listed when the
//! grant has every class of the tool; the catalog does not depend on the
//! switches. Each tool maps to a `coordinator.query` (a read) or to a
//! `coordinator.call` op (a change). The station checks only the JSON shape
//! and the limit of `max_items`; the deck decides everything else.

use fleetdeck_proto::mutation::{CoordinatorOp, CoordinatorQuery};
use serde_json::{Map, Value, json};

use super::bindings::ToolClass;
use super::tools::Tool;

/// The most items that `read_session` returns (ADR-008 §4.1).
pub const MAX_TAIL_ITEMS: u64 = 200;
/// The items that `read_session` returns when `max_items` is absent.
pub const DEFAULT_TAIL_ITEMS: u64 = 50;

fn session() -> Value {
    json!({"type": "object", "required": ["session_id"],
           "properties": {"session_id": {"type": "string"}}})
}

fn tail() -> Value {
    json!({"type": "object", "required": ["session_id"], "properties": {
        "session_id": {"type": "string"},
        "max_items": {"type": "integer", "minimum": 1, "maximum": MAX_TAIL_ITEMS,
                      "default": DEFAULT_TAIL_ITEMS}}})
}

fn none() -> Value {
    json!({"type": "object", "properties": {}})
}

fn project_items() -> Value {
    json!({"type": "object", "required": ["project_id", "kind"], "properties": {
        "project_id": {"type": "string"},
        "kind": {"type": "string", "enum": ["issues", "prs"]}}})
}

fn artifacts() -> Value {
    json!({"type": "object", "properties": {"session_id": {"type": "string"}}})
}

fn artifact() -> Value {
    json!({"type": "object", "required": ["artifact_id"], "properties": {
        "artifact_id": {"type": "string"}, "version": {"type": "integer", "minimum": 1}}})
}

fn file_read() -> Value {
    json!({"type": "object", "required": ["session_id", "path"], "properties": {
        "session_id": {"type": "string"},
        "path": {"type": "string", "description": "A path below the root of the session."}}})
}

fn file_search() -> Value {
    json!({"type": "object", "required": ["session_id", "pattern", "mode"], "properties": {
        "session_id": {"type": "string"}, "pattern": {"type": "string"},
        "mode": {"type": "string", "enum": ["name", "content"]}}})
}

fn target() -> Value {
    json!({"type": "object", "required": ["station", "session_id"],
           "properties": {"station": {"type": "string"}, "session_id": {"type": "string"}}})
}

fn rename() -> Value {
    json!({"type": "object", "required": ["station", "session_id", "title"], "properties": {
        "station": {"type": "string"}, "session_id": {"type": "string"},
        "title": {"type": "string"}}})
}

fn set_mode() -> Value {
    json!({"type": "object", "required": ["station", "session_id", "mode"], "properties": {
        "station": {"type": "string"}, "session_id": {"type": "string"},
        "mode": {"type": "string"}}})
}

fn move_to_project() -> Value {
    json!({"type": "object", "required": ["station", "session_id", "target"], "properties": {
        "station": {"type": "string"}, "session_id": {"type": "string"},
        "target": {"oneOf": [
            {"type": "object", "required": ["kind", "id"], "properties": {
                "kind": {"const": "project"}, "id": {"type": "string"}}},
            {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "none"}}},
            {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "auto"}}}]}}})
}

fn start_in_project() -> Value {
    json!({"type": "object", "required": ["project_id", "prompt"], "properties": {
        "project_id": {"type": "string"}, "prompt": {"type": "string"},
        "provider_instance": {"type": "string"},
        "worktree": {"type": "boolean", "default": true}}})
}

fn answer_request() -> Value {
    json!({"type": "object", "required": ["station", "session_id", "request_id"], "properties": {
        "station": {"type": "string"}, "session_id": {"type": "string"},
        "request_id": {"type": "string"},
        "option_id": {"type": "string", "description": "An option that allows once or denies."},
        "question_answers": {"type": "object", "description": "The answers of a structured question."},
        "text": {"type": "string", "description": "The answer to a free-text question."}}})
}

/// Every fleet-control tool. The order is the order of `tools/list` after
/// the ADR-006 tools.
pub const FLEET_TOOLS: &[Tool] = &[
    Tool {
        name: "read_session",
        classes: &[ToolClass::Inspect],
        description: "Read the last items of a session: at most 200 items and 64 KiB.",
        schema: tail,
    },
    Tool {
        name: "session_diff",
        classes: &[ToolClass::Inspect],
        description: "Read the diff of a session against its base commit: removed lines are only counted, and the diff has at most 256 KiB.",
        schema: session,
    },
    Tool {
        name: "list_needs_you",
        classes: &[ToolClass::Inspect, ToolClass::Answer],
        description: "List the open approvals and questions of the sessions that you may answer for.",
        schema: none,
    },
    Tool {
        name: "list_projects",
        classes: &[ToolClass::Inspect],
        description: "List the projects on the stations of the grant.",
        schema: none,
    },
    Tool {
        name: "project_items",
        classes: &[ToolClass::Inspect],
        description: "List the issues or the pull requests of a project on a station of the grant.",
        schema: project_items,
    },
    Tool {
        name: "list_artifacts",
        classes: &[ToolClass::Inspect],
        description: "List the artifacts of one session, or of every session on the stations of the grant.",
        schema: artifacts,
    },
    Tool {
        name: "read_artifact",
        classes: &[ToolClass::Inspect],
        description: "Read one text artifact, at its latest version or at a given version: at most 256 KiB.",
        schema: artifact,
    },
    Tool {
        name: "read_file",
        classes: &[ToolClass::Inspect],
        description: "Read one file below the root of a session: at most 256 KiB, and private files stay hidden.",
        schema: file_read,
    },
    Tool {
        name: "search_files",
        classes: &[ToolClass::Inspect],
        description: "Search file names or file contents below the root of a session: at most 50 hits.",
        schema: file_search,
    },
    Tool {
        name: "resume_session",
        classes: &[ToolClass::Manage],
        description: "Resume an ended session: its mode must be within the ceiling of the grant and never a bypass mode.",
        schema: target,
    },
    Tool {
        name: "rename_session",
        classes: &[ToolClass::Manage],
        description: "Change the title of a session.",
        schema: rename,
    },
    Tool {
        name: "set_mode",
        classes: &[ToolClass::Manage],
        description: "Set the mode of a session: the mode must be within the ceiling of the grant and never a bypass mode.",
        schema: set_mode,
    },
    Tool {
        name: "move_to_project",
        classes: &[ToolClass::Manage],
        description: "Move a session to a project of its own station, out of every project, or to the project that the auto rules pick.",
        schema: move_to_project,
    },
    Tool {
        name: "start_in_project",
        classes: &[ToolClass::Spawn, ToolClass::Manage],
        description: "Start a session in a project of the grant: it counts against the spawn cap.",
        schema: start_in_project,
    },
    Tool {
        name: "answer_request",
        classes: &[ToolClass::Answer],
        description: "Answer one approval or question of another session: allow once or deny only.",
        schema: answer_request,
    },
];

/// True for a fleet-control tool that is a `coordinator.query`.
#[must_use]
pub fn is_query(tool: &str) -> bool {
    matches!(
        tool,
        "read_session"
            | "session_diff"
            | "list_needs_you"
            | "list_projects"
            | "project_items"
            | "list_artifacts"
            | "read_artifact"
            | "read_file"
            | "search_files"
    )
}

/// The arguments as an object.
fn object(args: &Value) -> Result<Map<String, Value>, String> {
    args.as_object()
        .cloned()
        .ok_or_else(|| "invalid_params: the arguments are not an object".to_owned())
}

/// The `coordinator.query` of a fleet-control read tool. The query type
/// refuses unknown arguments and wrong types; `max_items` is at most
/// [`MAX_TAIL_ITEMS`] and is [`DEFAULT_TAIL_ITEMS`] when absent.
///
/// # Errors
/// `invalid_params` for any other shape.
pub fn query(tool: &str, args: &Value) -> Result<CoordinatorQuery, String> {
    let mut obj = object(args)?;
    let name = match tool {
        "read_session" => {
            let max = obj
                .entry("max_items")
                .or_insert_with(|| json!(DEFAULT_TAIL_ITEMS));
            if max.as_u64().is_some_and(|n| n > MAX_TAIL_ITEMS) {
                return Err(format!(
                    "invalid_params: `max_items` is at most {MAX_TAIL_ITEMS}"
                ));
            }
            "session_tail"
        }
        "session_diff" => "session_diff",
        "list_needs_you" => "needs_you",
        "list_projects" => "projects",
        "project_items" => "project_items",
        "list_artifacts" => "artifacts",
        "read_artifact" => "artifact",
        "read_file" => "file_read",
        "search_files" => "file_search",
        other => return Err(format!("invalid_params: no read tool `{other}`")),
    };
    obj.insert("query".to_owned(), json!(name));
    serde_json::from_value(Value::Object(obj)).map_err(|e| format!("invalid_params: {e}"))
}

/// The `coordinator.call` op of a fleet-control change tool. Unknown
/// arguments are refused: no argument can name the caller.
///
/// # Errors
/// `invalid_params` for any other shape.
pub fn op(tool: &str, args: &Value) -> Result<CoordinatorOp, String> {
    let mut obj = object(args)?;
    let name = match tool {
        "resume_session" => "resume",
        "rename_session" => "rename",
        "set_mode" => "set_mode",
        "move_to_project" => "project_assign",
        "start_in_project" => {
            obj.entry("worktree").or_insert(json!(true));
            "project_start"
        }
        "answer_request" => "answer",
        other => return Err(format!("forbidden: the grant has no tool `{other}`")),
    };
    obj.insert("op".to_owned(), json!(name));
    serde_json::from_value(Value::Object(obj)).map_err(|e| format!("invalid_params: {e}"))
}
