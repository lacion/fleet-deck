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
use hyper::body::{Body, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, WWW_AUTHENTICATE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use super::bindings::{Auth, Bindings, Caller};
use super::jsonrpc::{self, Answer};

/// The deck side of a tool call: the station forwards an authorized call
/// as `coordinator.call` or `coordinator.query`.
#[async_trait]
pub trait Forward: Send + Sync {
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

struct Shared {
    port: u16,
    bindings: Bindings,
    forward: Arc<dyn Forward>,
    unauth: Arc<Semaphore>,
    limits: Limits,
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
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            port: addr.port(),
            bindings,
            forward,
            unauth: Arc::new(Semaphore::new(limits.unauthenticated)),
            limits,
        });
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
                    let svc_shared = shared.clone();
                    let service = hyper::service::service_fn(move |req| {
                        let shared = svc_shared.clone();
                        async move { Ok::<_, Infallible>(handle(req, &shared).await) }
                    });
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(shared.limits.headers);
                    let _ = builder.serve_connection(TokioIo::new(tcp), service).await;
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            _ = stop.changed() => break,
        }
    }
    tasks.abort_all();
}

fn plain(status: StatusCode) -> Response<String> {
    let mut r = Response::new(String::new());
    *r.status_mut() = status;
    r
}

fn json(status: StatusCode, body: &Value) -> Response<String> {
    let mut r = Response::new(body.to_string());
    *r.status_mut() = status;
    r.headers_mut().insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    r
}

fn unauthorized() -> Response<String> {
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
fn closing(status: StatusCode) -> Response<String> {
    let mut r = plain(status);
    r.headers_mut().insert(
        hyper::header::CONNECTION,
        hyper::header::HeaderValue::from_static("close"),
    );
    r
}

async fn handle(req: Request<Incoming>, shared: &Shared) -> Response<String> {
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
    let (binding, _permit) = match shared.bindings.authenticate(auth.as_deref()) {
        Auth::Unauthorized => return unauthorized(),
        Auth::Limited => return plain(StatusCode::TOO_MANY_REQUESTS),
        Auth::Ok { binding, permit } => (binding, permit),
    };
    drop(_unauth);
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"}}),
        );
    };
    if msg.is_array() {
        return plain(StatusCode::BAD_REQUEST);
    }
    match jsonrpc::answer(msg, &binding, shared.forward.as_ref()).await {
        Answer::Accepted => plain(StatusCode::ACCEPTED),
        Answer::Json(v) => json(StatusCode::OK, &v),
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
