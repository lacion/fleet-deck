//! The fixed coordinator tool catalog (ADR-006 §5.3, §6.3).
//!
//! The catalog depends only on the granted classes. It is the same for a
//! pending, an active and a renewed binding: Codex caches it at startup.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::annotations::{annotations, output_schema};
use super::bindings::ToolClass;

/// One tool: its name, classes and input schema. A grant lists the tool
/// when it has every class of the tool (ADR-008 §4.1).
pub struct Tool {
    pub name: &'static str,
    pub classes: &'static [ToolClass],
    pub description: &'static str,
    pub(super) schema: fn() -> Value,
}

fn target() -> Value {
    json!({"type": "object", "required": ["station", "session_id"],
           "properties": {"station": {"type": "string"}, "session_id": {"type": "string"}}})
}

fn send() -> Value {
    json!({"type": "object", "required": ["to", "text"], "properties": {
        "to": {"description": "A session, or a group address: @idle, @claude or @codex.",
               "oneOf": [target(), {"type": "string", "enum": ["@idle", "@claude", "@codex"]}]},
        "text": {"type": "string"},
        "delivery": {"type": "string", "enum": ["follow_up", "steer"]}}})
}

fn assign() -> Value {
    json!({"type": "object", "required": ["task", "to"],
           "properties": {"task": {"type": "string"}, "to": target()}})
}

fn assign_auto() -> Value {
    json!({"type": "object", "required": ["task"], "properties": {
        "task": {"type": "string"},
        "requirements": {"type": "object", "properties": {
            "repo": {"type": "string"}, "provider": {"type": "string"}}}}})
}

fn complete() -> Value {
    json!({"type": "object", "required": ["task_id"], "properties": {
        "task_id": {"type": "string"}, "result": {"type": "string"},
        "state": {"type": "string", "enum": ["done", "failed", "blocked", "question"]},
        "reason": {"type": "string"}, "question": {"type": "string"}}})
}

fn ask() -> Value {
    json!({"type": "object", "required": ["question"],
           "properties": {"question": {"type": "string"}}})
}

fn tasks() -> Value {
    json!({"type": "object", "required": ["task_ids"], "properties": {
        "task_ids": {"type": "array", "items": {"type": "string"},
                     "minItems": 1, "maxItems": 50}}})
}

fn spawn() -> Value {
    json!({"type": "object", "required": ["station", "spec"], "properties": {
        "station": {"type": "string", "description": "Station id or name."},
        "spec": {"type": "object", "required": ["provider_instance", "cwd"], "properties": {
            "provider_instance": {"type": "string"}, "cwd": {"type": "string"},
            "mode": {"type": "string"}, "model": {"type": "string"}, "title": {"type": "string"}}},
        "prompt": {"type": "string"}}})
}

fn list() -> Value {
    json!({"type": "object", "properties": {}})
}

fn task() -> Value {
    json!({"type": "object", "required": ["task_id"],
           "properties": {"task_id": {"type": "string"}, "timeout_ms": {"type": "integer"}}})
}

fn command() -> Value {
    json!({"type": "object", "required": ["command_id"],
           "properties": {"command_id": {"type": "string"}}})
}

fn publish_artifact() -> Value {
    json!({"type": "object", "properties": {
        "path": {"type": "string", "description": "A file in your workspace"},
        "name": {"type": "string", "description": "The file name of inline content, for example report.html"},
        "content": {"type": "string", "description": "The full text of the file, when you cannot or do not want to write a file"},
        "title": {"type": "string", "description": "A short title for the artifact."},
        "artifact_id": {"type": "string", "description": "The artifact_id that an earlier publish_artifact call returned. The file becomes its next version. Leave it out for a new artifact."}}})
}

fn set_focus() -> Value {
    json!({"type": "object", "required": ["text"], "properties": {
        "text": {"type": "string", "maxLength": 120,
                 "description": "One line about what you do now. Empty text clears it."}}})
}

fn fleet_view() -> Value {
    json!({"type": "object", "required": ["view"], "additionalProperties": false, "properties": {
        "view": {"type": "string", "enum": ["stations", "projects", "sessions", "me"]},
        "cursor": {"type": "string", "maxLength": 4096},
        "station": {"type": "string", "maxLength": 128, "description": "Station id or name."}}})
}

