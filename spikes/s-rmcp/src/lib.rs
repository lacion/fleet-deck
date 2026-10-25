//! S-RMCP spike: does `rmcp` 3.5.1 fit behind the station ingress?
//!
//! The station keeps the listener, the pre-auth checks and the bearer
//! authentication ([`outer`]); `rmcp` owns the protocol of both eras
//! ([`SpikeServer`]).

pub mod outer;

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, Json, RoleServer, ServerHandler, tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// A stand-in for the station binding of one session.
#[derive(Debug, Clone)]
pub struct Binding {
    pub session_id: String,
    pub classes: Vec<&'static str>,
}

/// The page that `list_things` returns as structured output.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ThingsPage {
    pub things: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Signals of the `slow` tool for the cancellation test (T10).
#[derive(Default)]
pub struct Probe {
    /// The handler started.
    pub started: Notify,
    /// `RequestContext::ct` was cancelled.
    pub ct_cancelled: Notify,
    /// The handler future was dropped (the drop guard ran).
    pub dropped: Notify,
}

struct DropSignal(Arc<Probe>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.dropped.notify_one();
    }
}

/// The class that a binding needs to see and use a tool (`None`: no class).
fn tool_class(name: &str) -> Option<&'static str> {
    match name {
        "poke" => Some("mail"),
        "slow" => Some("slow"),
        _ => None,
    }
}

/// Reads the station binding that the outer service put into the request
/// extensions. `rmcp` stores the `http::request::Parts` of the POST in the
/// extensions of the request context.
pub fn binding_of(ctx: &RequestContext<RoleServer>) -> Option<Binding> {
    ctx.extensions
        .get::<http::request::Parts>()?
        .extensions
        .get::<Binding>()
        .cloned()
}

#[derive(Clone)]
pub struct SpikeServer {
    tool_router: ToolRouter<Self>,
    probe: Arc<Probe>,
}

impl SpikeServer {
    pub fn new(probe: Arc<Probe>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            probe,
        }
    }
}

#[tool_router]
impl SpikeServer {
    /// Returns the session id of the binding of the request.
    #[tool(
        description = "Returns the session id of the caller.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn whoami(&self, ctx: RequestContext<RoleServer>) -> Result<String, ErrorData> {
        match binding_of(&ctx) {
            Some(binding) => Ok(binding.session_id),
            None => Err(ErrorData::internal_error("no binding in the request", None)),
        }
    }

    #[tool(
        description = "Lists the things.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_things(&self) -> Json<ThingsPage> {
        Json(ThingsPage {
            things: vec!["alpha".to_owned(), "beta".to_owned()],
            next_cursor: None,
        })
    }

    #[tool(
        description = "Pokes the mail box.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn poke(&self) -> String {
        "poked".to_owned()
    }

    #[tool(description = "Test only: sleeps 30 s and observes the cancellation.")]
    async fn slow(&self, ctx: RequestContext<RoleServer>) -> String {
        let _guard = DropSignal(self.probe.clone());
        self.probe.started.notify_one();
        tokio::select! {
            () = ctx.ct.cancelled() => {
                self.probe.ct_cancelled.notify_one();
                "cancelled".to_owned()
            }
            () = tokio::time::sleep(Duration::from_secs(30)) => "done".to_owned(),
        }
    }
}

impl ServerHandler for SpikeServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("fleetdeck", "0.0.0-spike"))
            .with_instructions("Fleet Deck tools for one session. Use whoami to see your session.")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let binding = binding_of(&context);
        let classes: &[&'static str] = binding.as_ref().map_or(&[], |b| b.classes.as_slice());
        // `ToolRouter::list_all` sorts by name: the order is the same for
        // every call. `with_all_items` sets `resultType` to `complete`; the
        // service clears it for a peer of the legacy era.
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|tool| tool_class(&tool.name).is_none_or(|class| classes.contains(&class)))
            .collect();
        let mut result = ListToolsResult::with_all_items(tools);
        if context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
        {
            result.ttl_ms = Some(0);
            result.cache_scope = Some(CacheScope::Private);
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}
