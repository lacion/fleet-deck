//! Builders for the frames that the host writes to the CLI's stdin in
//! `--input-format stream-json` mode (host → CLI).

use serde_json::{Value, json};

/// A user message (a prompt, a follow-up, or a steer during a turn). The client
/// `uuid` makes the CLI report the input's fate in `command_lifecycle` frames.
#[must_use]
pub fn user_message(text: &str, session_id: &str, uuid: &str) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
        "session_id": session_id,
        "uuid": uuid,
    })
}

/// A host control request (`initialize`, `interrupt`, `set_permission_mode`, `set_model`, ...).
#[must_use]
pub fn control_request(request_id: &str, subtype: &str, mut fields: Value) -> Value {
    let mut request = json!({"subtype": subtype});
    if let (Some(dst), Some(src)) = (request.as_object_mut(), fields.as_object_mut()) {
        dst.append(src);
    }
    json!({"type": "control_request", "request_id": request_id, "request": request})
}

/// `initialize` without hooks or SDK MCP servers.
#[must_use]
pub fn initialize(request_id: &str) -> Value {
    control_request(request_id, "initialize", json!({}))
}

/// `interrupt` the running turn.
#[must_use]
pub fn interrupt(request_id: &str) -> Value {
    control_request(request_id, "interrupt", json!({}))
}

/// `set_permission_mode` to a native mode name.
#[must_use]
pub fn set_permission_mode(request_id: &str, mode: &str) -> Value {
    control_request(request_id, "set_permission_mode", json!({"mode": mode}))
}

/// `set_model`. `None` returns to the default model (trap 2: "default" is a sentinel).
#[must_use]
pub fn set_model(request_id: &str, model: Option<&str>) -> Value {
    control_request(request_id, "set_model", json!({"model": model}))
}

/// The host's decision for a `can_use_tool` request.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    /// Allow the tool call. `updated_input` replaces the tool input (edit before allow).
    Allow { updated_input: Value },
    /// Deny the tool call. `interrupt` also stops the turn.
    Deny { message: String, interrupt: bool },
}

/// Success response to a CLI control request.
#[must_use]
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
}

/// Error response to a CLI control request (used to fail closed on unknown requests).
#[must_use]
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": error},
    })
}

/// Response to a `can_use_tool` request.
#[must_use]
pub fn permission_response(request_id: &str, decision: &PermissionDecision) -> Value {
    let body = match decision {
        PermissionDecision::Allow { updated_input } => {
            json!({"behavior": "allow", "updatedInput": updated_input})
        }
        PermissionDecision::Deny { message, interrupt } => {
            json!({"behavior": "deny", "message": message, "interrupt": interrupt})
        }
    };
    control_success(request_id, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_merges_fields() {
        let v = set_permission_mode("r2", "plan");
        assert_eq!(
            v,
            json!({"type":"control_request","request_id":"r2","request":{"subtype":"set_permission_mode","mode":"plan"}})
        );
    }

    #[test]
    fn permission_response_shapes() {
        let allow = permission_response(
            "q",
            &PermissionDecision::Allow {
                updated_input: json!({"command":"echo edited"}),
            },
        );
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(
            allow["response"]["response"]["updatedInput"]["command"],
            "echo edited"
        );
        let deny = permission_response(
            "q",
            &PermissionDecision::Deny {
                message: "no".into(),
                interrupt: false,
            },
        );
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(
            control_error("z", "unknown")["response"]["subtype"],
            "error"
        );
    }
}
