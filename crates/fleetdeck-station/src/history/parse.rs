//! Parsing of one transcript record (DL-189 §3.4).
//!
//! The functions here read one JSON line of a Claude or Codex transcript and
//! say what it holds: history items, tool results, or nothing. A line that
//! looks like a conversation record but has a shape this build does not know
//! is [`Parsed::Unknown`]; the caller counts it as skipped.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryKind, ToolResult};
use serde_json::Value;

/// The text of an image block.
const IMAGE_TEXT: &str = "[image]";

/// The wrapper tags that mark a Claude user text as no real prompt.
const CLAUDE_WRAPPERS: &[&str] = &[
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-caveat",
    "system-reminder",
];

/// The starts of the Codex user blocks that the CLI injects.
const CODEX_INJECTED: &[&str] = &[
    "# AGENTS.md instructions",
    "<environment_context>",
    "<skill>",
    "<user_shell_command>",
    "<user_instructions>",
];

/// A Codex `response_item` payload type that holds no conversation.
const CODEX_IGNORED: &[&str] = &["reasoning", "agent_message"];

/// One item of a record, before the cuts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawItem {
    /// The provider's record id; `None` when the record has none.
    pub id: Option<String>,
    pub kind: HistoryKind,
    pub text: String,
    pub tool: Option<RawTool>,
}

/// A tool call, before the cuts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTool {
    pub name: String,
    pub summary: String,
    pub call_id: String,
}

/// What one line holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Not a conversation record (or blank).
    Ignored,
    /// A conversation-looking record that this build cannot read.
    Unknown,
    /// A record of a known shape.
    Known(Record),
}

/// A record of a known shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    pub items: Vec<RawItem>,
    /// `(tool call id, result)` of the tool results in the record.
    pub results: Vec<(String, ToolResult)>,
    /// A block of an unknown type was left out.
    pub partial: bool,
    /// The time of the record in Unix ms, if it has a parseable one.
    pub at: Option<i64>,
}

/// Parses one transcript line.
#[must_use]
pub fn parse_line(kind: ProviderKind, line: &[u8]) -> Parsed {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Parsed::Ignored;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Parsed::Unknown;
    };
    match kind {
        ProviderKind::Claude => claude(&value, false),
        ProviderKind::Codex => codex(&value),
        ProviderKind::Unknown => Parsed::Ignored,
    }
}

/// A record of a subagent transcript that names another session or agent
/// (DL-191).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotOurs;

/// Parses one line of a subagent transcript (DL-191). A conversation record
/// must have `isSidechain: true`, `sessionId == native` and
/// `agentId == agent_id`; the history rules then apply (meta, thinking and
/// wrappers dropped; tool calls and results paired). A conversation record
/// with another `sessionId` or `agentId` is [`NotOurs`]. A record without
/// `sessionId`, `agentId` or the sidechain flag is [`Parsed::Unknown`]
/// (skipped). Other record types are [`Parsed::Ignored`].
///
/// # Errors
/// [`NotOurs`] for a record of another session or agent.
pub fn parse_line_subagent(line: &[u8], native: &str, agent_id: &str) -> Result<Parsed, NotOurs> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(Parsed::Ignored);
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Ok(Parsed::Unknown);
    };
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("user" | "assistant")
    ) {
        return Ok(Parsed::Ignored);
    }
    let named = |key: &str| value.get(key).and_then(Value::as_str);
    let (Some(session), Some(agent)) = (named("sessionId"), named("agentId")) else {
        return Ok(Parsed::Unknown);
    };
    if session != native || agent != agent_id {
        return Err(NotOurs);
    }
    if value.get("isSidechain").and_then(Value::as_bool) != Some(true) {
        return Ok(Parsed::Unknown);
    }
    Ok(claude(&value, true))
}

/// The time of a record line, if the line is JSON with a parseable time.
#[must_use]
pub fn line_time(line: &[u8]) -> Option<i64> {
    let value = serde_json::from_slice::<Value>(line).ok()?;
    time_of(&value)
}

/// The session id that a record names (Claude `sessionId`, Codex
/// `session_meta.payload.id`).
#[must_use]
pub fn embedded_session(kind: ProviderKind, line: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(line).ok()?;
    match kind {
        ProviderKind::Claude => value.get("sessionId")?.as_str().map(str::to_owned),
        ProviderKind::Codex => {
            if value.get("type")?.as_str()? != "session_meta" {
                return None;
            }
            value.get("payload")?.get("id")?.as_str().map(str::to_owned)
        }
        ProviderKind::Unknown => None,
    }
}

fn time_of(value: &Value) -> Option<i64> {
    parse_time_ms(value.get("timestamp")?.as_str()?)
}