/// Every tool. The order is the order of `tools/list`.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "list_sessions",
        classes: &[ToolClass::Read],
        description: "List the sessions that the grant can see.",
        schema: list,
    },
    Tool {
        name: "wait_task",
        classes: &[ToolClass::Read],
        description: "Wait (at most about a minute) until a task is no longer open: done, failed, \
                      blocked, or question (the worker asks you something; answer with send_mail).",
        schema: task,
    },
    Tool {
        name: "wait_tasks",
        classes: &[ToolClass::Read],
        description: "Wait for the first of several tasks to end, block or ask a question.",
        schema: tasks,
    },
    Tool {
        name: "command_status",
        classes: &[ToolClass::Read],
        description: "Read the state of a call that answered `pending`: its receipt and deliveries, by its command_id.",
        schema: command,
    },
    Tool {
        name: "send_mail",
        classes: &[ToolClass::Mail],
        description: "Send mail to a session.",
        schema: send,
    },
    Tool {
        name: "assign",
        classes: &[ToolClass::Mail],
        description: "Assign a task to a session.",
        schema: assign,
    },
    Tool {
        name: "assign_auto",
        classes: &[ToolClass::Mail],
        description: "Assign a task to the best session.",
        schema: assign_auto,
    },
    Tool {
        name: "complete_task",
        classes: &[ToolClass::Mail],
        description: "Report the end of a task assigned to you: done, failed, blocked (with a \
                      reason) or question (with a reason and a question).",
        schema: complete,
    },
    Tool {
        name: "ask_coordinator",
        classes: &[ToolClass::Mail],
        description: "Ask the coordinator that gave you a task a question.",
        schema: ask,
    },
    Tool {
        name: "interrupt",
        classes: &[ToolClass::Drive],
        description: "Interrupt a session that you started.",
        schema: target,
    },
    Tool {
        name: "stop",
        classes: &[ToolClass::Drive],
        description: "Stop a session that you started.",
        schema: target,
    },
    Tool {
        name: "spawn",
        classes: &[ToolClass::Spawn],
        description: "Start a session.",
        schema: spawn,
    },
    Tool {
        name: "publish_artifact",
        classes: &[ToolClass::Own],
        description: "Keep a file of your workspace (HTML, Markdown, an image, SVG, PDF), or inline text that you pass as `content` with a `name`, as a versioned artifact that the owner opens in Fleet Deck. An HTML file is kept with the local files that it names. Give exactly one of `path` or `content`.",
        schema: publish_artifact,
    },
    Tool {
        name: "set_focus",
        classes: &[ToolClass::Own],
        description: "Set the one line that Fleet Deck shows on your card (at most 120 characters).",
        schema: set_focus,
    },
    Tool {
        name: "fleet_view",
        classes: &[ToolClass::Own],
        description: "Read the fleet: stations and platforms, projects (one station with station), live sessions (titles, states), or yourself and your grant. Pass next_cursor back as cursor.",
        schema: fleet_view,
    },
];

impl Tool {
    /// Whether the granted classes give this tool. `ask_coordinator` is a
    /// `Mail` tool, and every session's own binding (`Own`) has it too
    /// (DL-165): there the call goes to the deck without a grant (R2.3).
    /// `command_status` is granted by an outbox class (`Mail`, `Drive`,
    /// `Spawn`, `Manage`, `Answer`), and `read` alone does not give it.
    pub fn granted(&self, classes: &BTreeSet<ToolClass>) -> bool {
        if self.name == "command_status" {
            return classes.iter().any(|c| {
                matches!(
                    c,
                    ToolClass::Mail
                        | ToolClass::Drive
                        | ToolClass::Spawn
                        | ToolClass::Manage
                        | ToolClass::Answer
                )
            });
        }
        self.classes.iter().all(|c| classes.contains(c))
            || (self.name == "ask_coordinator" && classes.contains(&ToolClass::Own))
            || (self.name == "desktop_info"
                && (classes.contains(&ToolClass::Desktop) || classes.contains(&ToolClass::Capture)))
    }

