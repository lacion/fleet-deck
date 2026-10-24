//! The spike tests T1 to T10. Run with `--nocapture` to see the wire bytes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use s_rmcp::outer::{Outer, Running, bridge_call, serve};
use s_rmcp::{Binding, Probe};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN_A: &str = "token-a";
const TOKEN_B: &str = "token-b";
const TOKEN_SLOW: &str = "token-slow";
const LEGACY: &str = "2025-11-25";
const MODERN: &str = "2026-07-28";

fn bindings() -> HashMap<String, Binding> {
    let mut map = HashMap::new();
    let mut add = |token: &str, session: &str, classes: Vec<&'static str>| {
        map.insert(
            token.to_owned(),
            Binding {
                session_id: session.to_owned(),
                classes,
            },
        );
    };
    add(TOKEN_A, "session-a", vec![]);
    add(TOKEN_B, "session-b", vec!["mail"]);
    add(TOKEN_SLOW, "session-slow", vec!["slow"]);
    map
}

async fn start() -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    serve(listener, bindings()).await
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "not JSON ({error}): status {} body {:?}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The default headers of a good request; `extra` replaces or adds headers.
fn build_request(
    addr: SocketAddr,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: &str,
) -> Request<Full<Bytes>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "host",
        HeaderValue::from_str(&addr.to_string()).expect("host"),
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(token) = token {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).expect("token"),
        );
    }
    for (name, value) in extra {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("name"),
            HeaderValue::from_str(value).expect("value"),
        );
    }
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .expect("request");
    *request.headers_mut() = headers;
    request
}

async fn connect(
    addr: SocketAddr,
) -> (
    hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    tokio::task::JoinHandle<()>,
) {
    let tcp = TcpStream::connect(addr).await.expect("connect");
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .expect("handshake");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (sender, task)
}

