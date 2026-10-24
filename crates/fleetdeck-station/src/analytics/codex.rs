//! The Codex rollout parser (design §3.4): the growth of the thread's
//! cumulative total, with the restart check and the counter epochs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Parsed, UsageRecord, has_bytes};
use crate::history::parse::parse_time_ms;

/// The fork or sub-agent copy burst ends after this gap.
const COPY_GAP_MS: i64 = 1000;
/// The longest response id that makes a key.
const MAX_RESPONSE_ID: usize = 128;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexState {
    /// The model of the last `turn_context`.
    pub model: String,
    /// The id of the first `session_meta` (this file's thread).
    pub thread: String,
    pub seen_meta: bool,
    /// The fork or sub-agent copy burst: copies set the baseline only.
    pub copying: bool,
    pub copy_anchor_ms: i64,
    /// The last cumulative totals of the thread.
    pub baseline: Option<Totals>,
    /// The counter epoch: the time of the first `session_meta` (else of the
    /// first usage line), and the time of the usage line of each restart
    /// (rules 5.4 and 5.7). Replay-stable: a read of the same bytes from 0
    /// gives the same epochs.
    pub epoch_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl Totals {
    fn at_least(&self, other: &Totals) -> bool {
        self.input >= other.input
            && self.cached >= other.cached
            && self.cache_write >= other.cache_write
            && self.output >= other.output
            && self.reasoning >= other.reasoning
    }

    fn minus(&self, other: &Totals) -> Totals {
        Totals {
            input: self.input - other.input,
            cached: self.cached - other.cached,
            cache_write: self.cache_write - other.cache_write,
            output: self.output - other.output,
            reasoning: self.reasoning - other.reasoning,
        }
    }

    fn from_json(value: &Value) -> Option<Totals> {
        let count = |key: &str| value.get(key).and_then(Value::as_u64);
        Some(Totals {
            input: count("input_tokens")?,
            output: count("output_tokens")?,
            cached: count("cached_input_tokens").unwrap_or(0),
            cache_write: count("cache_write_input_tokens").unwrap_or(0),
            reasoning: count("reasoning_output_tokens").unwrap_or(0),
        })
    }
}

/// The ancestry facts of the first `session_meta` of a rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMeta {
    pub thread: String,
    pub parent: Option<String>,
}

/// The ancestry of a `session_meta` payload: the thread and the parent in
/// one of the two encodings (the sub-agent spawn first).
fn meta_of(payload: &Value) -> ThreadMeta {
    fn text(value: Option<&Value>) -> Option<&str> {
        value.and_then(Value::as_str).filter(|s| !s.is_empty())
    }
    let thread = text(payload.get("id")).or_else(|| text(payload.get("session_id")));
    let spawned = payload
        .get("source")
        .and_then(|v| v.get("subagent"))
        .and_then(|v| v.get("thread_spawn"))
        .and_then(|v| v.get("parent_thread_id"));
    let parent = text(spawned).or_else(|| text(payload.get("parent_thread_id")));
    ThreadMeta {
        thread: thread.unwrap_or("").to_owned(),
        parent: parent.map(str::to_owned),
    }
}

