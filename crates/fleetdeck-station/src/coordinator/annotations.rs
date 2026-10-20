//! The MCP tool annotations and the output schemas of the catalog
//! (`docs/v2/design/mcp-p1.md` §4.1, §4.2). The keys are the MCP wire keys.

use serde_json::{Value, json};

/// The annotations of tool `name` (§4.1), or `None` for an unknown name. A
/// read-only tool has `readOnlyHint` and `openWorldHint`; every other tool
/// has all four hints, because the spec default of `destructiveHint` is true.
pub(super) fn annotations(name: &str) -> Option<Value> {
    let read_only = |open_world: bool| json!({"readOnlyHint": true, "openWorldHint": open_world});
    let write = |destructive: bool, idempotent: bool, open_world: bool| {
        json!({
            "readOnlyHint": false,
            "destructiveHint": destructive,
            "idempotentHint": idempotent,
            "openWorldHint": open_world,
        })
    };
    let hints = match name {
        "list_sessions"
        | "wait_task"
        | "wait_tasks"
        | "command_status"
        | "read_session"
        | "session_diff"
        | "list_needs_you"
        | "list_projects"
        | "list_artifacts"
        | "read_artifact"
        | "read_file"
        | "search_files"
        | "project_branches"
        | "desktop_info"
        | "desktop_action_status"
        | "capture_status"
        | "read_capture_image"
        | "window_list"
        | "fleet_view" => read_only(false),
        "project_items" => read_only(true),
        "set_focus" | "resume_session" | "rename_session" | "set_mode" | "move_to_project"
        | "wall_show" | "pointer_move" => write(false, true, false),
        "publish_artifact" | "send_mail" | "assign" | "assign_auto" | "complete_task"
        | "ask_coordinator" | "spawn" | "start_in_project" | "notify" | "screenshot"
        | "record_monitor" => write(false, false, false),
        "project_fetch" => write(false, true, true),
        "preview_open" => write(false, false, true),
        "interrupt" | "stop" | "window_close" => write(true, true, false),
        "answer_request" | "move_session" => write(true, false, false),
        "land_session" => write(true, false, true),
        "key_press" | "type_text" | "pointer_click" => write(true, false, true),
        _ => return None,
    };
    Some(hints)
}

/// The output schema of tool `name` (§4.2): only the two station-owned
/// results, `publish_artifact` and `set_focus`.
pub(super) fn output_schema(name: &str) -> Option<Value> {
    match name {
        "publish_artifact" => Some(json!({
            "type": "object",
            "required": ["artifact_id", "version", "title", "created"],
            "properties": {
                "artifact_id": {"type": "string"},
                "version": {"type": "integer", "minimum": 1},
                "title": {"type": "string"},
                "created": {"type": "boolean"},
            },
        })),
        "set_focus" => Some(json!({
            "type": "object",
            "required": ["focus"],
            "properties": {"focus": {"type": "string", "maxLength": 120}},
        })),
        _ => None,
    }
}
