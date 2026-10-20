//! `StationMcp`: the `rmcp` server handler of the coordinator endpoint
//! (`docs/v2/design/mcp-p1.md` §3.2, §3.3).
//!
//! `rmcp` owns the protocol of both MCP eras. This handler owns what the
//! station decides: the catalog of the binding, the class check of every
//! call, the dispatch (`jsonrpc::call`) and the cancellation. The binding
//! comes only from the request extensions that the station put there
//! ([`BindingExt`]); `clientInfo`, `_meta` and the arguments never change
//! the caller.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, CompleteRequestMethod,
    CompleteRequestParams, CompleteResult, ContentBlock, DiscoverResult, GetPromptRequestParams,
    GetPromptResponse, Implementation, InitializeRequestParams, InitializeResult,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ResourceContents, ResultType, ServerCapabilities, ServerConfig,
    SubscriptionFilter, Tool,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};

use super::bindings::{Binding, BindingHandle, BindingState, Bindings, ToolClass};
use super::desktop_forward::Rechecked;
use super::ingress::{FleetTools, Forward};
use super::jsonrpc::{self, CallOutcome, NOT_BOUND};
use super::mcp_fleet::{self, ResourceRef};
use super::tools;

/// The binding of an authenticated request, in the request extensions. A
/// newtype, so that no other code inserts a binding by accident.
#[derive(Clone)]
pub(super) struct BindingExt(pub Arc<Binding>);

/// The change handle of a `subscriptions/listen` request, in the request
/// extensions next to [`BindingExt`] (MCP P4b §8.2). Only the listen path of
/// the ingress puts it there.
#[derive(Clone)]
pub(super) struct ListenHandle(pub BindingHandle);

/// The protocol versions that the station serves: the legacy versions that
/// the providers use and the stateless version of 2026-07-28. Not
/// `2024-11-05`.
static SUPPORTED: [ProtocolVersion; 4] = [
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2026_07_28,
];

/// How long a client may keep a `tools/list` result of the modern era, in
/// milliseconds. The scope is `private`: the catalog depends on the binding.
const LIST_TTL_MS: u64 = 60_000;

/// The text of a desktop result that does not convert to an MCP result.
const DESKTOP_ENCODING: &str = "internal: the desktop result could not be encoded";

/// The tools whose result carries `structuredContent` (design §4.2).
fn is_structured(name: &str) -> bool {
    matches!(name, "publish_artifact" | "set_focus")
}

/// The `rmcp` form of the catalog of a grant, in the fixed order of
/// [`tools::catalog`].
///
/// # Errors
/// An entry of the catalog does not convert to an `rmcp` tool.
pub(super) fn rmcp_tools(classes: &BTreeSet<ToolClass>) -> Result<Vec<Tool>, serde_json::Error> {
    let catalog = tools::catalog(classes);
    catalog
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|entry| serde_json::from_value::<Tool>(entry.clone()))
        .collect()
}

/// The `rmcp` handler of one station. Cheap to clone (`Arc`s inside).
#[derive(Clone)]
pub(super) struct StationMcp {
    bindings: Bindings,
    forward: Arc<dyn Forward>,
    fleet: Option<Arc<dyn FleetTools>>,
}

impl StationMcp {
    pub(super) fn new(
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
    ) -> Self {
        Self {
            bindings,
            forward,
            fleet,
        }
    }

    /// The server description with the generated instructions of the
    /// binding (MCP P3 §3.3).
    fn info_for(&self, binding: &Binding) -> ServerConfig {
        let mut info = self.get_info();
        info.instructions = Some(self.instructions(binding));
        info
    }

    /// The generated instructions of a binding, from its catalog and the
    /// station facts that the station recorded.
    fn instructions(&self, binding: &Binding) -> String {
        mcp_fleet::instructions(binding, self.bindings.station().as_ref())
    }
}

/// The binding of the request, or `internal_error` when the station put none
/// (a programming error: the call then fails closed).
fn binding_of(context: &RequestContext<RoleServer>) -> Result<Arc<Binding>, McpError> {
    context
        .extensions
        .get::<http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<BindingExt>())
        .map(|ext| ext.0.clone())
        .ok_or_else(|| McpError::internal_error("no binding", None))
}

