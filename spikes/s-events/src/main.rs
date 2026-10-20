//! S-EVENTS spike server: an HTTP rmcp server that exercises MCP events
//! (subscriptions, list changes, resource updates, progress, cancellation,
//! cache TTLs) and logs what the host does.
//!
//! Flags: `--port N` (default 0: any), `--stateless` (default) or `--stateful`,
//! `--json` (default) or `--sse`, `--no-listen` (no `subscriptions/listen`).
//! Environment: `EVENTS_DIR` (the `bump` trigger file), `EVENTS_LOG` (log file),
//! `EVENTS_NO_PROGRESS` (never send progress notifications).

use std::convert::Infallible;
use std::io::Write;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper_util::rt::TokioIo;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CancelledNotificationParam,
    Implementation, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProgressNotificationParam, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ResourceUpdatedNotificationParam,
    ServerCapabilities, ServerConfig, SubscribeRequestParams, SubscriptionFilter, Tool,
    UnsubscribeRequestParams,
};
use rmcp::service::{NotificationContext, Peer, RequestContext, SubscriptionContext};
use rmcp::transport::streamable_http_server::session::SessionManager;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

const BOARD_URI: &str = "fleet://board";
const TTL_MS: u64 = 5_000;

/// Seconds since the first log line, for the log stamps.
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Appends one line to stderr and to the file named by `EVENTS_LOG`.
fn log(line: &str) {
    let start = *START.get_or_init(Instant::now);
    let stamp = format!("{:8.3}", start.elapsed().as_secs_f64());
    eprintln!("[events {stamp}] {line}");
    if let Ok(path) = std::env::var("EVENTS_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        && let Err(err) = writeln!(file, "[{stamp}] {line}")
    {
        eprintln!("[events] log write failed: {err}");
    }
}

/// One event for the open streams.
#[derive(Debug, Clone)]
enum Event {
    ToolsChanged,
    ResourceUpdated(String),
}

struct Shared {
    late_enabled: AtomicBool,
    board: AtomicU64,
    /// Events for the `subscriptions/listen` streams.
    events: broadcast::Sender<Event>,
    /// Legacy peers (one per session, `--stateful`), set at `initialized`.
    peers: Mutex<Vec<Peer<RoleServer>>>,
    /// Legacy `resources/subscribe` records: (peer, uri).
    legacy_subscriptions: Mutex<Vec<(Peer<RoleServer>, String)>>,
    listen: bool,
}

#[derive(Clone)]
struct EventsServer {
    tool_router: ToolRouter<Self>,
    shared: Arc<Shared>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SlowArgs {
    /// How many seconds to count (at most 120).
    seconds: u64,
}

/// Logs when the value drops (a handler future that is dropped).
struct DropLog(String);

impl Drop for DropLog {
    fn drop(&mut self) {
        log(&self.0);
    }
}

fn token_text(token: Option<rmcp::model::ProgressToken>) -> String {
    token.map_or_else(
        || "none".to_owned(),
        |t| serde_json::to_string(&t).unwrap_or_default(),
    )
}

#[tool_router]
impl EventsServer {
    #[tool(
        description = "Counts to the given number of seconds (at most 120) and returns done:<n>."
    )]
    async fn slow_count(
        &self,
        Parameters(args): Parameters<SlowArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
        let seconds = args.seconds.min(120);
        // `EVENTS_NO_PROGRESS` (any value): never send progress (a control run).
        let token = if std::env::var_os("EVENTS_NO_PROGRESS").is_some() {
            None
        } else {
            ctx.meta.get_progress_token()
        };
        log(&format!(
            "slow_count start id={} seconds={seconds} progressToken={}",
            ctx.id,
            token_text(token.clone())
        ));
        let mut guard = DropLog(format!(
            "slow_count future dropped id={} (before the end)",
            ctx.id
        ));
        let mut counted = 0u64;
        while counted < seconds {
            tokio::select! {
                () = ctx.ct.cancelled() => {
                    log(&format!("slow_count ctx.ct CANCELLED id={} at n={counted}", ctx.id));
                    guard.0 = format!("slow_count future dropped id={} after cancel", ctx.id);
                    return format!("cancelled:{counted}");
                }
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            counted += 1;
            if let Some(token) = token.clone() {
                let param = ProgressNotificationParam::new(token, counted as f64)
                    .with_total(seconds as f64)
                    .with_message(format!("counted {counted}"));
                match ctx.peer.notify_progress(param).await {
                    Ok(()) => log(&format!(
                        "sent notifications/progress id={} n={counted}",
                        ctx.id
                    )),
                    Err(err) => log(&format!(
                        "notifications/progress FAILED id={} n={counted}: {err}",
                        ctx.id
                    )),
                }
            }
        }
        guard.0 = format!("slow_count future dropped id={} after done", ctx.id);
        log(&format!("slow_count done id={} n={counted}", ctx.id));
        format!("done:{counted}")
    }

    #[tool(description = "Returns tick. After the first call the server adds the tool late_tool.")]
    async fn tick(&self) -> String {
        if !self.shared.late_enabled.swap(true, Ordering::SeqCst) {
            log("tick: first call, late_tool enabled, list_changed in 300 ms");
            let shared = self.shared.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                emit_tools_changed(&shared).await;
            });
        }
        "tick".to_owned()
    }

    #[tool(description = "Returns LATE-OK. It exists only after tick was called.")]
    async fn late_tool(&self) -> String {
        "LATE-OK".to_owned()
    }
}

