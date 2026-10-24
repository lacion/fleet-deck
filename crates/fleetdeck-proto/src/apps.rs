//! MCP Apps (B15, design `docs/v2/design/mcp-apps.md` §4.5): the wire types of
//! the link methods `session.app_meta`, `session.app_document`,
//! `session.app_call`, `session.app_calls` and `session.app_release` (feature
//! `mcp_apps.v1`) and of the deck routes of the board.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ids::{NodeId, SessionId};

/// The MIME type of an MCP App document.
pub const MCP_APP_MIME: &str = "text/html;profile=mcp-app";
/// The MCP extension name that a host offers to the servers.
pub const MCP_APP_EXTENSION: &str = "io.modelcontextprotocol/ui";
/// The largest stored app document.
pub const MAX_APP_DOC_BYTES: usize = 2 * 1024 * 1024;
/// The largest stored tool input of an app.
pub const MAX_APP_INPUT_BYTES: usize = 64 * 1024;
/// The largest stored tool result of an app.
pub const MAX_APP_RESULT_BYTES: usize = 512 * 1024;
/// The largest serialized `arguments` of an app `tools/call`.
pub const MAX_APP_ARGS_BYTES: usize = 64 * 1024;
/// The largest serialized provider result that an app call returns.
pub const MAX_APP_REPLY_BYTES: usize = 768 * 1024;
/// The largest raw size of one `session.app_document` chunk.
pub const APP_DOC_CHUNK_BYTES: usize = 256 * 1024;

/// The CSP domains of an app (`_meta.ui.csp`), sanitized by the station.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCsp {
    #[serde(default)]
    pub connect_domains: Vec<String>,
    #[serde(default)]
    pub resource_domains: Vec<String>,
}

/// `session.app_meta` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppMetaQuery {
    pub session_id: SessionId,
    pub app_id: String,
}

/// `session.app_document` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppDocumentQuery {
    pub session_id: SessionId,
    pub app_id: String,
    #[ts(type = "number")]
    pub offset: u64,
}

/// `session.app_meta` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppMeta {
    pub app_id: String,
    pub node_id: NodeId,
    pub server: String,
    pub tool: String,
    pub resource_uri: String,
    pub csp: AppCsp,
    pub prefers_border: Option<bool>,
    #[ts(type = "number")]
    pub doc_bytes: u64,
    pub doc_sha256: String,
    #[ts(type = "unknown")]
    pub tool_input: Value,
    #[ts(type = "unknown")]
    pub tool_result: Option<Value>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// The state of an open app call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "apps/")]
pub enum AppCallState {
    Running,
    Uncertain,
}

/// Why an app call is `uncertain`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "apps/")]
pub enum AppUncertainCause {
    StationRestart,
    ProviderError,
}

/// One open app call (§4.6.1). It does not need the app's row: `app_id` can
/// name an app that the caps deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppOpenCall {
    pub call_id: String,
    pub session_id: SessionId,
    pub app_id: String,
    pub server: String,
    pub tool: String,
    pub state: AppCallState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cause: Option<AppUncertainCause>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// `session.app_calls` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCallsQuery {
    pub session_id: SessionId,
}

/// `session.app_calls` answer (also the deck answer of the list route).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCallsReply {
    pub calls: Vec<AppOpenCall>,
}

/// `session.app_release` params. The session is the call's own session
/// (`AppOpenCall.session_id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppReleaseParams {
    pub session_id: SessionId,
    pub call_id: String,
}

/// `session.app_release` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppReleaseReply {
    pub released: bool,
}

/// `session.app_document` answer: at most [`APP_DOC_CHUNK_BYTES`] raw bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppDocumentChunk {
    pub data_b64: String,
    pub eof: bool,
    #[ts(type = "number")]
    pub total: u64,
}

/// A request of an app to its own MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "method")]
#[ts(export_to = "apps/")]
pub enum AppRequest {
    #[serde(rename = "tools/call")]
    ToolsCall {
        name: String,
        #[serde(default)]
        #[ts(type = "unknown")]
        arguments: Value,
    },
    #[serde(rename = "resources/read")]
    ResourcesRead { uri: String },
}

/// `session.app_call` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCallParams {
    pub session_id: SessionId,
    pub app_id: String,
    pub request: AppRequest,
    #[serde(default)]
    pub confirmed: bool,
}

/// `session.app_call` answer and the deck answer of the call route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[ts(export_to = "apps/")]
pub enum AppCallReply {
    Done {
        #[ts(type = "unknown")]
        result: Value,
    },
    Confirm {
        tool: String,
        title: Option<String>,
    },
}

/// Board to deck body of the call route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppCallBody {
    pub request: AppRequest,
    #[serde(default)]
    pub confirmed: bool,
}

/// Deck to board answer of the view route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "apps/")]
pub struct AppViewReply {
    pub url: String,
    #[ts(type = "number")]
    pub expires_at: i64,
    pub server: String,
    pub tool: String,
    pub resource_uri: String,
    pub csp: AppCsp,
    pub prefers_border: Option<bool>,
    #[ts(type = "unknown")]
    pub tool_input: Value,
    #[ts(type = "unknown")]
    pub tool_result: Option<Value>,
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
