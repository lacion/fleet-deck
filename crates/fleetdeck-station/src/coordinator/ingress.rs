//! The loopback HTTP listener of the coordinator endpoint (ADR-006 §5.1–§5.3).
//!
//! Limits apply before authentication and before JSON parsing: open
//! connections, unauthenticated requests in flight, header and body time,
//! and body size. After authentication each binding has its own
//! concurrency and rate limit.

use std::convert::Infallible;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_view::FleetView;
use http::HeaderMap;
use http_body_util::Full;
use hyper::body::{Body, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, WWW_AUTHENTICATE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;

use super::bindings::{Auth, Binding, Bindings, Caller};
use super::desktop_forward::{DesktopRequest, McpToolResult, p6_error};
use super::mcp_service::{McpService, Reply};
use super::permit_io::{PermitIo, PermitSlot};

/// The deck side of a tool call: the station forwards an authorized call
/// as `coordinator.call` or `coordinator.query`. `ask_coordinator` of a
/// binding without a grant goes here too (R2.3).
#[async_trait]
pub trait Forward: Send + Sync {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String>;

    /// A P6 desktop or capture tool (R13 P6 §6.1): one forward to the deck
    /// through the dedicated methods, never the outbox. A forwarder without
    /// them answers `desktop_unsupported`.
    async fn call_desktop(&self, _caller: Caller, _request: DesktopRequest) -> McpToolResult {
        p6_error("desktop_unsupported", None, None)
    }

    /// The fleet view of MCP P3 (`fleet.view`): one page of `view` for
    /// `caller`. The result is `{"items", "next_cursor"}`. A forwarder
    /// without the method answers `unavailable`.
    async fn fleet_view(
        &self,
        _caller: Caller,
        _view: FleetView,
        _cursor: Option<String>,
        _station: Option<StationId>,
    ) -> Result<Value, String> {
        Err(FLEET_VIEW_UNSERVED.to_owned())
    }
}

/// The text of a fleet view that the deck does not serve.
pub const FLEET_VIEW_UNSERVED: &str = "unavailable: the deck does not serve the fleet view yet";

/// The station side of the session's own tools (`publish_artifact`,
/// `set_focus`; DL-165): the station answers them, the deck is not asked.
#[async_trait]
pub trait FleetTools: Send + Sync {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String>;
}

/// The resource limits of §5.1. Tests use shorter times.
#[derive(Debug, Clone)]
pub struct Limits {
    pub connections: usize,
    pub unauthenticated: usize,
    pub headers: Duration,
    pub body: Duration,
    pub body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 128,
            unauthenticated: 16,
            headers: Duration::from_secs(5),
            body: Duration::from_secs(10),
            body_bytes: 256 * 1024,
        }
    }
}

/// What a message path needs: the HTTP listener and the bridge socket
/// (DL-194) build one each and answer through the same code.
pub(super) struct Shared {
    port: u16,
    pub(super) bindings: Bindings,
    mcp: McpService,
    pub(super) unauth: Arc<Semaphore>,
    pub(super) limits: Limits,
}

impl Shared {
    pub(super) fn new(
        port: u16,
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
        limits: Limits,
    ) -> Self {
        Self {
            port,
            mcp: McpService::new(bindings.clone(), forward, fleet),
            bindings,
            unauth: Arc::new(Semaphore::new(limits.unauthenticated)),
            limits,
        }
    }
}

/// The result of the shared message path (DL-194): the HTTP listener and
/// the bridge socket each map it to their own reply.
pub(super) enum MessageOutcome {
    /// Missing, malformed, unknown or ended credential.
    Unauthorized,
    /// The binding is over its rate or concurrency limit.
    Limited,
    /// The binding is held until the replay of its shim finished (DL-194
    /// §9.7).
    Starting,
    /// The body is not JSON.
    ParseError,
    /// The body is a JSON-RPC batch, which the endpoint does not take.
    Batch,
    /// The reply of the MCP service.
    Reply(Reply),
}

/// Checks the credential of a request (`Bearer <token>`). The binding
/// permit lives as long as the call.
pub(super) fn authenticate_message(
    shared: &Shared,
    auth: Option<&str>,
) -> Result<(Box<Binding>, OwnedSemaphorePermit), MessageOutcome> {
    match shared.bindings.authenticate(auth) {
        Auth::Unauthorized => Err(MessageOutcome::Unauthorized),
        Auth::Limited => Err(MessageOutcome::Limited),
        Auth::Starting => Err(MessageOutcome::Starting),
        Auth::Ok { binding, permit } => Ok((binding, permit)),
    }
}

/// Checks and answers the body of an authenticated request. The body must
/// be JSON and must not be a batch; both are refused before `rmcp` sees it.
/// `headers` builds the request headers from the parsed message (the socket
/// synthesizes them; HTTP passes the ones it received).
pub(super) async fn answer_message(
    shared: &Shared,
    binding: &Binding,
    body: Vec<u8>,
    headers: impl FnOnce(&Value) -> HeaderMap,
) -> MessageOutcome {
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return MessageOutcome::ParseError;
    };
    if msg.is_array() {
        return MessageOutcome::Batch;
    }
    let headers = headers(&msg);
    drop(msg);
    MessageOutcome::Reply(
        shared
            .mcp
            .serve(binding.clone(), Bytes::from(body), &headers)
            .await,
    )
}

/// A running endpoint.
pub struct Ingress {
    pub addr: SocketAddr,
    stop: watch::Sender<bool>,
}

impl Ingress {
    /// Bind `127.0.0.1:0` and serve until [`Ingress::shutdown`] or drop.
    pub async fn start(
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        limits: Limits,
    ) -> std::io::Result<Self> {
        Self::start_with(bindings, forward, None, limits).await
    }