async fn emit_tools_changed(shared: &Shared) {
    let listeners = shared.events.send(Event::ToolsChanged).unwrap_or(0);
    log(&format!(
        "emit tools/list_changed: {listeners} listen stream(s)"
    ));
    let peers: Vec<Peer<RoleServer>> = shared.peers.lock().expect("peers lock").clone();
    for peer in &peers {
        match peer.notify_tool_list_changed().await {
            Ok(()) => log("sent notifications/tools/list_changed on the legacy stream"),
            Err(err) => log(&format!("legacy tools/list_changed FAILED: {err}")),
        }
    }
    if peers.is_empty() {
        log("emit tools/list_changed: no legacy peer");
    }
}

async fn emit_resource_updated(shared: &Shared, uri: &str) {
    let listeners = shared
        .events
        .send(Event::ResourceUpdated(uri.to_owned()))
        .unwrap_or(0);
    log(&format!(
        "emit resources/updated {uri}: {listeners} listen stream(s)"
    ));
    let targets: Vec<Peer<RoleServer>> = shared
        .legacy_subscriptions
        .lock()
        .expect("subscriptions lock")
        .iter()
        .filter(|(_, u)| u == uri)
        .map(|(p, _)| p.clone())
        .collect();
    for peer in &targets {
        match peer
            .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
            .await
        {
            Ok(()) => log("sent notifications/resources/updated on the legacy stream"),
            Err(err) => log(&format!("legacy resources/updated FAILED: {err}")),
        }
    }
    if targets.is_empty() {
        log("emit resources/updated: no legacy subscription");
    }
}

fn is_modern(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

impl EventsServer {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            shared,
        }
    }

    fn visible(&self, name: &str) -> bool {
        name != "late_tool" || self.shared.late_enabled.load(Ordering::SeqCst)
    }
}

