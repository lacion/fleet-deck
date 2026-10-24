//! The Claude transcript parser (design §3.4). Stateless: one line, one
//! result. The dedupe of the records of one message is the store's job.

use serde::Deserialize;

use super::{Parsed, UsageRecord, has_bytes};
use crate::history::parse::parse_time_ms;

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Parses one transcript line.
#[must_use]
pub fn parse_line(line: &[u8]) -> Parsed {
    if !has_bytes(line, b"\"usage\"") || !has_bytes(line, b"\"assistant\"") {
        return Parsed::Nothing;
    }
    let Ok(parsed) = serde_json::from_slice::<Line>(line) else {
        return Parsed::Malformed;
    };
    if parsed.kind.as_deref() != Some("assistant") {
        return Parsed::Nothing;
    }
    let Some(message) = parsed.message else {
        return Parsed::Nothing;
    };
    let Some(usage) = message.usage else {
        return Parsed::Nothing;
    };
    let model = match message.model {
        Some(model) if !model.is_empty() && model != "<synthetic>" => model,
        _ => return Parsed::Nothing,
    };
    let Some(at) = parsed.timestamp.as_deref().and_then(parse_time_ms) else {
        return Parsed::Malformed;
    };
    let conversation = match parsed.session_id {
        Some(id) if !id.is_empty() => id,
        _ => return Parsed::Malformed,
    };
    let key = if message.id.is_none() && parsed.request_id.is_none() {
        None
    } else {
        Some(format!(
            "claude:{}:{}",
            message.id.as_deref().unwrap_or(""),
            parsed.request_id.as_deref().unwrap_or("")
        ))
    };
    Parsed::Record(UsageRecord {
        at,
        model,
        conversation,
        key,
        input: usage.input_tokens.unwrap_or(0),
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        reasoning: 0,
    })
}