    /// Whether the station's own session tools answer this call: an `Own`
    /// tool. `ask_coordinator` is always forwarded to the deck (R2.3).
    pub fn is_own(&self) -> bool {
        self.classes == [ToolClass::Own]
    }

    /// True for a tool that only a fleet-control grant has: one of its
    /// classes is `inspect`, `manage` or `answer` (ADR-008 §2).
    pub fn is_fleet_control(&self) -> bool {
        self.classes.iter().any(|c| c.is_fleet_control())
    }
}

/// The tool by name.
pub fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS
        .iter()
        .chain(super::fleet_tools::FLEET_TOOLS)
        .chain(super::desktop_tools::DESKTOP_TOOLS)
        .find(|t| t.name == name)
}

/// The `tools/list` result for the granted classes.
pub fn catalog(classes: &BTreeSet<ToolClass>) -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .chain(super::fleet_tools::FLEET_TOOLS)
        .chain(super::desktop_tools::DESKTOP_TOOLS)
        .filter(|t| t.granted(classes))
        .map(|t| {
            let mut entry = json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": annotations(t.name),
            });
            if let (Some(object), Some(output)) = (entry.as_object_mut(), output_schema(t.name)) {
                object.insert("outputSchema".to_owned(), output);
            }
            entry
        })
        .collect();
    json!({ "tools": tools })
}

