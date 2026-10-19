//! The `rmcp` service behind the station ingress and the bridge socket
//! (`docs/v2/design/mcp-p1.md` §3.1, §3.4).
//!
//! The listener, the pre-auth checks, the authentication and the limits stay
//! in `ingress.rs` and `socket.rs`. After them, [`McpService::serve`] hands
//! the request of an authenticated binding to `rmcp`, which answers the
//! protocol of both MCP eras, and returns the reply as plain data.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::Value;

use super::bindings::{Binding, Bindings};
use super::ingress::{FleetTools, Forward};
use super::mcp_server::{BindingExt, StationMcp};

/// The largest request body that `rmcp` takes; the same as the station
/// limit of the outer checks.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// The `Host` of a message that arrives on the bridge socket.
const BRIDGE_HOST: &str = "127.0.0.1";

/// The sentinel around a base64 header value (`rmcp` `decode_header_value`).
const BASE64_PREFIX: &str = "=?base64?";
const BASE64_SUFFIX: &str = "?=";

/// The reply of `rmcp` to one request.
#[derive(Debug, Clone)]
pub(super) struct Reply {
    pub status: StatusCode,
    pub content_type: Option<HeaderValue>,
    pub body: Bytes,
}

/// The `rmcp` service of a station.
#[derive(Clone)]
pub(super) struct McpService {
    config: StreamableHttpServerConfig,
    server: StationMcp,
}

impl McpService {
    pub(super) fn new(
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
    ) -> Self {
        // Stateless: no session state at all. JSON replies, no SSE in P1.
        // The Host check of `rmcp` stays on (`localhost`, `127.0.0.1`,
        // `::1`); an Origin header is refused as a second layer (the outer
        // 403 comes first).
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(MAX_BODY_BYTES)
            .enforce_origin_validation();
        Self {
            config,
            server: StationMcp::new(bindings, forward, fleet),
        }
    }

    /// Answers one request of an authenticated binding. The binding goes
    /// into the request extensions and nowhere else: it never comes from a
    /// header or from the body.
    ///
    /// A new `rmcp` service is built for every request. `rmcp` keeps a
    /// shared cache of tool schemas that is keyed by any tool name that a
    /// caller sends; a service of its own drops that cache with the request.
    ///
    /// The returned future is cancel-safe: when the caller drops it (the
    /// client closed the connection), `rmcp` cancels the request context.
    pub(super) async fn serve(&self, binding: Binding, body: Bytes, headers: &HeaderMap) -> Reply {
        let server = self.server.clone();
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(NeverSessionManager::default()),
            self.config.clone(),
        );
        let mut request = Request::new(Full::new(body));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = http::Uri::from_static("/mcp");
        for (name, value) in headers {
            // The credential is for the station only.
            if name != AUTHORIZATION {
                request.headers_mut().append(name.clone(), value.clone());
            }
        }
        if !request.headers().contains_key(ACCEPT) {
            request.headers_mut().insert(
                ACCEPT,
                HeaderValue::from_static("application/json, text/event-stream"),
            );
        }
        request
            .extensions_mut()
            .insert(BindingExt(Arc::new(binding)));
        let response = service.handle(request).await;
        let (head, body) = response.into_parts();
        let content_type = head.headers.get(CONTENT_TYPE).cloned();
        let body = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(never) => match never {},
        };
        Reply {
            status: head.status,
            content_type,
            body,
        }
    }
}

/// The value of an `Mcp-Method` or `Mcp-Name` header for `text`. A value
/// that is not a visible-ASCII header value, or that `rmcp` could mistake
/// for its own base64 form, is sent as `=?base64?<base64 of the UTF-8>?=`,
/// the form that `rmcp` decodes.
fn header_value(text: &str) -> Option<HeaderValue> {
    if !needs_base64(text)
        && let Ok(value) = HeaderValue::from_str(text)
    {
        return Some(value);
    }
    let encoded = format!("{BASE64_PREFIX}{}{BASE64_SUFFIX}", STANDARD.encode(text));
    HeaderValue::from_str(&encoded).ok()
}

/// True when `rmcp` cannot read `text` from a plain header: a character
/// outside visible ASCII (`HeaderValue::from_str` takes non-ASCII bytes but
/// `rmcp` reads the header with `to_str`), a space or tab at either end, or
/// the sentinel of the base64 form itself.
fn needs_base64(text: &str) -> bool {
    let visible = text.chars().all(|c| (' '..='~').contains(&c));
    let padded = text.starts_with([' ', '\t']) || text.ends_with([' ', '\t']);
    let sentinel = text.starts_with(BASE64_PREFIX) && text.ends_with(BASE64_SUFFIX);
    !visible || padded || sentinel
}

/// The headers that the station synthesizes for a message of the bridge
/// socket, which has no HTTP headers (design §3.4). `legacy` is the version
/// that the last successful `initialize` of the binding negotiated.
///
/// - Always: `Host`, `Content-Type` and `Accept`.
/// - A message with `params._meta["io.modelcontextprotocol/protocolVersion"]`
///   (a string): `MCP-Protocol-Version` with that value, `Mcp-Method` with the
///   method and `Mcp-Name` (`params.name` for `tools/call` and `prompts/get`,
///   `params.uri` for `resources/*`) when present.
/// - A message without that `_meta`, other than `initialize`, of a binding
///   with a recorded legacy version: `MCP-Protocol-Version` with it. An
///   `initialize` never gets a version header: `rmcp` negotiates from its
///   body.
pub(super) fn bridge_headers(message: &Value, legacy: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static(BRIDGE_HOST));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    let method = message.get("method").and_then(Value::as_str);
    let modern = message
        .pointer("/params/_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str);
    let version = match (modern, legacy) {
        (Some(version), _) => Some(version),
        (None, Some(version)) if method != Some("initialize") => Some(version),
        (None, _) => None,
    };
    if let Some(Ok(value)) = version.map(HeaderValue::from_str) {
        headers.insert(HeaderName::from_static("mcp-protocol-version"), value);
    }
    if modern.is_some() {
        if let Some(value) = method.and_then(header_value) {
            headers.insert(HeaderName::from_static("mcp-method"), value);
        }
        let key = match method {
            Some("tools/call" | "prompts/get") => Some("name"),
            Some("resources/read" | "resources/subscribe" | "resources/unsubscribe") => Some("uri"),
            _ => None,
        };
        let name = key
            .and_then(|key| message.pointer(&format!("/params/{key}")))
            .and_then(Value::as_str);
        if let Some(value) = name.and_then(header_value) {
            headers.insert(HeaderName::from_static("mcp-name"), value);
        }
    }
    headers
}
