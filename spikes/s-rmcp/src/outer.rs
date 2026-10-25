//! The outer boundary: the checks of `ingress.rs` `handle` run here, before
//! `rmcp` sees the request.

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Display;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN, WWW_AUTHENTICATE};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

use crate::{Binding, Probe, SpikeServer};

pub type OuterBody = BoxBody<Bytes, Infallible>;

/// A response body that holds the binding permit until the body is dropped,
/// that is after the last byte was sent (or the connection ended).
struct PermitBody {
    inner: OuterBody,
    _permit: OwnedSemaphorePermit,
}

impl Body for PermitBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

struct Inner {
    port: u16,
    bindings: HashMap<String, Binding>,
    permits: HashMap<String, Arc<Semaphore>>,
    service: StreamableHttpService<SpikeServer, LocalSessionManager>,
}

/// The outer service: the same code answers a TCP request and an in-process
/// bridge call.
#[derive(Clone)]
pub struct Outer {
    inner: Arc<Inner>,
}

fn plain(status: StatusCode) -> Response<OuterBody> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = status;
    response
}

fn unauthorized() -> Response<OuterBody> {
    let mut response =
        Response::new(Full::new(Bytes::from_static(br#"{"error":"unauthorized"}"#)).boxed());
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
        .headers_mut()
        .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

impl Outer {
    #[must_use]
    pub fn new(port: u16, bindings: HashMap<String, Binding>, probe: Arc<Probe>) -> Self {
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true);
        Self::with_config(port, bindings, probe, config)
    }

    /// [`Outer::new`] with an explicit `rmcp` config (for the comparison
    /// tests).
    #[must_use]
    pub fn with_config(
        port: u16,
        bindings: HashMap<String, Binding>,
        probe: Arc<Probe>,
        config: StreamableHttpServerConfig,
    ) -> Self {
        let permits = bindings
            .keys()
            .map(|token| (token.clone(), Arc::new(Semaphore::new(1))))
            .collect();
        let service = StreamableHttpService::new(
            move || Ok(SpikeServer::new(probe.clone())),
            LocalSessionManager::default().into(),
            config,
        );
        Self {
            inner: Arc::new(Inner {
                port,
                bindings,
                permits,
                service,
            }),
        }
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// The checks of `ingress.rs` `handle`, in the same order: path, method,
    /// Host and Origin, Content-Type, then the bearer token and the binding
    /// permit. The body size, the body time and the unauthenticated permit of
    /// the station are not part of the spike.
    pub async fn handle<B>(&self, request: Request<B>) -> Response<OuterBody>
    where
        B: Body + Send + 'static,
        B::Error: Display,
    {
        if request.uri().path() != "/mcp" {
            return plain(StatusCode::NOT_FOUND);
        }
        if request.method() != Method::POST {
            return plain(StatusCode::METHOD_NOT_ALLOWED);
        }
        let port = self.inner.port;
        let host_ok = request
            .headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
        if !host_ok || request.headers().contains_key(ORIGIN) {
            return plain(StatusCode::FORBIDDEN);
        }
        let json_type = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|c| c.split(';').next().map(str::trim) == Some("application/json"));
        if !json_type {
            return plain(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        let token = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(str::to_owned);
        let Some((token, binding)) = token.and_then(|token| {
            let binding = self.inner.bindings.get(&token)?.clone();
            Some((token, binding))
        }) else {
            return unauthorized();
        };
        let Some(semaphore) = self.inner.permits.get(&token) else {
            return unauthorized();
        };
        let Ok(permit) = semaphore.clone().try_acquire_owned() else {
            return plain(StatusCode::TOO_MANY_REQUESTS);
        };
        let (mut parts, body) = request.into_parts();
        parts.extensions.insert(binding);
        let response = self
            .inner
            .service
            .handle(Request::from_parts(parts, body))
            .await;
        let (head, body) = response.into_parts();
        Response::from_parts(
            head,
            PermitBody {
                inner: body,
                _permit: permit,
            }
            .boxed(),
        )
    }
}

/// A running spike server.
pub struct Running {
    pub addr: SocketAddr,
    pub outer: Outer,
    pub probe: Arc<Probe>,
    accept: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// Serves `outer` on `listener` with `hyper` (HTTP/1) until the result drops.
pub async fn serve(listener: TcpListener, bindings: HashMap<String, Binding>) -> Running {
    let addr = listener.local_addr().expect("listener address");
    let probe = Arc::new(Probe::default());
    let outer = Outer::new(addr.port(), bindings, probe.clone());
    let accept = tokio::spawn({
        let outer = outer.clone();
        async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let outer = outer.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                        let outer = outer.clone();
                        async move { Ok::<_, Infallible>(outer.handle(req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        }
    });
    Running {
        addr,
        outer,
        probe,
        accept,
    }
}

/// The in-process form of the bridge: one JSON-RPC line in, one line out
/// (`None` for 202). The `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name`
/// headers are synthesized from a modern request (one with the per-request
/// protocol version in `_meta`).
pub async fn bridge_call(outer: &Outer, token: &str, line: &[u8]) -> Option<Vec<u8>> {
    let message: Value = serde_json::from_slice(line).unwrap_or(Value::Null);
    let port = outer.port();
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(HOST, format!("127.0.0.1:{port}"))
        .header(CONTENT_TYPE, "application/json")
        .header(http::header::ACCEPT, "application/json, text/event-stream")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let version = message
        .pointer("/params/_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str);
    if let (Some(version), Some(method)) = (version, message.get("method").and_then(Value::as_str))
    {
        builder = builder
            .header("MCP-Protocol-Version", version)
            .header("Mcp-Method", method);
        let key = match method {
            "tools/call" | "prompts/get" => Some("name"),
            "resources/read" | "resources/subscribe" | "resources/unsubscribe" => Some("uri"),
            "tasks/get" | "tasks/update" | "tasks/cancel" => Some("taskId"),
            _ => None,
        };
        if let Some(name) = key
            .and_then(|key| message.pointer(&format!("/params/{key}")))
            .and_then(Value::as_str)
        {
            builder = builder.header("Mcp-Name", name);
        }
    }
    let request = builder
        .body(Full::new(Bytes::copy_from_slice(line)))
        .expect("valid request");
    let response = outer.handle(request).await;
    let status = response.status();
    let body = response.into_body().collect().await.ok()?.to_bytes();
    if status == StatusCode::ACCEPTED {
        return None;
    }
    Some(body.to_vec())
}
