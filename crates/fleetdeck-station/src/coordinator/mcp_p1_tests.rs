//! MCP P1 protocol-core tests (`docs/v2/design/mcp-p1.md` §6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;

use super::bindings::{Binding, Bindings, digest};
use super::ingress::Forward;
use super::mcp_service::{McpService, bridge_headers};

/// Sends one JSON-RPC message of `binding` through [`McpService::serve`] and
/// returns the JSON body of the reply. `legacy` is the version header of a
/// legacy message (a modern message carries its version in `_meta`). The
/// binding is inside a [`Bindings`], so the P6 recheck sees it.
pub(super) async fn serve_json(
    binding: &Binding,
    forward: Arc<dyn Forward>,
    fleet_control: bool,
    legacy: Option<&str>,
    message: &Value,
) -> Value {
    let bindings = Bindings::default();
    bindings.set_fleet_control(fleet_control);
    bindings.insert(digest("serve-json"), binding.clone());
    let service = McpService::new(bindings, forward, None);
    let headers = bridge_headers(message, legacy);
    let reply = service
        .serve(binding.clone(), Bytes::from(message.to_string()), &headers)
        .await;
    serde_json::from_slice(&reply.body).expect("the reply is JSON")
}