/// The ancestry of one `session_meta` line; `None` for any other line or a
/// line without a thread id.
#[must_use]
pub fn parse_meta(line: &[u8]) -> Option<ThreadMeta> {
    if !has_bytes(line, b"session_meta") {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let meta = meta_of(value.get("payload")?);
    (!meta.thread.is_empty()).then_some(meta)
}

/// Parses one rollout line and moves the state.
#[must_use]
pub fn parse_line(line: &[u8], state: &mut CodexState) -> Parsed {
    const GATE: [&[u8]; 4] = [
        b"token_count",
        b"token_usage_record",
        b"turn_context",
        b"session_meta",
    ];
    if !GATE.iter().any(|needle| has_bytes(line, needle)) {
        return Parsed::Nothing;
    }
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Parsed::Malformed;
    };
    let time = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_time_ms);
    let payload = value.get("payload");
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "session_meta" => {
            if let Some(payload) = payload {
                session_meta(payload, time, state);
            }
            Parsed::Nothing
        }
        "turn_context" => {
            if let Some(model) = payload.and_then(|p| p.get("model")).and_then(Value::as_str) {
                state.model = model.to_owned();
            }
            Parsed::Nothing
        }
        "event_msg" => {
            let Some(payload) = payload else {
                return Parsed::Nothing;
            };
            if payload.get("type").and_then(Value::as_str) != Some("token_count") {
                return Parsed::Nothing;
            }
            let Some(info) = payload.get("info").filter(|i| i.is_object()) else {
                return Parsed::Nothing;
            };
            let (Some(c), Some(l)) = (info.get("total_token_usage"), info.get("last_token_usage"))
            else {
                return Parsed::Nothing;
            };
            usage_line(time, c, l, None, state)
        }
        "token_usage_record" => {
            let Some(payload) = payload else {
                return Parsed::Nothing;
            };
            let response = payload
                .get("response_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= MAX_RESPONSE_ID);
            let (Some(c), Some(l)) = (payload.get("thread_token_usage"), payload.get("usage"))
            else {
                return Parsed::Nothing;
            };
            usage_line(time, c, l, response, state)
        }
        _ => Parsed::Nothing,
    }
}

/// Rule 2 of the design: only the first `session_meta` counts.
fn session_meta(payload: &Value, time: Option<i64>, state: &mut CodexState) {
    if state.seen_meta {
        return;
    }
    state.seen_meta = true;
    let meta = meta_of(payload);
    state.thread = meta.thread;
    if let Some(at) = time {
        state.epoch_ms = at;
    }
    let forked = payload.get("forked_from_id").is_some_and(Value::is_string);
    if let (true, Some(at)) = (forked || meta.parent.is_some(), time) {
        state.copying = true;
        state.copy_anchor_ms = at;
    }
}

/// A usage line: `C` the cumulative totals, `L` this request, `R` the
/// response id. The rules of design §3.4 item 5, in order.
fn usage_line(
    time: Option<i64>,
    cumulative: &Value,
    request: &Value,
    response: Option<&str>,
    state: &mut CodexState,
) -> Parsed {
    let (Some(c), Some(l), Some(time)) = (
        Totals::from_json(cumulative),
        Totals::from_json(request),
        time,
    ) else {
        return Parsed::Malformed;
    };
    if state.epoch_ms == 0 {
        state.epoch_ms = time;
    }
    // 1. The copy burst of a fork or a sub-agent sets the baseline only.
    if state.copying {
        if time - state.copy_anchor_ms < COPY_GAP_MS {
            state.copy_anchor_ms = time;
            state.baseline = Some(c);
            return Parsed::Nothing;
        }
        state.copying = false;
    }
    // 2. The same request reported twice, or a replay.
    if state.baseline == Some(c) {
        return Parsed::Nothing;
    }
    // 3. A context-size report.
    if l.input + l.output == 0 {
        state.baseline = Some(c);
        return Parsed::Nothing;
    }
    // 4 to 7.
    let count = match state.baseline {
        Some(base) if base != Totals::default() && c == l => {
            state.epoch_ms = time;
            l
        }
        None if c.at_least(&l) => l,
        None => {
            state.baseline = Some(c);
            return Parsed::Malformed;
        }
        Some(base) if c.at_least(&base) => c.minus(&base),
        Some(_) => {
            state.epoch_ms = time;
            l
        }
    };
    // 8.
    state.baseline = Some(c);
    let input = count
        .input
        .saturating_sub(count.cached)
        .saturating_sub(count.cache_write);
    if input == 0 && count.cached == 0 && count.cache_write == 0 && count.output == 0 {
        return Parsed::Nothing;
    }
    let or_unknown = |text: &str| if text.is_empty() { "unknown" } else { text }.to_owned();
    let conversation = or_unknown(&state.thread);
    let key = match response {
        Some(id) => format!("codex:resp:{id}"),
        None => format!(
            "codex:{conversation}:{}:{}:{}",
            state.epoch_ms, c.input, c.output
        ),
    };
    Parsed::Record(UsageRecord {
        at: time,
        model: or_unknown(&state.model),
        conversation,
        key: Some(key),
        input,
        cache_read: count.cached,
        cache_write: count.cache_write,
        output: count.output,
        reasoning: count.reasoning,
    })
}