/// `2026-10-15T12:30:16.123Z` (or with a `+hh:mm` offset) to Unix ms.
#[must_use]
pub fn parse_time_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0_i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        let part = &frac[..digits.min(3)];
        millis = part.parse::<i64>().ok()? * 10_i64.pow(3 - u32::try_from(part.len()).ok()?);
        rest = &frac[digits..];
    }
    let offset = match rest {
        "" | "Z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6).unwrap_or("00").parse().ok()?;
            sign * (hh * 60 + mm)
        }
    };
    // Days from the civil date (proleptic Gregorian).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hour) * 60 + minute - offset) * 60_000 + second * 1000 + millis)
}

// ---------------------------------------------------------------- Claude

/// `own_sidechain`: the record comes from a subagent transcript, where the
/// sidechain flag is expected and does not drop the record.
fn claude(value: &Value, own_sidechain: bool) -> Parsed {
    let role = match value.get("type").and_then(Value::as_str) {
        Some("user") => HistoryKind::User,
        Some("assistant") => HistoryKind::Assistant,
        _ => return Parsed::Ignored,
    };
    let at = time_of(value);
    let flag = |name: &str| value.get(name).and_then(Value::as_bool).unwrap_or(false);
    let left_out =
        |flag: &dyn Fn(&str) -> bool| (flag("isSidechain") && !own_sidechain) || flag("isMeta");
    let content = value.get("message").and_then(|m| m.get("content"));
    let shape_ok = value.get("message").is_some_and(Value::is_object)
        && matches!(content, Some(Value::String(_) | Value::Array(_)));
    if !shape_ok {
        // A side chain or a meta record with a strange body is still left out.
        return if left_out(&flag) {
            Parsed::Known(Record {
                at,
                ..Record::default()
            })
        } else {
            Parsed::Unknown
        };
    }
    let mut record = Record {
        at,
        ..Record::default()
    };
    if left_out(&flag) {
        return Parsed::Known(record);
    }
    let uuid = value.get("uuid").and_then(Value::as_str).map(str::to_owned);
    match content {
        Some(Value::String(text)) => {
            if let Some(text) = claude_text(role, text) {
                record.items.push(item(uuid, role, text, None));
            }
        }
        Some(Value::Array(blocks)) => claude_blocks(role, blocks, &uuid, &mut record),
        _ => {}
    }
    Parsed::Known(record)
}

fn claude_blocks(role: HistoryKind, blocks: &[Value], uuid: &Option<String>, out: &mut Record) {
    let mut texts: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && let Some(text) = claude_text(role, text)
                {
                    texts.push(text);
                }
            }
            Some("image") => texts.push(IMAGE_TEXT.to_owned()),
            Some("thinking" | "redacted_thinking") => {}
            Some("tool_use") if role == HistoryKind::Assistant => {
                // Text before the call stays its own item, in order.
                flush_texts(role, &mut texts, uuid, out);
                let name = str_of(block, "name");
                let input = block.get("input").unwrap_or(&Value::Null);
                let tool = RawTool {
                    summary: claude_summary(&name, input),
                    name,
                    call_id: str_of(block, "id"),
                };
                out.items
                    .push(item(uuid.clone(), role, String::new(), Some(tool)));
            }
            Some("tool_result") if role == HistoryKind::User => {
                let status = if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                    ToolResult::Failed
                } else {
                    ToolResult::Ok
                };
                out.results.push((str_of(block, "tool_use_id"), status));
            }
            _ => out.partial = true,
        }
    }
    flush_texts(role, &mut texts, uuid, out);
}

/// Pushes the collected texts as one item (joined by a blank line).
fn flush_texts(
    role: HistoryKind,
    texts: &mut Vec<String>,
    uuid: &Option<String>,
    out: &mut Record,
) {
    if texts.is_empty() {
        return;
    }
    let joined = texts.join("\n\n");
    texts.clear();
    out.items.push(item(uuid.clone(), role, joined, None));
}

fn item(id: Option<String>, kind: HistoryKind, text: String, tool: Option<RawTool>) -> RawItem {
    RawItem {
        id,
        kind,
        text,
        tool,
    }
}

fn str_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A text of a Claude record: `None` when it is empty or only a wrapper.
fn claude_text(role: HistoryKind, text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    if role == HistoryKind::User && only_wrappers(text) {
        return None;
    }
    Some(text.to_owned())
}

/// True when the text is made only of command or hook wrappers.
fn only_wrappers(text: &str) -> bool {
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let Some(tag) = CLAUDE_WRAPPERS
            .iter()
            .find(|t| rest.starts_with(&format!("<{t}>")))
        else {
            return false;
        };
        let close = format!("</{tag}>");
        rest = match rest.find(&close) {
            Some(at) => rest[at + close.len()..].trim_start(),
            // An unclosed wrapper runs to the end.
            None => "",
        };
    }
    true
}