impl ServerHandler for EventsServer {
    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_list_changed()
            .enable_resources_subscribe()
            .build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new("events", "0.0.0-spike"))
            .with_instructions("Fleet events test server.")
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        log("notifications/initialized (legacy session ready)");
        self.shared
            .peers
            .lock()
            .expect("peers lock")
            .push(context.peer.clone());
    }

    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        log(&format!(
            "notifications/cancelled SEEN requestId={:?} reason={:?}",
            notification.request_id, notification.reason
        ));
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools: Vec<Tool> = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|tool| self.visible(&tool.name))
            .collect();
        log(&format!(
            "tools/list answered: {:?} (modern={})",
            tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>(),
            is_modern(&context)
        ));
        let mut result = ListToolsResult::with_all_items(tools);
        if is_modern(&context) {
            result = result
                .with_ttl_ms(TTL_MS)
                .with_cache_scope(CacheScope::Private);
        }
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        log(&format!(
            "tools/call name={} id={} progressToken={}",
            request.name,
            context.id,
            token_text(context.meta.get_progress_token())
        ));
        if !self.visible(&request.name) {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", request.name),
                None,
            ));
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if self.visible(name) {
            self.tool_router.get(name).cloned()
        } else {
            None
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        log("resources/list answered");
        let mut result = ListResourcesResult::with_all_items(vec![
            Resource::new(BOARD_URI, "board")
                .with_description("The fleet board.")
                .with_mime_type("text/plain"),
        ]);
        if is_modern(&context) {
            result = result
                .with_ttl_ms(TTL_MS)
                .with_cache_scope(CacheScope::Private);
        }
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != BOARD_URI {
            return Err(ErrorData::resource_not_found(
                format!("unknown resource {}", request.uri),
                None,
            ));
        }
        let n = self.shared.board.load(Ordering::SeqCst);
        log(&format!("resources/read {} answered BOARD-{n}", request.uri));
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(format!("The board word is BOARD-{n}.\n"), request.uri)
                .with_mime_type("text/plain"),
        ])
        .into())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let json = serde_json::to_string(requested).unwrap_or_default();
        if self.shared.listen {
            log(&format!(
                "subscriptions/listen requested filter={json}: accepted"
            ));
            Some(requested.supported_by(&self.get_info().capabilities))
        } else {
            log(&format!(
                "subscriptions/listen requested filter={json}: REFUSED (--no-listen)"
            ));
            None
        }
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let id = context.sink().id().clone();
        let accepted = serde_json::to_string(context.accepted()).unwrap_or_default();
        log(&format!("listen stream OPENED id={id} accepted={accepted}"));
        let mut guard = DropLog(format!("listen stream future dropped id={id}"));
        let mut rx = self.shared.events.subscribe();
        loop {
            tokio::select! {
                () = context.cancelled() => {
                    log(&format!("listen stream CLOSED id={id}: ct cancelled (client closed or cancelled)"));
                    guard.0 = format!("listen stream future dropped id={id} after cancel");
                    return Ok(());
                }
                event = rx.recv() => match event {
                    Ok(Event::ToolsChanged) => {
                        match context.sink().notify_tool_list_changed().await {
                            Ok(()) => log(&format!("sent notifications/tools/list_changed on listen id={id}")),
                            Err(err) => log(&format!("listen id={id} tools/list_changed not sent: {err}")),
                        }
                    }
                    Ok(Event::ResourceUpdated(uri)) => {
                        match context.sink().notify_resource_updated(uri.clone()).await {
                            Ok(()) => log(&format!("sent notifications/resources/updated {uri} on listen id={id}")),
                            Err(err) => log(&format!("listen id={id} resources/updated {uri} not sent: {err}")),
                        }
                    }
                    Err(err) => log(&format!("listen id={id} event channel: {err}")),
                },
            }
        }
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        log(&format!("resources/subscribe (legacy) uri={}", request.uri));
        self.shared
            .legacy_subscriptions
            .lock()
            .expect("subscriptions lock")
            .push((context.peer.clone(), request.uri));
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        log(&format!("resources/unsubscribe (legacy) uri={}", request.uri));
        Ok(())
    }
}

// ---- HTTP layer: logs every request and every frame of a response ----

type Out = BoxBody<Bytes, Infallible>;

/// Wraps a response body: logs each frame (cut) and the end or the drop.
struct LogBody {
    inner: Out,
    tag: String,
    started: Instant,
    frames: u64,
    ended: bool,
}

impl Body for LogBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let polled = Pin::new(&mut self.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                self.frames += 1;
                if let Some(data) = frame.data_ref() {
                    let text = String::from_utf8_lossy(data).replace('\n', "\\n");
                    let cut: String = text.chars().take(400).collect();
                    log(&format!("{} wire frame {}: {cut}", self.tag, self.frames));
                }
            }
            Poll::Ready(None) => {
                self.ended = true;
                log(&format!(
                    "{} response body ENDED by the server after {:.1}s ({} frames)",
                    self.tag,
                    self.started.elapsed().as_secs_f64(),
                    self.frames
                ));
            }
            _ => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for LogBody {
    fn drop(&mut self) {
        if !self.ended {
            log(&format!(
                "{} response body DROPPED before its end after {:.1}s ({} frames): the client closed the HTTP stream",
                self.tag,
                self.started.elapsed().as_secs_f64(),
                self.frames
            ));
        }
    }
}

static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

