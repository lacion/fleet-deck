//! MCP Apps for the provider adapters (B15, design `docs/v2/design/mcp-apps.md`
//! §4.2): the candidate of a completed MCP tool call, the [`AppBridge`] that
//! reaches the MCP servers of a live session through the provider, and the
//! rules that both providers share. The station decides what to capture and
//! what to call; an adapter only relays.

use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::NodeId;
use fleetdeck_proto::event::ProviderKind;
use serde_json::Value;

#[path = "apps_names.rs"]
mod names;
pub use names::{ClaudeName, ClaudeToolMap, claude_parse, claude_part, claude_tool_name};

/// A completed MCP tool call that may name an app (B15). Emitted by a core
/// as `Output::AppCandidate`; the station decides.
#[derive(Debug, Clone, PartialEq)]
pub struct AppCandidate {
    pub node_id: NodeId,
    /// Codex: the item's `server`. Claude: `None` (the station resolves it).
    pub server: Option<String>,
    /// Claude only: the raw server name of the `can_use_tool` request of this
    /// tool use (`mcp_server.name`), when the provider asked. Evidence that the
    /// station's resolution must agree with; `None` for Codex.
    pub server_hint: Option<String>,
    /// Codex: the item's `tool`. Claude: the full name `mcp__<server>__<tool>`.
    pub tool: String,
    /// Codex: `mcpAppUi.resourceUri`, else `mcpAppResourceUri`, only when it
    /// starts with `ui://`. Claude: `None`.
    pub resource_uri: Option<String>,
    /// The tool arguments (JSON object); `Value::Null` when over
    /// `fleetdeck_proto::apps::MAX_APP_INPUT_BYTES` serialized.
    pub arguments: Value,
    /// The `CallToolResult` (`content`, `structuredContent`; `_meta` only when
    /// it is an object); `None` when over `MAX_APP_RESULT_BYTES` serialized.
    pub result: Option<Value>,
}

/// One tool of one MCP server of a live session.
#[derive(Debug, Clone, PartialEq)]
pub struct AppTool {
    pub server: String,
    pub name: String,
    pub title: Option<String>,
    /// Codex `annotations.readOnlyHint == true`; Claude `annotations.readOnly == true`.
    pub read_only: bool,
    /// `_meta.ui.visibility` when it is an array of strings; `None` = default (model and app).
    pub visibility: Option<Vec<String>>,
    /// `_meta.ui.resourceUri`, else `_meta["ui/resourceUri"]`, when it starts with `ui://`.
    pub resource_uri: Option<String>,
}

/// Why an app operation has no result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    /// The provider answered with an error (the text as the provider sent it).
    /// For `tools` and `read_resource` the operation ended. For `call_tool` it
    /// proves **nothing** about the tool: the provider also answers an error
    /// after its own tool timeout, a cancellation, a lost transport, or a
    /// result over its size cap after the tool ran (fact 21). The station
    /// treats it as an unproven end (§4.6.1).
    #[error("provider error: {0}")]
    Answered(String),
    /// The provider process ended before an answer. For a tool call this is
    /// the end of the call (its stdio MCP servers end with the provider).
    /// Only the observed exit of the provider process gives it: a detach of
    /// the host (station shutdown, `DurableDriver::abandon`) or a drop of the
    /// bridge never does (§4.6.1, "End proof").
    #[error("the session ended")]
    Ended,
    /// `tools` and `read_resource` only: no answer within their timeout.
    #[error("no answer in time")]
    Timeout,
    /// The provider has no app path (Claude without the three capabilities).
    /// Nothing was sent to the provider.
    #[error("not supported by this provider")]
    Unsupported,
}

/// The answer of [`AppBridge::call_tool`].
#[derive(Debug, Clone, PartialEq)]
pub struct ToolAnswer {
    /// The `CallToolResult` JSON that the app receives.
    pub result: Value,
    /// `true` only when the answer proves that the server's tool handler
    /// returned. Claude: always `true` (its success answer relays the server's
    /// result; its timeouts and transport errors are error answers, fact 21).
    /// Codex: `false` when `result.isError` is `true` (Codex may carry a
    /// client-side `tools/call` timeout as an error result; not recorded).
    pub proves_end: bool,
}

