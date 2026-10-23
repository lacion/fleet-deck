//! Builders for the frames that the host writes to the CLI's stdin in
//! `--input-format stream-json` mode (host → CLI).

use fleetdeck_proto::command::UserInput;

use crate::adapter::ShellContext;
use serde_json::{Value, json};

/// A user message (a prompt, a follow-up, or a steer during a turn). The client
/// `uuid` makes the CLI report the input's fate in `command_lifecycle` frames.
/// Pasted images go before the text as base64 `image` blocks (DL-304); an
/// input without images or shell results keeps the plain string content. The
/// shell results of `context` come first, as the `bash-*` text blocks that
/// Claude Code writes for its own `!` line (B8).
#[must_use]
pub fn user_message(
    input: &UserInput,
    context: &[ShellContext],
    session_id: &str,
    uuid: &str,
) -> Value {
    let content = if input.images.is_empty() && context.is_empty() {
        Value::String(input.text.clone())
    } else {
        let mut blocks: Vec<Value> = context.iter().flat_map(shell_blocks).collect();
        blocks.extend(input.images.iter().map(|i| {
            json!({"type": "image", "source": {
                "type": "base64", "media_type": i.media_type, "data": i.data,
            }})
        }));
        blocks.push(json!({"type": "text", "text": input.text}));
        Value::Array(blocks)
    };
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "session_id": session_id,
        "uuid": uuid,
    })
}

/// The text shown for a command with no stdout.
const NO_OUTPUT: &str = "(Bash completed with no output)";

/// Escapes the three characters that would close or open a tag.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The two text blocks of one shell result (B8): the input, then the output.
#[must_use]
pub fn shell_blocks(result: &ShellContext) -> [Value; 2] {
    let stdout = if result.stdout.is_empty() {
        NO_OUTPUT.to_owned()
    } else {
        escape(&result.stdout)
    };
    let mut stderr = escape(&result.stderr);
    if result.end != "exit 0" {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&format!("[{}]", result.end));
    }
    [
        json!({"type": "text", "text": format!("<bash-input>{}</bash-input>", escape(&result.command))}),
        json!({"type": "text", "text": format!(
            "<bash-stdout>{stdout}</bash-stdout><bash-stderr>{stderr}</bash-stderr>"
        )}),
    ]
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

/// `get_usage`: the plan rate-limit windows. `skip_behaviors` skips the scan
/// of the local transcripts, which a usage meter does not need.
#[must_use]
pub fn get_usage(request_id: &str) -> Value {
    control_request(request_id, "get_usage", json!({"skip_behaviors": true}))
}

/// `get_settings`: the settings that apply to the session. The host reads
/// only `applied.effort` of the answer (B18).
#[must_use]
pub fn get_settings(request_id: &str) -> Value {
    control_request(request_id, "get_settings", json!({}))
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

/// Response to a `can_use_tool` request. `tool_use_id` echoes the request's
/// `tool_use_id` as `toolUseID`, the same as the TypeScript SDK does (DL-007).
#[must_use]
pub fn permission_response(
    request_id: &str,
    decision: &PermissionDecision,
    tool_use_id: Option<&str>,
) -> Value {
    let mut body = match decision {
        PermissionDecision::Allow { updated_input } => {
            json!({"behavior": "allow", "updatedInput": updated_input})
        }
        PermissionDecision::Deny { message, interrupt } => {
            json!({"behavior": "deny", "message": message, "interrupt": interrupt})
        }
    };
    if let (Some(id), Some(map)) = (tool_use_id, body.as_object_mut()) {
        map.insert("toolUseID".to_owned(), Value::String(id.to_owned()));
    }
    control_success(request_id, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleetdeck_proto::command::ImageInput;

    #[test]
    fn a_claude_prompt_with_images_sends_image_blocks_before_the_text() {
        let plain = user_message(&UserInput::new("hi"), &[], "s", "u");
        assert_eq!(plain["message"]["content"], json!("hi"));
        let mut input = UserInput::new("what is this?");
        input.images.push(ImageInput {
            media_type: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        });
        let v = user_message(&input, &[], "s", "u");
        assert_eq!(
            v["message"]["content"],
            json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
                {"type": "text", "text": "what is this?"},
            ])
        );
    }

    fn result(command: &str, end: &str, stdout: &str, stderr: &str) -> ShellContext {
        ShellContext {
            command: command.to_owned(),
            end: end.to_owned(),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn shell_results_become_escaped_bash_blocks_before_the_prompt() {
        let context = [
            result("echo '<a&b>'", "exit 0", "<a&b>\n", ""),
            result("false", "exit 1", "", "boom"),
            result("sleep 9", "cancelled", "", ""),
        ];
        let v = user_message(&UserInput::new("go"), &context, "s", "u");
        assert_eq!(
            v["message"]["content"],
            json!([
                {"type": "text", "text": "<bash-input>echo '&lt;a&amp;b&gt;'</bash-input>"},
                {"type": "text", "text": "<bash-stdout>&lt;a&amp;b&gt;\n</bash-stdout><bash-stderr></bash-stderr>"},
                {"type": "text", "text": "<bash-input>false</bash-input>"},
                {"type": "text", "text": "<bash-stdout>(Bash completed with no output)</bash-stdout><bash-stderr>boom\n[exit 1]</bash-stderr>"},
                {"type": "text", "text": "<bash-input>sleep 9</bash-input>"},
                {"type": "text", "text": "<bash-stdout>(Bash completed with no output)</bash-stdout><bash-stderr>[cancelled]</bash-stderr>"},
                {"type": "text", "text": "go"},
            ])
        );
    }

    #[test]
    fn shell_results_go_before_the_images() {
        let mut input = UserInput::new("look");
        input.images.push(ImageInput {
            media_type: "image/png".to_owned(),
            data: "AA==".to_owned(),
        });
        let v = user_message(&input, &[result("ls", "exit 0", "x", "")], "s", "u");
        let kinds: Vec<_> = v["message"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(kinds, ["text", "text", "image", "text"]);
    }

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
            Some("toolu_1"),
        );
        assert_eq!(allow["response"]["response"]["toolUseID"], "toolu_1");
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
            None,
        );
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert!(deny["response"]["response"].get("toolUseID").is_none());
        assert_eq!(
            control_error("z", "unknown")["response"]["subtype"],
            "error"
        );
    }
}