async fn handle<M: SessionManager>(
    service: StreamableHttpService<EventsServer, M>,
    request: Request<Incoming>,
) -> Result<Response<LogBody>, Infallible> {
    let seq = REQUEST_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let (parts, body) = request.into_parts();
    let bytes = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    let rpc_method = json.get("method").and_then(|v| v.as_str()).unwrap_or("-");
    let rpc_id = json
        .get("id")
        .map_or_else(|| "-".to_owned(), ToString::to_string);
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_owned()
    };
    let meta_keys: Vec<String> = json
        .pointer("/params/_meta")
        .and_then(|m| m.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let progress_token = json
        .pointer("/params/_meta/progressToken")
        .map_or_else(|| "none".to_owned(), ToString::to_string);
    let version_meta = json
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .or_else(|| json.pointer("/params/protocolVersion"))
        .map_or_else(|| "-".to_owned(), ToString::to_string);
    let tag = format!("http#{seq}");
    log(&format!(
        "{tag} {} {} rpc={rpc_method} id={rpc_id} Mcp-Method={} MCP-Protocol-Version={} Mcp-Session-Id={} Accept={} version={version_meta} progressToken={progress_token} meta={meta_keys:?}",
        parts.method,
        parts.uri.path(),
        header("mcp-method"),
        header("mcp-protocol-version"),
        header("mcp-session-id"),
        header("accept"),
    ));
    if matches!(
        rpc_method,
        "notifications/cancelled" | "tools/call" | "subscriptions/listen" | "resources/subscribe"
    ) {
        let cut: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
        log(&format!("{tag} body: {cut}"));
    }
    let request = Request::from_parts(parts, Full::new(bytes));
    let started = Instant::now();
    let response = service.handle(request).await;
    log(&format!(
        "{tag} -> status {} content-type={}",
        response.status(),
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
    ));
    let (head, out) = response.into_parts();
    Ok(Response::from_parts(
        head,
        LogBody {
            inner: out,
            tag,
            started,
            frames: 0,
            ended: false,
        },
    ))
}

async fn serve<M: SessionManager>(
    listener: TcpListener,
    service: StreamableHttpService<EventsServer, M>,
) {
    loop {
        let Ok((tcp, peer)) = listener.accept().await else {
            continue;
        };
        log(&format!("tcp connection accepted from {peer}"));
        let service = service.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
                let service = service.clone();
                async move { handle(service, req).await }
            });
            let result = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tcp), svc)
                .await;
            log(&format!("tcp connection from {peer} closed: {result:?}"));
        });
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let stateful = flag("--stateful");
    let sse = flag("--sse");
    let listen = !flag("--no-listen");
    if stateful && flag("--stateless") {
        return Err("--stateful and --stateless exclude each other".into());
    }
    if sse && flag("--json") {
        return Err("--sse and --json exclude each other".into());
    }

    let (events, _) = broadcast::channel(64);
    let shared = Arc::new(Shared {
        late_enabled: AtomicBool::new(false),
        board: AtomicU64::new(1),
        events,
        peers: Mutex::new(Vec::new()),
        legacy_subscriptions: Mutex::new(Vec::new()),
        listen,
    });

    // The file trigger: touch $EVENTS_DIR/bump.
    if let Ok(dir) = std::env::var("EVENTS_DIR") {
        let shared = shared.clone();
        tokio::spawn(async move {
            let trigger = std::path::Path::new(&dir).join("bump");
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if trigger.exists() {
                    if let Err(err) = std::fs::remove_file(&trigger) {
                        log(&format!("bump: cannot remove the trigger file: {err}"));
                        continue;
                    }
                    let n = shared.board.fetch_add(1, Ordering::SeqCst) + 1;
                    log(&format!("bump: board is now BOARD-{n}"));
                    emit_resource_updated(&shared, BOARD_URI).await;
                }
            }
        });
    }

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    log(&format!(
        "start listening=127.0.0.1:{} stateful={stateful} sse={sse} listen={listen}",
        addr.port()
    ));
    println!("PORT={}", addr.port());

    let factory_shared = shared.clone();
    let factory = move || Ok(EventsServer::new(factory_shared.clone()));
    if stateful {
        // Legacy session mode: sessions, a GET stream; replies are SSE.
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_json_response(false);
        let service = StreamableHttpService::new(
            factory,
            Arc::new(LocalSessionManager::default()),
            config,
        );
        serve(listener, service).await;
    } else {
        // Stateless, as the station: no session state, no GET.
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(!sse);
        let service = StreamableHttpService::new(
            factory,
            Arc::new(NeverSessionManager::default()),
            config,
        );
        serve(listener, service).await;
    }
    Ok(())
}