/// The MCP operations of a live session for its apps. Cloneable handle: it
/// never needs the station's `Live` lock. When the provider process ends,
/// every waiting operation ends with `AppError::Ended`. A detach of the host
/// ends no operation with `AppError::Ended`: the waits stay pending.
#[async_trait]
pub trait AppBridge: Send + Sync {
    /// Waits at most `TOOLS_TIMEOUT`.
    async fn tools(&self) -> Result<Vec<AppTool>, AppError>;
    /// `ReadResourceResult` JSON (`{ "contents": [...] }`). Waits at most `READ_TIMEOUT`.
    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, AppError>;
    /// **No timeout of its own:** it waits for the provider's answer or the
    /// end of the process. The provider's own tool timeout still applies and
    /// gives `AppError::Answered`, which proves no end (§4.6.1). The station
    /// passes only a pair that it validated (Claude: `ClaudeToolMap::address`).
    async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolAnswer, AppError>;
}

pub const TOOLS_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the station waits for a tool call before it answers `timeout`;
/// the call itself goes on (§4.6.1). The provider's own tool timeout can be
/// shorter or longer; it does not end the station's wait or the row.
pub const CALL_REPLY_WAIT: Duration = Duration::from_secs(60);
/// The provider families and minimum versions with a recorded app path.
pub const APPS_VERIFIED: [(ProviderKind, &str); 2] = [
    (ProviderKind::Claude, "2.1.308"),
    (ProviderKind::Codex, "0.163.0"),
];

/// Whether the app path is recorded for a provider version: true only for a
/// known provider with a parsed version at or above its [`APPS_VERIFIED`]
/// minimum.
#[must_use]
pub fn apps_verified(kind: ProviderKind, version: Option<&str>) -> bool {
    let Some(min) = APPS_VERIFIED
        .iter()
        .find(|(k, _)| *k == kind)
        .and_then(|(_, min)| numeric(min))
    else {
        return false;
    };
    version.and_then(numeric).is_some_and(|got| got >= min)
}

/// The numeric parts of a dotted version (`2.1.308 (Claude Code)` →
/// `[2, 1, 308]`), or `None` when it does not start with a number.
fn numeric(version: &str) -> Option<Vec<u64>> {
    let head = version.split_whitespace().next()?;
    let parts: Vec<u64> = head.split('.').map_while(|p| p.parse().ok()).collect();
    (!parts.is_empty()).then_some(parts)
}

/// `"text/html;profile=mcp-app"` after removing whitespace and lowercasing.
#[must_use]
pub fn is_app_mime(mime: &str) -> bool {
    let compact: String = mime
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    compact == fleetdeck_proto::apps::MCP_APP_MIME
}

/// `visibility` and `resource_uri` of the `_meta` object of an MCP tool
/// (`_meta.ui.visibility` when it is an array of strings; `_meta.ui.resourceUri`,
/// else `_meta["ui/resourceUri"]`, when it starts with `ui://`). Both providers
/// list their tools with this `_meta` shape.
#[must_use]
pub fn ui_meta(meta: Option<&Value>) -> (Option<Vec<String>>, Option<String>) {
    let ui = meta.and_then(|m| m.get("ui"));
    let visibility = ui
        .and_then(|u| u.get("visibility"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<String>>>()
        });
    let resource_uri = ui
        .and_then(|u| u.get("resourceUri"))
        .or_else(|| meta.and_then(|m| m.get("ui/resourceUri")))
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("ui://"))
        .map(str::to_owned);
    (visibility, resource_uri)
}

/// The serialized size of a JSON value, without building the text.
#[must_use]
pub fn json_len(value: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing to a counter cannot fail; a failure counts what was written.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