    /// [`Ingress::start`] with the station's own tools for every session
    /// (DL-165). Without them a call of such a tool is an error.
    pub async fn start_with(
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
        limits: Limits,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared::new(addr.port(), bindings, forward, fleet, limits));
        let (stop, rx) = watch::channel(false);
        tokio::spawn(serve(listener, shared, rx));
        Ok(Self { addr, stop })
    }

    /// The MCP URL that the provider configuration names.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.addr.port())
    }

    pub fn shutdown(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for Ingress {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn serve(listener: TcpListener, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let conns = Arc::new(Semaphore::new(shared.limits.connections));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((tcp, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                // Above the limit: close at once, no waiting task.
                let Ok(permit) = conns.clone().try_acquire_owned() else {
                    drop(tcp);
                    continue;
                };
                let shared = shared.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    // The binding permit of a reply lives in this slot until
                    // the connection flushed it (`permit_io.rs`).
                    let slot = PermitSlot::default();
                    let svc_shared = shared.clone();
                    let svc_slot = slot.clone();
                    let service = hyper::service::service_fn(move |req| {
                        let shared = svc_shared.clone();
                        let slot = svc_slot.clone();
                        async move { Ok::<_, Infallible>(handle(req, &shared, &slot).await) }
                    });
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(shared.limits.headers);
                    let io = PermitIo::new(TokioIo::new(tcp), slot);
                    let _ = builder.serve_connection(io, service).await;
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            _ = stop.changed() => break,
        }
    }
    tasks.abort_all();
}

fn plain(status: StatusCode) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::new()));
    *r.status_mut() = status;
    r
}

fn json(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body.to_string())));
    *r.status_mut() = status;
    r.headers_mut().insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    r
}

/// The answer while a reattached shim replays its buffered output (DL-194
/// §9.7).
fn starting() -> Response<Full<Bytes>> {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        &serde_json::json!({"jsonrpc": "2.0", "id": null,
            "error": {"code": -32000, "message": "Fleet Deck is starting; try again in a moment."}}),
    )
}

fn unauthorized() -> Response<Full<Bytes>> {
    let mut r = json(
        StatusCode::UNAUTHORIZED,
        &serde_json::json!({"error": "unauthorized"}),
    );
    r.headers_mut().insert(
        WWW_AUTHENTICATE,
        hyper::header::HeaderValue::from_static("Bearer"),
    );
    r
}

/// A connection-closing answer for a body that did not arrive in time.
fn closing(status: StatusCode) -> Response<Full<Bytes>> {
    let mut r = plain(status);
    r.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("close"),
    );
    r
}

/// The HTTP response of an `rmcp` reply: its status, content type and body.
fn from_reply(reply: Reply) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(reply.body));
    *r.status_mut() = reply.status;
    if let Some(content_type) = reply.content_type {
        r.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    r
}

async fn handle(
    req: Request<Incoming>,
    shared: &Shared,
    slot: &PermitSlot,
) -> Response<Full<Bytes>> {
    if req.uri().path() != "/mcp" {
        return plain(StatusCode::NOT_FOUND);
    }
    if req.method() != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED);
    }
    // §5.2: checks before authentication.
    let host = req.headers().get(HOST).and_then(|h| h.to_str().ok());
    let port = shared.port;
    let host_ok =
        host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok || req.headers().contains_key(ORIGIN) {
        return plain(StatusCode::FORBIDDEN);
    }
    let content_type = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok());
    let json_type = content_type
        .is_some_and(|c| c.split(';').next().map(str::trim) == Some("application/json"));
    if !json_type {
        return plain(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let declared = req
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<usize>().ok());
    if declared.is_some_and(|n| n > shared.limits.body_bytes) {
        return closing(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let Ok(_unauth) = shared.unauth.clone().try_acquire_owned() else {
        return plain(StatusCode::TOO_MANY_REQUESTS);
    };
    let auth = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let headers = req.headers().clone();
    let body = match tokio::time::timeout(
        shared.limits.body,
        read_body(req.into_body(), shared.limits.body_bytes),
    )
    .await
    {
        Err(_) => return closing(StatusCode::REQUEST_TIMEOUT),
        Ok(Err(status)) => return closing(status),
        Ok(Ok(body)) => body,
    };
    let (binding, permit) = match authenticate_message(shared, auth.as_deref()) {
        Ok(ok) => ok,
        Err(MessageOutcome::Limited) => return plain(StatusCode::TOO_MANY_REQUESTS),
        Err(MessageOutcome::Starting) => return starting(),
        Err(_) => return unauthorized(),
    };
    drop(_unauth);
    let outcome = answer_message(shared, &binding, body, move |_| headers).await;
    // The permit lives until the connection flushed the reply (a slow reader
    // keeps it); a dropped request (the client left) drops it with this
    // frame.
    slot.hold(permit);
    match outcome {
        MessageOutcome::ParseError => json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"}}),
        ),
        MessageOutcome::Batch => plain(StatusCode::BAD_REQUEST),
        MessageOutcome::Reply(reply) => from_reply(reply),
        // The authentication step returns these; the answer step never does.
        MessageOutcome::Unauthorized | MessageOutcome::Limited | MessageOutcome::Starting => {
            unauthorized()
        }
    }
}

async fn read_body(mut body: Incoming, limit: usize) -> Result<Vec<u8>, StatusCode> {
    let mut out = Vec::new();
    while let Some(frame) = poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Ok(data) = frame.into_data() {
            if out.len() + data.len() > limit {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            out.extend_from_slice(&data);
        }
    }
    Ok(out)
}