async fn post(addr: SocketAddr, token: Option<&str>, extra: &[(&str, &str)], body: &str) -> Reply {
    let (mut sender, task) = connect(addr).await;
    let response = sender
        .send_request(build_request(addr, token, extra, body))
        .await
        .expect("send");
    let (head, body) = response.into_parts();
    let body = body.collect().await.expect("body").to_bytes();
    task.abort();
    Reply {
        status: head.status,
        headers: head.headers,
        body,
    }
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn meta_with_version(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn modern_body(id: u32, method: &str, params: Value) -> String {
    let mut params = params;
    params["_meta"] = meta();
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn modern_headers(
    method: &'static str,
    name: Option<&'static str>,
) -> Vec<(&'static str, &'static str)> {
    let mut headers = vec![("mcp-protocol-version", MODERN), ("mcp-method", method)];
    if let Some(name) = name {
        headers.push(("mcp-name", name));
    }
    headers
}

fn legacy_body(id: u32, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn tool_names(reply: &Reply) -> Vec<String> {
    reply.json()["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_owned())
        .collect()
}

#[tokio::test]
async fn legacy_initialize_then_tools_list_and_call() {
    let server = start().await;
    let addr = server.addr;
    let initialize = legacy_body(
        1,
        "initialize",
        json!({
            "protocolVersion": LEGACY,
            "capabilities": {},
            "clientInfo": {"name": "spike-client", "version": "0"},
        }),
    );
    let reply = post(addr, Some(TOKEN_A), &[], &initialize).await;
    println!("T1 initialize: {} {}", reply.status, reply.text());
    println!("T1 initialize headers: {:?}", reply.headers);
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.json()["result"];
    assert_eq!(result["protocolVersion"], LEGACY);
    assert_eq!(result["serverInfo"]["name"], "fleetdeck");
    assert_eq!(result["serverInfo"]["version"], "0.0.0-spike");
    println!(
        "T1 Mcp-Session-Id header on initialize: {:?}",
        reply.headers.get("mcp-session-id")
    );

    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    let reply = post(
        addr,
        Some(TOKEN_A),
        &[("mcp-protocol-version", LEGACY)],
        &initialized,
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);

    let list = legacy_body(2, "tools/list", json!({}));
    let reply = post(
        addr,
        Some(TOKEN_A),
        &[("mcp-protocol-version", LEGACY)],
        &list,
    )
    .await;
    println!("T1 tools/list: {} {}", reply.status, reply.text());
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(tool_names(&reply), ["list_things", "whoami"]);

    let call = legacy_body(3, "tools/call", json!({"name": "whoami", "arguments": {}}));
    let reply = post(
        addr,
        Some(TOKEN_A),
        &[("mcp-protocol-version", LEGACY)],
        &call,
    )
    .await;
    println!("T1 tools/call whoami: {} {}", reply.status, reply.text());
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["result"]["content"][0]["text"], "session-a");
}

#[tokio::test]
async fn modern_discover_then_stateless_call() {
    let server = start().await;
    let addr = server.addr;
    let discover = modern_body(1, "server/discover", json!({}));
    let reply = post(
        addr,
        Some(TOKEN_A),
        &modern_headers("server/discover", None),
        &discover,
    )
    .await;
    println!("T2 request  server/discover: {discover}");
    println!(
        "T2 response server/discover: {} {}",
        reply.status,
        reply.text()
    );
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.json()["result"];
    assert_eq!(result["resultType"], "complete");
    assert!(
        result["supportedVersions"]
            .as_array()
            .expect("supportedVersions")
            .iter()
            .any(|v| v == MODERN)
    );

    let call = modern_body(
        2,
        "tools/call",
        json!({"name": "list_things", "arguments": {}}),
    );
    let reply = post(
        addr,
        Some(TOKEN_A),
        &modern_headers("tools/call", Some("list_things")),
        &call,
    )
    .await;
    println!("T2 request  tools/call: {call}");
    println!("T2 response tools/call: {} {}", reply.status, reply.text());
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.json()["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(
        result["structuredContent"],
        json!({"things": ["alpha", "beta"]})
    );
    let text: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().expect("text copy"))
            .expect("text copy is JSON");
    assert_eq!(text, result["structuredContent"]);
}

#[tokio::test]
async fn unsupported_version_gets_32022_with_supported() {
    let server = start().await;
    let addr = server.addr;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": {"_meta": meta_with_version("2099-01-01")},
    })
    .to_string();
    let reply = post(
        addr,
        Some(TOKEN_A),
        &[
            ("mcp-protocol-version", "2099-01-01"),
            ("mcp-method", "tools/list"),
        ],
        &body,
    )
    .await;
    println!("T3 request : {body}");
    println!("T3 response: {} {}", reply.status, reply.text());
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let error = &reply.json()["error"];
    assert_eq!(error["code"], -32022);
    assert!(error["data"]["supported"].is_array(), "supported list");

    // Report, do not assert: a legacy initialize with an unknown version.
    let initialize = legacy_body(
        2,
        "initialize",
        json!({
            "protocolVersion": "2099-01-01",
            "capabilities": {},
            "clientInfo": {"name": "spike-client", "version": "0"},
        }),
    );
    let reply = post(addr, Some(TOKEN_A), &[], &initialize).await;
    println!(
        "T3 initialize 2099-01-01 (no header): {} {}",
        reply.status,
        reply.text()
    );
    let reply = post(
        addr,
        Some(TOKEN_A),
        &[("mcp-protocol-version", "2099-01-01")],
        &initialize,
    )
    .await;
    println!(
        "T3 initialize 2099-01-01 (with header): {} {}",
        reply.status,
        reply.text()
    );
}

#[tokio::test]
async fn header_mismatch_gets_32020() {
    let server = start().await;
    let body = modern_body(1, "tools/call", json!({"name": "whoami", "arguments": {}}));
    let reply = post(
        server.addr,
        Some(TOKEN_A),
        &modern_headers("tools/call", Some("poke")),
        &body,
    )
    .await;
    println!("T4 request : {body}");
    println!("T4 response: {} {}", reply.status, reply.text());
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json()["error"]["code"], -32020);
}

