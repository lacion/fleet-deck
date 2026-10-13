//! The fixed coordinator tool catalog (ADR-006 §5.3, §6.3).
//!
//! The catalog depends only on the granted classes. It is the same for a
//! pending, an active and a renewed binding: Codex caches it at startup.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::bindings::ToolClass;

/// One tool: its name, class and input schema.
pub struct Tool {
    pub name: &'static str,
    pub class: ToolClass,
    pub description: &'static str,
    schema: fn() -> Value,
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
        "station": {"type": "string"},
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

fn publish_artifact() -> Value {
    json!({"type": "object", "required": ["path"], "properties": {
        "path": {"type": "string", "description": "A file in your workspace (relative or absolute)."},
        "title": {"type": "string", "description": "A short title for the artifact."}}})
}

fn set_focus() -> Value {
    json!({"type": "object", "required": ["text"], "properties": {
        "text": {"type": "string", "maxLength": 120,
                 "description": "One line about what you do now. Empty text clears it."}}})
}

/// Every tool. The order is the order of `tools/list`.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "list_sessions",
        class: ToolClass::Read,
        description: "List the sessions that the grant can see.",
        schema: list,
    },
    Tool {
        name: "wait_task",
        class: ToolClass::Read,
        description: "Wait for the end of a task.",
        schema: task,
    },
    Tool {
        name: "wait_tasks",
        class: ToolClass::Read,
        description: "Wait for the first of several tasks to end, block or ask a question.",
        schema: tasks,
    },
    Tool {
        name: "send_mail",
        class: ToolClass::Mail,
        description: "Send mail to a session.",
        schema: send,
    },
    Tool {
        name: "assign",
        class: ToolClass::Mail,
        description: "Assign a task to a session.",
        schema: assign,
    },
    Tool {
        name: "assign_auto",
        class: ToolClass::Mail,
        description: "Assign a task to the best session.",
        schema: assign_auto,
    },
    Tool {
        name: "complete_task",
        class: ToolClass::Mail,
        description: "Report the end of a task assigned to you: done, failed, blocked (with a \
                      reason) or question (with a reason and a question).",
        schema: complete,
    },
    Tool {
        name: "ask_coordinator",
        class: ToolClass::Mail,
        description: "Ask the coordinator that gave you a task a question.",
        schema: ask,
    },
    Tool {
        name: "interrupt",
        class: ToolClass::Drive,
        description: "Interrupt a session that you started.",
        schema: target,
    },
    Tool {
        name: "stop",
        class: ToolClass::Drive,
        description: "Stop a session that you started.",
        schema: target,
    },
    Tool {
        name: "spawn",
        class: ToolClass::Spawn,
        description: "Start a session.",
        schema: spawn,
    },
    Tool {
        name: "publish_artifact",
        class: ToolClass::Own,
        description: "Keep a file of your workspace (HTML, Markdown, an image, SVG, PDF) as a versioned artifact that the owner opens in Fleet Deck. An HTML file is kept with the local files that it names.",
        schema: publish_artifact,
    },
    Tool {
        name: "set_focus",
        class: ToolClass::Own,
        description: "Set the one line that Fleet Deck shows on your card (at most 120 characters).",
        schema: set_focus,
    },
];

impl Tool {
    /// Whether the granted classes give this tool. `ask_coordinator` is a
    /// `Mail` tool, and every session's own binding (`Own`) has it too
    /// (DL-165): there the call goes to the deck without a grant (R2.3).
    pub fn granted(&self, classes: &BTreeSet<ToolClass>) -> bool {
        classes.contains(&self.class)
            || (self.name == "ask_coordinator" && classes.contains(&ToolClass::Own))
    }

    /// Whether the station's own session tools answer this call: an `Own`
    /// tool. `ask_coordinator` is always forwarded to the deck (R2.3).
    pub fn is_own(&self) -> bool {
        self.class == ToolClass::Own
    }
}

/// The tool by name.
pub fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The `tools/list` result for the granted classes.
pub fn catalog(classes: &BTreeSet<ToolClass>) -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .filter(|t| t.granted(classes))
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
        .collect();
    json!({ "tools": tools })
}

/// The `coordinator.call` operation of a mutating tool call. Read tools
/// (`list_sessions`, `wait_task`, `wait_tasks`) are queries and give `Ok(None)`. Arguments
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
        "list_sessions" | "wait_task" | "wait_tasks" => return Ok(None),
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
}