fn claude_summary(name: &str, input: &Value) -> String {
    let key = match name {
        "Bash" => "command",
        "Read" | "Write" | "Edit" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        "Glob" | "Grep" => "pattern",
        _ => return String::new(),
    };
    str_of(input, key)
}

// ----------------------------------------------------------------- Codex

fn codex(value: &Value) -> Parsed {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return Parsed::Ignored;
    }
    let at = time_of(value);
    let Some(payload) = value.get("payload").filter(|p| p.is_object()) else {
        return Parsed::Unknown;
    };
    let mut record = Record {
        at,
        ..Record::default()
    };
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => match payload.get("role").and_then(Value::as_str) {
            Some("user") => codex_message(payload, HistoryKind::User, &mut record),
            Some("assistant") => codex_message(payload, HistoryKind::Assistant, &mut record),
            Some("developer" | "system") => {}
            _ => return Parsed::Unknown,
        },
        Some(kind @ ("function_call" | "custom_tool_call")) => {
            let name = str_of(payload, "name");
            let summary = if kind == "function_call" {
                codex_summary(payload)
            } else {
                String::new()
            };
            let call_id = str_of(payload, "call_id");
            let tool = RawTool {
                name,
                summary,
                call_id: call_id.clone(),
            };
            record.items.push(RawItem {
                id: Some(call_id).filter(|c| !c.is_empty()),
                kind: HistoryKind::Tool,
                text: String::new(),
                tool: Some(tool),
            });
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            record
                .results
                .push((str_of(payload, "call_id"), codex_status(payload)));
        }
        Some(other) if CODEX_IGNORED.contains(&other) => {}
        _ => return Parsed::Unknown,
    }
    Parsed::Known(record)
}

fn codex_message(payload: &Value, role: HistoryKind, out: &mut Record) {
    let Some(blocks) = payload.get("content").and_then(Value::as_array) else {
        out.partial = true;
        return;
    };
    let wanted = if role == HistoryKind::User {
        "input_text"
    } else {
        "output_text"
    };
    let mut texts: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some(t) if t == wanted => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let head = text.trim_start();
                if text.trim().is_empty()
                    || (role == HistoryKind::User
                        && CODEX_INJECTED.iter().any(|p| head.starts_with(p)))
                {
                    continue;
                }
                texts.push(text.to_owned());
            }
            Some("input_image") => texts.push(IMAGE_TEXT.to_owned()),
            _ => out.partial = true,
        }
    }
    if !texts.is_empty() {
        out.items.push(RawItem {
            id: payload.get("id").and_then(Value::as_str).map(str::to_owned),
            kind: role,
            text: texts.join("\n\n"),
            tool: None,
        });
    }
}

/// The summary of a Codex function call: the shell command, a path or a
/// search pattern from its JSON arguments.
fn codex_summary(payload: &Value) -> String {
    let Some(args) = payload
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|a| serde_json::from_str::<Value>(a).ok())
    else {
        return String::new();
    };
    for key in ["cmd", "command", "file_path", "path", "pattern", "query"] {
        match args.get(key) {
            Some(Value::String(s)) => return s.clone(),
            Some(Value::Array(parts)) => {
                let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                return words.join(" ");
            }
            _ => {}
        }
    }
    String::new()
}

/// The status of a Codex tool output from its structured exit code.
fn codex_status(payload: &Value) -> ToolResult {
    let direct = payload.get("exit_code").and_then(Value::as_i64);
    let meta = payload
        .get("metadata")
        .and_then(|m| m.get("exit_code"))
        .and_then(Value::as_i64);
    let inner = payload
        .get("output")
        .and_then(Value::as_str)
        .and_then(|o| serde_json::from_str::<Value>(o).ok())
        .and_then(|o| o.get("metadata")?.get("exit_code")?.as_i64());
    match direct.or(meta).or(inner) {
        Some(0) => ToolResult::Ok,
        Some(_) => ToolResult::Failed,
        None => ToolResult::Unknown,
    }
}

// ------------------------------------------------------------------ Cuts

/// Cuts `text` to at most `max` bytes (the cut text ends with `… [cut]`).
#[must_use]
pub fn cut_text(text: &str, max: usize) -> String {
    const MARK: &str = "… [cut]";
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(MARK.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARK}", &text[..end])
}

/// Cuts `text` to at most `max` characters.
#[must_use]
pub fn cut_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Cuts `text` to at most `max` bytes, at a character boundary.
#[must_use]
pub fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