/// The `coordinator.call` operation of a mutating tool call. Read tools
/// (`list_sessions`, `wait_task`, `wait_tasks`, `command_status` and the fleet-control read
/// tools) are queries and give `Ok(None)`. Arguments
/// that are not in the tool's schema are refused: no argument can name the
/// caller (ADR-006 §5.3).
pub fn to_op(
    tool: &str,
    args: &Value,
) -> Result<Option<fleetdeck_proto::mutation::CoordinatorOp>, String> {
    let Some(obj) = args.as_object() else {
        return Err("invalid_params: the arguments are not an object".to_owned());
    };
    let mut obj = obj.clone();
    let op = match tool {
        "list_sessions" | "wait_task" | "wait_tasks" | "command_status" => return Ok(None),
        t if super::fleet_tools::is_query(t) => return Ok(None),
        // The P6 tools never become a `CoordinatorOp` (they use their own
        // forwarder).
        t if super::desktop_tools::is_desktop(t) => {
            return Err("forbidden: the grant has no such tool".to_owned());
        }
        "resume_session" | "rename_session" | "set_mode" | "move_to_project"
        | "start_in_project" | "answer_request" | "land_session" | "move_session" => {
            return super::fleet_tools::op(tool, args).map(Some);
        }
        "send_mail" => {
            obj.entry("delivery").or_insert(json!("follow_up"));
            if obj.get("to").is_some_and(Value::is_string) {
                if let Some(group) = obj.remove("to") {
                    obj.insert("group".to_owned(), group);
                }
                "send_group"
            } else {
                "send"
            }
        }
        "assign_auto" => {
            obj.entry("requirements").or_insert(json!({}));
            "assign_auto"
        }
        "complete_task" => {
            obj.entry("result").or_insert(Value::Null);
            "complete_task"
        }
        "spawn" => {
            let prompt = match obj.remove("prompt") {
                Some(Value::String(text)) => json!({ "text": text }),
                _ => Value::Null,
            };
            obj.insert("prompt".to_owned(), prompt);
            if let Some(spec) = obj.get_mut("spec").and_then(Value::as_object_mut) {
                for key in ["mode", "model", "title"] {
                    spec.entry(key).or_insert(Value::Null);
                }
            }
            "spawn"
        }
        "assign" | "interrupt" | "stop" | "ask_coordinator" => tool,
        _ => return Err("forbidden: the grant has no such tool".to_owned()),
    };
    obj.insert("op".to_owned(), json!(op));
    serde_json::from_value(Value::Object(obj))
        .map(Some)
        .map_err(|e| format!("invalid_params: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorOp};
    use fleetdeck_proto::orchestration::{MailGroup, TaskState};

    use super::*;

    #[test]
    fn tool_arguments_map_to_coordinator_ops_and_unknown_arguments_are_refused() {
        let to = json!({"station": "alpha", "session_id": "worker-1"});
        let op = to_op("send_mail", &json!({"to": to, "text": "hi"})).unwrap();
        assert!(matches!(
            op,
            Some(CoordinatorOp::Send {
                delivery: CoordinatorDelivery::FollowUp,
                ..
            })
        ));
        let spawn = json!({"station": "alpha", "spec": {"provider_instance": "claude", "cwd": "/w"},
                           "prompt": "go"});
        let Some(CoordinatorOp::Spawn { prompt, spec, .. }) = to_op("spawn", &spawn).unwrap()
        else {
            panic!("spawn");
        };
        assert_eq!(prompt.unwrap().text, "go");
        assert_eq!(spec.mode, None);
        assert!(
            to_op("assign_auto", &json!({"task": "t"}))
                .unwrap()
                .is_some()
        );
        assert!(
            to_op("complete_task", &json!({"task_id": "t"}))
                .unwrap()
                .is_some()
        );
        assert!(to_op("list_sessions", &json!({})).unwrap().is_none());
        assert!(
            to_op("wait_tasks", &json!({"task_ids": ["a"]}))
                .unwrap()
                .is_none()
        );
        // A group address in `to` is a group send; a target stays a send.
        let Some(CoordinatorOp::SendGroup { group, .. }) =
            to_op("send_mail", &json!({"to": "@idle", "text": "hi"})).unwrap()
        else {
            panic!("group");
        };
        assert_eq!(group, MailGroup::Idle);
        assert!(
            to_op("send_mail", &json!({"to": "@everyone", "text": "hi"}))
                .unwrap_err()
                .starts_with("invalid_params")
        );
        // `complete_task` carries its kind; `ask_coordinator` its question.
        let Some(CoordinatorOp::CompleteTask {
            state,
            reason,
            question,
            ..
        }) = to_op(
            "complete_task",
            &json!({"task_id": "t", "state": "question", "reason": "r", "question": "q?"}),
        )
        .unwrap()
        else {
            panic!("complete");
        };
        assert_eq!(
            (state, reason.as_deref(), question.as_deref()),
            (Some(TaskState::Question), Some("r"), Some("q?"))
        );
        assert!(matches!(
            to_op("ask_coordinator", &json!({"question": "q?"})).unwrap(),
            Some(CoordinatorOp::AskCoordinator { .. })
        ));
        assert!(to_op("ask_coordinator", &json!({})).is_err());
        // An argument that names another caller is refused, not ignored.
        let forged = json!({"to": to, "text": "hi", "caller": "victim-1"});
        assert!(
            to_op("send_mail", &forged)
                .unwrap_err()
                .starts_with("invalid_params")
        );
        assert!(
            to_op("drop_tables", &json!({}))
                .unwrap_err()
                .starts_with("forbidden")
        );
        assert!(to_op("stop", &json!("x")).is_err());
    }

    #[test]
    fn command_status_is_listed_for_an_outbox_class_only() {
        let names = |classes: &[ToolClass]| -> Vec<String> {
            let set: BTreeSet<ToolClass> = classes.iter().copied().collect();
            catalog(&set)["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        };
        let listed = |classes: &[ToolClass]| names(classes).iter().any(|n| n == "command_status");
        for none in [
            &[ToolClass::Read][..],
            &[ToolClass::Inspect],
            &[ToolClass::Read, ToolClass::Inspect],
            &[ToolClass::Own],
            &[ToolClass::Desktop],
            &[ToolClass::Capture],
            &[ToolClass::Input, ToolClass::InputEnabled],
        ] {
            assert!(!listed(none), "{none:?}");
        }
        for class in [
            ToolClass::Mail,
            ToolClass::Drive,
            ToolClass::Spawn,
            ToolClass::Manage,
            ToolClass::Answer,
        ] {
            assert!(listed(&[class]), "{class:?}");
        }
    }

    #[test]
    fn command_status_is_a_read_query_and_needs_a_command_id() {
        assert!(
            to_op("command_status", &json!({"command_id": "c"}))
                .unwrap()
                .is_none()
        );
        assert!(to_op("command_status", &json!({})).unwrap().is_none());
    }
}