#[tokio::test]
async fn outer_checks_run_before_rmcp() {
    let server = start().await;
    let addr = server.addr;
    let body = legacy_body(1, "tools/list", json!({}));

    let reply = post(addr, None, &[], &body).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.text(), r#"{"error":"unauthorized"}"#);
    assert_eq!(reply.headers["www-authenticate"], "Bearer");

    let reply = post(addr, Some("unknown"), &[], &body).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.text(), r#"{"error":"unauthorized"}"#);
    assert_eq!(reply.headers["www-authenticate"], "Bearer");

    let reply = post(addr, Some(TOKEN_A), &[("origin", "http://evil")], &body).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let reply = post(addr, Some(TOKEN_A), &[("host", "evil:1")], &body).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let reply = post(
        addr,
        Some(TOKEN_A),
        &[("content-type", "text/plain")],
        &body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // The order: a bad Host is answered before a bad bearer.
    let reply = post(addr, None, &[("host", "evil:1")], &body).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_catalog_depends_on_the_binding() {
    let server = start().await;
    let addr = server.addr;
    let body = legacy_body(1, "tools/list", json!({}));
    let modern = modern_body(1, "tools/list", json!({}));
    for _ in 0..2 {
        let a = post(addr, Some(TOKEN_A), &[], &body).await;
        assert_eq!(tool_names(&a), ["list_things", "whoami"]);
        let b = post(addr, Some(TOKEN_B), &[], &body).await;
        assert_eq!(tool_names(&b), ["list_things", "poke", "whoami"]);
        let a = post(
            addr,
            Some(TOKEN_A),
            &modern_headers("tools/list", None),
            &modern,
        )
        .await;
        assert_eq!(tool_names(&a), ["list_things", "whoami"]);
        let b = post(
            addr,
            Some(TOKEN_B),
            &modern_headers("tools/list", None),
            &modern,
        )
        .await;
        assert_eq!(tool_names(&b), ["list_things", "poke", "whoami"]);
        println!(
            "T6 modern tools/list result keys (B): ttlMs={} cacheScope={}",
            b.json()["result"]["ttlMs"],
            b.json()["result"]["cacheScope"]
        );
    }
    // The order is the order of `ToolRouter::list_all`: sorted by name.
}

#[tokio::test]
async fn annotations_and_output_schema_on_the_wire() {
    let server = start().await;
    let addr = server.addr;
    let modern = modern_body(1, "tools/list", json!({}));
    let reply = post(
        addr,
        Some(TOKEN_B),
        &modern_headers("tools/list", None),
        &modern,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let tools = reply.json()["result"]["tools"].clone();
    let by_name = |name: &str| {
        tools
            .as_array()
            .expect("tools")
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tool {name}"))
            .clone()
    };
    let list_things = by_name("list_things");
    assert_eq!(list_things["annotations"]["readOnlyHint"], true);
    assert_eq!(list_things["annotations"]["openWorldHint"], false);
    assert!(list_things["outputSchema"].is_object(), "outputSchema");
    let poke = by_name("poke");
    assert_eq!(poke["annotations"]["destructiveHint"], false);
    assert_eq!(poke["annotations"]["idempotentHint"], true);
    println!(
        "T7 serialized tools/list (modern, token B): {} bytes",
        reply.body.len()
    );
    println!("T7 body: {}", reply.text());
    let reply_a = post(
        addr,
        Some(TOKEN_A),
        &modern_headers("tools/list", None),
        &modern,
    )
    .await;
    println!(
        "T7 serialized tools/list (modern, token A): {} bytes",
        reply_a.body.len()
    );
    let legacy = legacy_body(1, "tools/list", json!({}));
    let reply_l = post(addr, Some(TOKEN_B), &[], &legacy).await;
    println!(
        "T7 serialized tools/list (legacy, token B): {} bytes",
        reply_l.body.len()
    );
}

#[tokio::test]
async fn in_process_bridge_call() {
    let server = start().await;
    let legacy = legacy_body(1, "tools/list", json!({}));
    let answer = bridge_call(&server.outer, TOKEN_B, legacy.as_bytes())
        .await
        .expect("an answer");
    println!("T8 legacy answer: {}", String::from_utf8_lossy(&answer));
    assert!(!answer.contains(&b'\n'));
    let value: Value = serde_json::from_slice(&answer).expect("JSON");
    assert_eq!(value["id"], 1);
    assert_eq!(value["result"]["tools"].as_array().expect("tools").len(), 3);

    let modern = modern_body(
        2,
        "tools/call",
        json!({"name": "list_things", "arguments": {}}),
    );
    let answer = bridge_call(&server.outer, TOKEN_A, modern.as_bytes())
        .await
        .expect("an answer");
    println!("T8 modern answer: {}", String::from_utf8_lossy(&answer));
    let value: Value = serde_json::from_slice(&answer).expect("JSON");
    assert_eq!(value["result"]["resultType"], "complete");
    assert_eq!(
        value["result"]["structuredContent"],
        json!({"things": ["alpha", "beta"]})
    );

    let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    assert!(
        bridge_call(&server.outer, TOKEN_A, notification.as_bytes())
            .await
            .is_none()
    );

    let answer = bridge_call(&server.outer, "unknown", legacy.as_bytes())
        .await
        .expect("an answer");
    println!(
        "T8 unknown token answer: {}",
        String::from_utf8_lossy(&answer)
    );
}

fn in_process(outer: &Outer, token: &str, body: &str) -> Request<Full<Bytes>> {
    let addr: SocketAddr = ([127, 0, 0, 1], outer.port()).into();
    build_request(addr, Some(token), &[], body)
}

#[tokio::test]
async fn the_permit_lives_until_the_body_ends() {
    // In process, without TCP: the test holds the first response body
    // unread. On TCP, `hyper` writes a one-frame JSON body into the socket
    // buffer at once and drops the body (see the raw TCP check below).
    let outer = Outer::new(7, bindings(), Arc::new(Probe::default()));
    let body = legacy_body(1, "tools/list", json!({}));

    let first = outer.handle(in_process(&outer, TOKEN_A, &body)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = outer.handle(in_process(&outer, TOKEN_A, &body)).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    // Another binding has its own permit.
    let other = outer.handle(in_process(&outer, TOKEN_B, &body)).await;
    assert_eq!(other.status(), StatusCode::OK);

    let bytes = first.into_body().collect().await.expect("body").to_bytes();
    assert!(!bytes.is_empty());
    let third = outer.handle(in_process(&outer, TOKEN_A, &body)).await;
    assert_eq!(third.status(), StatusCode::OK);
    drop(third);
    drop(other);
    let fourth = outer.handle(in_process(&outer, TOKEN_A, &body)).await;
    assert_eq!(fourth.status(), StatusCode::OK);
}

/// Report only: on a real socket, with a response that the client does not
/// read, is the permit still held?
#[tokio::test]
async fn the_permit_on_tcp_with_an_unread_response() {
    let server = start().await;
    let addr = server.addr;
    let body = legacy_body(1, "tools/list", json!({}));
    let raw = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nAuthorization: Bearer {TOKEN_A}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut tcp = TcpStream::connect(addr).await.expect("connect");
    tcp.write_all(raw.as_bytes()).await.expect("write");
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The first response sits unread in the socket buffer of `tcp`.
    let second = post(addr, Some(TOKEN_A), &[], &body).await;
    println!(
        "T9 TCP, first response unread, second request: {}",
        second.status
    );
    let mut buffer = vec![0u8; 64];
    let read = tcp.read(&mut buffer).await.expect("read");
    println!("T9 TCP, first response now read: {read} bytes");
}

/// Report only: edge cases of the modern era and the legacy session mode.
#[tokio::test]
async fn probe_edge_cases() {
    let server = start().await;
    let addr = server.addr;
    let show = |label: &str, reply: &Reply| {
        println!("PROBE {label}: {} {}", reply.status, reply.text());
    };

    let body = modern_body(1, "tools/list", json!({}));
    show(
        "modern body, no MCP headers",
        &post(addr, Some(TOKEN_A), &[], &body).await,
    );
    show(
        "modern body, header version only (no Mcp-Method)",
        &post(
            addr,
            Some(TOKEN_A),
            &[("mcp-protocol-version", MODERN)],
            &body,
        )
        .await,
    );
    let no_meta = legacy_body(1, "tools/list", json!({}));
    show(
        "modern header, no _meta",
        &post(
            addr,
            Some(TOKEN_A),
            &modern_headers("tools/list", None),
            &no_meta,
        )
        .await,
    );
    let only_version = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN}},
    })
    .to_string();
    show(
        "modern, no clientCapabilities in _meta",
        &post(
            addr,
            Some(TOKEN_A),
            &modern_headers("tools/list", None),
            &only_version,
        )
        .await,
    );
    show(
        "header 2025-11-25 but _meta 2026-07-28",
        &post(
            addr,
            Some(TOKEN_A),
            &[
                ("mcp-protocol-version", LEGACY),
                ("mcp-method", "tools/list"),
            ],
            &body,
        )
        .await,
    );
    let unknown = modern_body(1, "no/such", json!({}));
    show(
        "modern unknown method",
        &post(
            addr,
            Some(TOKEN_A),
            &modern_headers("no/such", None),
            &unknown,
        )
        .await,
    );
    let poke = modern_body(1, "tools/call", json!({"name": "poke", "arguments": {}}));
    show(
        "poke called by token A (class mail missing)",
        &post(
            addr,
            Some(TOKEN_A),
            &modern_headers("tools/call", Some("poke")),
            &poke,
        )
        .await,
    );
    let nothing = modern_body(1, "tools/call", json!({"name": "nothing", "arguments": {}}));
    show(
        "unknown tool",
        &post(
            addr,
            Some(TOKEN_A),
            &modern_headers("tools/call", Some("nothing")),
            &nothing,
        )
        .await,
    );
    show(
        "no Accept header",
        &post(
            addr,
            Some(TOKEN_A),
            &[("accept", "application/json")],
            &no_meta,
        )
        .await,
    );
    let batch = format!("[{no_meta}]");
    show("batch", &post(addr, Some(TOKEN_A), &[], &batch).await);
    show("not JSON", &post(addr, Some(TOKEN_A), &[], "{").await);
    let ping = legacy_body(1, "ping", json!({}));
    show(
        "legacy ping without initialize",
        &post(addr, Some(TOKEN_A), &[], &ping).await,
    );

    // The legacy session mode (the `rmcp` default), in process.
    let config = StreamableHttpServerConfig::default().with_json_response(true);
    let outer = Outer::with_config(9, bindings(), Arc::new(Probe::default()), config);
    let initialize = legacy_body(
        1,
        "initialize",
        json!({
            "protocolVersion": LEGACY,
            "capabilities": {},
            "clientInfo": {"name": "spike-client", "version": "0"},
        }),
    );
    let response = outer.handle(in_process(&outer, TOKEN_A, &initialize)).await;
    let (head, body) = response.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    println!(
        "PROBE legacy_session_mode=true initialize: {} {:?} {}",
        head.status,
        head.headers,
        String::from_utf8_lossy(&bytes)
    );
    let list = legacy_body(2, "tools/list", json!({}));
    let response = outer.handle(in_process(&outer, TOKEN_A, &list)).await;
    let (head, body) = response.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes();
    println!(
        "PROBE legacy_session_mode=true tools/list without a session: {} {}",
        head.status,
        String::from_utf8_lossy(&bytes)
    );
}

