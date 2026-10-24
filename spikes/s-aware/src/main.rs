//! S-AWARE spike server: a stdio MCP server that serves instructions, one
//! tool, two resources, one prompt, and logs every request method it gets.

use std::io::Write;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, DiscoverResult, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, Implementation, InitializeRequestParams, InitializeResult,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate, Role,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::transport::stdio;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

const INSTRUCTIONS: &str = "Fleet test server. The magic word is PLUM.";

/// Appends one line to stderr and to the file named by `AWARE_LOG`.
fn log(line: &str) {
    eprintln!("[aware] {line}");
    if let Ok(path) = std::env::var("AWARE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        && let Err(err) = writeln!(file, "{line}")
    {
        eprintln!("[aware] log write failed: {err}");
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EchoArgs {
    /// The text to echo.
    text: String,
}

#[derive(Clone)]
struct AwareServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AwareServer {
    #[tool(description = "Echoes the text back.")]
    async fn fleet_echo(&self, Parameters(args): Parameters<EchoArgs>) -> String {
        format!("echo:{}", args.text)
    }
}

impl ServerHandler for AwareServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("aware", "0.0.0-spike"))
            .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        log(&format!(
            "initialize protocolVersion={} client={}",
            request.protocol_version,
            serde_json::to_string(&request.client_info).unwrap_or_default()
        ));
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, ErrorData> {
        log("server/discover");
        Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        ))
    }

    async fn on_initialized(&self, _context: NotificationContext<RoleServer>) {
        log("notifications/initialized");
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        log("tools/list");
        Ok(ListToolsResult::with_all_items(self.tool_router.list_all()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        log(&format!("tools/call name={}", request.name));
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        log("resources/list");
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("fleet://stations", "stations")
                .with_description("The stations of the fleet.")
                .with_mime_type("text/markdown"),
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        log("resources/templates/list");
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("fleet://sessions/{id}", "session")
                .with_description("One session of the fleet.")
                .with_mime_type("text/markdown"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        log(&format!("resources/read uri={}", request.uri));
        let text = if request.uri == "fleet://stations" {
            "# Stations\n\n- Lighting (Linux)\n- mac-m4 (macOS)\n\nThe fruit word is APRICOT.\n"
                .to_owned()
        } else if let Some(id) = request.uri.strip_prefix("fleet://sessions/") {
            format!("# Session {id}\n\nThe fruit word is MANGO-{id}.\n")
        } else {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource {}", request.uri),
                None,
            ));
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, request.uri).with_mime_type("text/markdown"),
        ])
        .into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        log("prompts/list");
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            "validate_on_platform",
            Some("Validate a change on a platform."),
            Some(vec![
                PromptArgument::new("platform")
                    .with_description("The platform name.")
                    .with_required(true),
            ]),
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        log(&format!(
            "prompts/get name={} arguments={}",
            request.name,
            serde_json::to_string(&request.arguments).unwrap_or_default()
        ));
        if request.name != "validate_on_platform" {
            return Err(ErrorData::invalid_params(
                format!("unknown prompt {}", request.name),
                None,
            ));
        }
        let platform = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("platform"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!("Reply with the word KIWI and the platform name. The platform is {platform}."),
        )])
        .into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    log("start");
    let server = AwareServer {
        tool_router: AwareServer::tool_router(),
    };
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    log("stop");
    Ok(())
}