/// A tool error result with a fixed text.
fn tool_error(text: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}

/// The MCP result of a finished call. Every result sets `resultType`
/// `complete` explicitly (`rmcp` removes it for a legacy peer).
fn into_result(name: &str, outcome: CallOutcome) -> CallToolResult {
    let mut result = match outcome {
        CallOutcome::Error(text) => tool_error(text),
        CallOutcome::Value(value) if is_structured(name) => CallToolResult::structured(value),
        CallOutcome::Value(value) => {
            CallToolResult::success(vec![ContentBlock::text(value.to_string())])
        }
        CallOutcome::Desktop(desktop) => {
            match serde_json::from_value::<CallToolResult>(desktop.to_value()) {
                Ok(converted) => converted,
                Err(_) => {
                    // The tool name only, never the value.
                    tracing::warn!(tool = %name, "a desktop result did not convert to an MCP result");
                    tool_error(DESKTOP_ENCODING)
                }
            }
        }
    };
    result.result_type = Some(ResultType::COMPLETE);
    result
}

impl ServerHandler for StationMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("fleetdeck", env!("CARGO_PKG_VERSION")))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&SUPPORTED[..])
    }

    /// Only `toolsListChanged` is served (MCP P4b §8.2).
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        (requested.tools_list_changed == Some(true))
            .then(|| requested.supported_by(&self.get_info().capabilities))
    }

    /// One `tools/list_changed` right after the acknowledgment (the client
    /// re-lists once: this covers every change before the listen), then one
    /// per version change of the credential. The stream ends with rmcp's
    /// final result when the credential ended or the client left.
    async fn listen(&self, ctx: SubscriptionContext) -> Result<(), McpError> {
        let Some(mut changes) = ctx
            .request_context()
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<ListenHandle>())
            .map(|handle| handle.0.changes.clone())
        else {
            return Ok(());
        };
        if ctx.sink().notify_tool_list_changed().await.is_err() {
            return Ok(());
        }
        loop {
            tokio::select! {
                () = ctx.cancelled() => return Ok(()),
                changed = changes.changed() => {
                    if changed.is_err() || ctx.sink().notify_tool_list_changed().await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let binding = binding_of(&context)?;
        context.peer.set_peer_info(request.clone());
        let mut info = self.negotiate_initialize(&request)?;
        info.instructions = Some(self.instructions(&binding));
        // Only a successful negotiation is remembered (bridge header hint).
        self.bindings
            .set_legacy_version(&binding.binding_id, info.protocol_version.as_str());
        Ok(info)
    }

    async fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, McpError> {
        let binding = binding_of(&context)?;
        Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.info_for(&binding),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let binding = binding_of(&context)?;
        let tools = rmcp_tools(&binding.classes)
            .map_err(|_| McpError::internal_error("the tool catalog could not be encoded", None))?;
        let mut result = ListToolsResult::with_all_items(tools);
        // The cache hints exist in the modern era only.
        if context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
        {
            result = result
                .with_ttl_ms(LIST_TTL_MS)
                .with_cache_scope(CacheScope::Private);
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let binding = binding_of(&context)?;
        let name = request.name.into_owned();
        let args = request
            .arguments
            .map_or_else(|| Value::Object(Map::new()), Value::Object);
        // A P6 result reaches the agent only after the caller policy and the
        // binding are checked again (R13 P6 §4.9).
        let forward = Rechecked {
            inner: self.forward.as_ref(),
            bindings: &self.bindings,
        };
        let work = jsonrpc::call(
            &binding,
            &forward,
            self.fleet.as_deref(),
            self.bindings.fleet_control(),
            &name,
            args,
        );
        // The station's own tools (`publish_artifact`, `set_focus`) run to
        // their end even when the client went away: they are local and
        // bounded, and a dropped artifact staging job would leave its files
        // until a restart (P1 PRE-PR finding).
        // The fleet view is `Own` but a request to the deck: a closed
        // connection cancels it.
        let outcome = if tools::tool(&name).is_some_and(|t| t.is_own() && t.name != "fleet_view") {
            work.await
        } else {
            // A closed connection cancels `context.ct`: the work future is
            // dropped (design §3.3 step 4) and no response reaches the
            // client. The cancel branch is first: a call that is already
            // cancelled is not started.
            tokio::select! {
                biased;
                () = context.ct.cancelled() => {
                    return Err(McpError::internal_error("cancelled", None));
                }
                outcome = work => outcome,
            }
        };
        Ok(CallToolResponse::Complete(into_result(&name, outcome)))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        let tool = tools::tool(name)?;
        serde_json::from_value(json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": (tool.schema)(),
        }))
        .ok()
    }

    // The `rmcp` default of `complete` answers success. The station serves
    // no completions.

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Err(McpError::method_not_found::<CompleteRequestMethod>())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let binding = binding_of(&context)?;
        let prompts = mcp_fleet::prompts(&mcp_fleet::catalog_names(&binding));
        Ok(with_hints_prompts(
            ListPromptsResult::with_all_items(prompts),
            &context,
        ))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let binding = binding_of(&context)?;
        let args = request.arguments.unwrap_or_default();
        let result = mcp_fleet::prompt(&request.name, &args, &mcp_fleet::catalog_names(&binding))?;
        Ok(GetPromptResponse::Complete(result))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        binding_of(&context)?;
        Ok(with_hints_resources(
            ListResourcesResult::with_all_items(mcp_fleet::resources()),
            &context,
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        binding_of(&context)?;
        Ok(with_hints_templates(
            ListResourceTemplatesResult::with_all_items(Vec::new()),
            &context,
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let binding = binding_of(&context)?;
        let uri = request.uri;
        let Some(resource) = mcp_fleet::parse_uri(&uri) else {
            return Err(McpError::resource_not_found(
                "unknown resource",
                Some(json!({"uri": uri})),
            ));
        };
        let (text, mime) = match &resource {
            // The guide needs no deck and no active binding.
            ResourceRef::Guide => (mcp_fleet::guide(&binding), "text/markdown"),
            ResourceRef::View {
                view,
                cursor,
                station,
            } => (
                self.read_view(&binding, *view, cursor.clone(), station.clone())
                    .await,
                "application/json",
            ),
        };
        let mut result =
            ReadResourceResult::new(vec![ResourceContents::text(text, uri).with_mime_type(mime)]);
        if is_modern(&context) {
            result = result
                .with_ttl_ms(resource.ttl_ms())
                .with_cache_scope(CacheScope::Private);
        }
        Ok(ReadResourceResponse::Complete(result))
    }
}

impl StationMcp {
    /// The text of a view read: the compact JSON `{"items", "next_cursor"}`,
    /// or `{"error": "<text>"}`. The caller is the binding's only (§3.1).
    async fn read_view(
        &self,
        binding: &Binding,
        view: fleetdeck_proto::fleet_view::FleetView,
        cursor: Option<String>,
        station: Option<fleetdeck_proto::StationId>,
    ) -> String {
        let Some(caller) = binding
            .caller()
            .filter(|_| binding.state == BindingState::Active)
        else {
            return mcp_fleet::error_text(NOT_BOUND);
        };
        let forward = Rechecked {
            inner: self.forward.as_ref(),
            bindings: &self.bindings,
        };
        match forward
            .fleet_view(caller, view, cursor, station)
            .await
            .and_then(mcp_fleet::page_value)
        {
            Ok(page) => page.to_string(),
            Err(text) => mcp_fleet::error_text(&text),
        }
    }
}

/// True for a client of the modern era, which reads the cache hints.
fn is_modern(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

fn with_hints_prompts(
    result: ListPromptsResult,
    context: &RequestContext<RoleServer>,
) -> ListPromptsResult {
    if is_modern(context) {
        result
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private)
    } else {
        result
    }
}

fn with_hints_resources(
    result: ListResourcesResult,
    context: &RequestContext<RoleServer>,
) -> ListResourcesResult {
    if is_modern(context) {
        result
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private)
    } else {
        result
    }
}

fn with_hints_templates(
    result: ListResourceTemplatesResult,
    context: &RequestContext<RoleServer>,
) -> ListResourceTemplatesResult {
    if is_modern(context) {
        result
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private)
    } else {
        result
    }
}