#[tokio::test]
async fn cancellation_drops_the_handler() {
    let server = start().await;
    let addr = server.addr;
    let body = modern_body(1, "tools/call", json!({"name": "slow", "arguments": {}}));
    let (mut sender, connection) = connect(addr).await;
    let request = build_request(
        addr,
        Some(TOKEN_SLOW),
        &modern_headers("tools/call", Some("slow")),
        &body,
    );
    let call = tokio::spawn(async move { sender.send_request(request).await.map(|_| ()) });
    tokio::time::timeout(Duration::from_secs(5), server.probe.started.notified())
        .await
        .expect("the handler started");
    // Close the client connection in the middle of the call.
    call.abort();
    connection.abort();
    let started = tokio::time::Instant::now();
    let ct =
        tokio::time::timeout(Duration::from_secs(2), server.probe.ct_cancelled.notified()).await;
    println!(
        "T10 ct cancelled within 2 s: {} (after {:?})",
        ct.is_ok(),
        started.elapsed()
    );
    let dropped =
        tokio::time::timeout(Duration::from_secs(2), server.probe.dropped.notified()).await;
    println!("T10 handler future dropped within 2 s: {}", dropped.is_ok());
    assert!(
        ct.is_ok(),
        "RequestContext::ct was not cancelled within 2 s"
    );
    assert!(dropped.is_ok(), "the handler was not dropped within 2 s");
}
