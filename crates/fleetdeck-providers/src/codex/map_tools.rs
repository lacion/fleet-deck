//! The kind and the title of a tool item (DL-063); moved out of `map.rs`
//! unchanged when B8 grew it.

use fleetdeck_proto::event::ToolKind;
use serde_json::Value;

use super::{QUESTION_ITEM, SUMMARY_CHARS, change_paths, str_at, truncate};

pub(super) fn tool_kind(item_type: &str) -> Option<ToolKind> {
    Some(match item_type {
        "commandExecution" => ToolKind::Shell,
        "fileChange" => ToolKind::Edit,
        "mcpToolCall" => ToolKind::Mcp,
        "webSearch" => ToolKind::Web,
        "collabAgentToolCall" | "subAgentActivity" => ToolKind::Subagent,
        "dynamicToolCall" | "imageView" | "imageGeneration" => ToolKind::Other,
        _ => return None,
    })
}

/// Title and input summary of a tool item (DL-063).
pub(super) fn tool_title(item: &Value, item_type: &str) -> (String, String) {
    let text = |k: &str| str_at(item, k).unwrap_or_default().to_owned();
    let (title, summary) = match item_type {
        "commandExecution" => ("Shell".to_owned(), text("command")),
        "fileChange" => ("Edit".to_owned(), change_paths(item).join(", ")),
        "mcpToolCall" => (
            format!("{}.{}", text("server"), text("tool")),
            item.get("arguments")
                .map(Value::to_string)
                .unwrap_or_default(),
        ),
        "webSearch" => ("Web search".to_owned(), text("query")),
        "collabAgentToolCall" => (
            str_at(item, "tool").unwrap_or("Agent").to_owned(),
            text("prompt"),
        ),
        "subAgentActivity" => ("Subagent".to_owned(), text("agentPath")),
        // The node of a `requestUserInput` question (DL-068).
        QUESTION_ITEM => ("Question".to_owned(), text("question")),
        "dynamicToolCall" => (
            str_at(item, "tool").unwrap_or(item_type).to_owned(),
            item.get("arguments")
                .map(Value::to_string)
                .unwrap_or_default(),
        ),
        other => (other.to_owned(), String::new()),
    };
    (title, truncate(&summary, SUMMARY_CHARS))
}
