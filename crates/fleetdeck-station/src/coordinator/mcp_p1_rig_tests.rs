//! The rig of the MCP P1 tests (`docs/v2/design/mcp-p1.md` §6): the station
//! with its HTTP ingress and its bridge socket, the fakes of the deck and of
//! the session tools, and the request helpers. The `mcp_p1_*tests.rs` files
//! share it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{Semaphore, watch};

use super::bindings::{Binding, BindingState, Bindings, Caller, ToolClass, digest, mint};
use super::desktop_forward::{DesktopRequest, McpToolResult};
use super::ingress::{FleetTools, Forward, Ingress, Limits};
use super::mcp_service::{McpService, bridge_headers};
use super::socket::BridgeSocket;

pub(super) const SKILL: &str = "SKILL v1";
/// The versions that the station serves (design §3.2).
pub(super) const VERSIONS: [&str; 4] = ["2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"];
pub(super) const MODERN: &str = "2026-07-28";
pub(super) const NOT_BOUND: &str =
    "not_bound: this session is still starting. Try again in a few seconds.";

pub(super) fn forbidden(name: &str) -> String {
    format!(
        "forbidden: your grant has no tool `{name}`. Call tools/list to see the tools you have."
    )
}

pub(super) fn binding(id: &str, state: BindingState, classes: &[ToolClass]) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: format!("grant-{id}"),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state != BindingState::Pending).then(|| "native-1".to_owned()),
        generation: u32::from(state != BindingState::Pending),
        classes: classes.iter().copied().collect(),
        state,
        control_skill: SKILL.to_owned(),
        scope: fleetdeck_proto::command::GrantScope::Lineage,
    }
}

/// Counts a gated call that was dropped before it was released.
struct DropCount<'a>(&'a AtomicUsize, bool);

impl Drop for DropCount<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// The deck side. While `gate` is set, every call waits for `open`.
pub(super) struct Fake {
    pub calls: Mutex<Vec<(Caller, String, Value)>>,
    pub desktop: Mutex<Vec<(Caller, DesktopRequest)>>,
    pub desktop_result: Mutex<McpToolResult>,
    pub gate: AtomicBool,
    /// One permit per call that entered the gate.
    pub entered: Semaphore,
    pub open: watch::Sender<bool>,
    /// Gated calls that were dropped before `open`.
    pub dropped: AtomicUsize,
}

impl Fake {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            desktop: Mutex::default(),
            desktop_result: Mutex::new(McpToolResult {
                content: vec![json!({"type": "text", "text": "{\"ok\":true}"})],
                is_error: false,
            }),
            gate: AtomicBool::new(false),
            entered: Semaphore::new(0),
            open: watch::channel(false).0,
            dropped: AtomicUsize::new(0),
        })
    }

    pub(super) fn forwards(&self) -> usize {
        self.calls.lock().unwrap().len() + self.desktop.lock().unwrap().len()
    }

    /// Waits until `n` gated calls entered.
    pub(super) async fn entered(&self, n: u32) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.acquire_many(n))
            .await
            .expect("the calls entered")
            .expect("open")
            .forget();
    }

    pub(super) async fn dropped(&self, n: usize) {
        for _ in 0..500 {
            if self.dropped.load(Ordering::SeqCst) >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{n} gated calls were not dropped");
    }

    async fn wait_gate(&self) {
        if !self.gate.load(Ordering::SeqCst) {
            return;
        }
        let mut guard = DropCount(&self.dropped, true);
        let mut rx = self.open.subscribe();
        self.entered.add_permits(1);
        rx.wait_for(|open| *open).await.expect("open");
        guard.1 = false;
    }
}

#[async_trait]
impl Forward for Fake {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((caller, tool.to_owned(), args));
        self.wait_gate().await;
        Ok(json!({"state": "accepted"}))
    }

    async fn call_desktop(&self, caller: Caller, request: DesktopRequest) -> McpToolResult {
        self.desktop.lock().unwrap().push((caller, request));
        self.wait_gate().await;
        self.desktop_result.lock().unwrap().clone()
    }
}

/// The station-owned session tools.
#[derive(Default)]
pub(super) struct FakeFleet {
    pub calls: Mutex<Vec<(Caller, String, Value)>>,
    pub focus_bytes: AtomicUsize,
    counter: AtomicU64,
}

#[async_trait]
impl FleetTools for FakeFleet {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((caller, tool.to_owned(), args.clone()));
        match tool {
            "publish_artifact" => Ok(json!({
                "artifact_id": format!("art-{}", self.counter.fetch_add(1, Ordering::SeqCst)),
                "version": 1,
                "title": args.get("title").cloned().unwrap_or(json!("untitled")),
                "created": true,
            })),
            "set_focus" => {
                let size = self.focus_bytes.load(Ordering::SeqCst);
                if size > 0 {
                    return Ok(json!({"focus": "x".repeat(size)}));
                }
                Ok(json!({"focus": args.get("text").cloned().unwrap_or(json!(""))}))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

/// One HTTP answer.
#[derive(Debug)]
pub(super) struct Http {
    pub status: u16,
    pub head: String,
    pub body: String,
}

impl Http {
    pub(super) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

pub(super) fn http_request(
    addr: SocketAddr,
    token: Option<&str>,
    extra: &[(String, String)],
    body: &str,
) -> String {
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let extra: String = extra.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n{auth}{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        body.len()
    )
}

pub(super) fn parse_http(buf: &[u8]) -> Http {
    let text = String::from_utf8_lossy(buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Http {
        status,
        head: head.to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

/// Sends a raw request and reads the answer to the end.
pub(super) async fn http_raw(addr: SocketAddr, request: String) -> Http {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut buf)).await;
    parse_http(&buf)
}

/// A request of the legacy era.
pub(super) fn legacy(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// A request of the modern era with its `_meta` and its standard headers.
pub(super) fn modern(
    version: &str,
    id: i64,
    method: &str,
    mut params: Value,
) -> (Value, Vec<(String, String)>) {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let mut headers = vec![
        ("MCP-Protocol-Version".to_owned(), version.to_owned()),
        ("Mcp-Method".to_owned(), method.to_owned()),
    ];
    if let Some(name) = params.get("name").and_then(Value::as_str)
        && method == "tools/call"
    {
        headers.push(("Mcp-Name".to_owned(), name.to_owned()));
    }
    (legacy(id, method, params), headers)
}

/// The station with its HTTP ingress and its bridge socket on one set of
/// bindings (as in production).
pub(super) struct Rig {
    pub bindings: Bindings,
    pub token: String,
    pub fake: Arc<Fake>,
    pub fleet: Arc<FakeFleet>,
    pub ingress: Ingress,
    /// Held so that the socket keeps listening.
    pub _socket: BridgeSocket,
    pub path: PathBuf,
    /// Held so that the socket directory stays.
    pub _dir: tempfile::TempDir,
    fresh: AtomicUsize,
}

impl Rig {
    pub(super) async fn start(state: BindingState, classes: &[ToolClass]) -> Self {
        Self::start_on(Fake::new(), state, classes).await
    }

    pub(super) async fn start_on(
        fake: Arc<Fake>,
        state: BindingState,
        classes: &[ToolClass],
    ) -> Self {
        let forward: Arc<dyn Forward> = fake.clone();
        Self::start_with(move |_| forward, fake, state, classes).await
    }

    /// The station on a forwarder that `make` builds from the bindings.
    pub(super) async fn start_with(
        make: impl FnOnce(&Bindings) -> Arc<dyn Forward>,
        fake: Arc<Fake>,
        state: BindingState,
        classes: &[ToolClass],
    ) -> Self {
        let bindings = Bindings::default();
        let (token, d) = mint().unwrap();
        bindings.insert(d, binding("b-1", state, classes));
        let forward = make(&bindings);
        let fleet = Arc::new(FakeFleet::default());
        let ingress = Ingress::start_with(
            bindings.clone(),
            forward.clone(),
            Some(fleet.clone()),
            Limits::default(),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.s");
        let socket = BridgeSocket::start(
            &path,
            bindings.clone(),
            forward,
            Some(fleet.clone()),
            Limits::default(),
        )
        .await
        .unwrap();
        Self {
            bindings,
            token: token.into_exposed(),
            fake,
            fleet,
            ingress,
            _socket: socket,
            path,
            _dir: dir,
            fresh: AtomicUsize::new(0),
        }
    }

    /// Adds another binding with its own credential; returns the credential.
    pub(super) fn add(&self, id: &str, state: BindingState, classes: &[ToolClass]) -> String {
        let (token, d) = mint().unwrap();
        self.bindings.insert(d, binding(id, state, classes));
        token.into_exposed()
    }

    /// A new binding with a credential of its own. The rate limit of a
    /// binding is 20 requests a second: a test that sends many requests uses
    /// one binding for each group.
    pub(super) fn fresh(&self, state: BindingState, classes: &[ToolClass]) -> String {
        let n = self.fresh.fetch_add(1, Ordering::SeqCst);
        self.add(&format!("fresh-{n}"), state, classes)
    }

    pub(super) fn addr(&self) -> SocketAddr {
        self.ingress.addr
    }

    /// One HTTP request with extra headers.
    pub(super) async fn http(
        &self,
        token: Option<&str>,
        extra: &[(String, String)],
        body: &Value,
    ) -> Http {
        let request = http_request(self.addr(), token, extra, &body.to_string());
        http_raw(self.addr(), request).await
    }

    /// A message of the legacy era over HTTP; `version` is the
    /// `MCP-Protocol-Version` header.
    pub(super) async fn http_legacy(&self, token: &str, version: &str, body: &Value) -> Http {
        let extra = [("MCP-Protocol-Version".to_owned(), version.to_owned())];
        self.http(Some(token), &extra, body).await
    }

    /// A message of the modern era over HTTP.
    pub(super) async fn http_modern(&self, token: &str, method: &str, params: Value) -> Http {
        let (body, headers) = modern(MODERN, 1, method, params);
        self.http(Some(token), &headers, &body).await
    }

    /// One line over the bridge socket; the reply line, empty for none.
    pub(super) async fn sock(&self, token: &str, line: &str) -> String {
        sock_exchange(&self.path, token, line).await
    }

    /// A message over the bridge socket, parsed.
    pub(super) async fn sock_json(&self, token: &str, message: &Value) -> Value {
        let line = self.sock(token, &message.to_string()).await;
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    /// The same request over HTTP (`true`) or the bridge socket (`false`);
    /// the JSON-RPC answer. The legacy version goes into the header (HTTP)
    /// or is remembered by an `initialize` (socket).
    pub(super) async fn rpc(
        &self,
        http: bool,
        token: &str,
        version: &str,
        method: &str,
        params: Value,
    ) -> Value {
        if version == MODERN {
            let (body, headers) = modern(version, 1, method, params);
            if http {
                return self.http(Some(token), &headers, &body).await.json();
            }
            return self.sock_json(token, &body).await;
        }
        let body = legacy(1, method, params);
        if http {
            return self.http_legacy(token, version, &body).await.json();
        }
        let init = legacy(
            0,
            "initialize",
            json!({"protocolVersion": version, "capabilities": {},
                   "clientInfo": {"name": "t", "version": "1"}}),
        );
        // The bridge remembers the version of the last `initialize`.
        let id = self.bindings.get_digest(&digest(token)).unwrap().binding_id;
        if self.bindings.legacy_version(&id).as_deref() != Some(version) {
            let got = self.sock_json(token, &init).await;
            assert_eq!(got["result"]["protocolVersion"], version, "{got}");
        }
        self.sock_json(token, &body).await
    }
}

/// One bridge exchange: `AUTH <token>`, the message line, the half-close and
/// the reply to the end of the stream.
pub(super) async fn sock_exchange(path: &Path, token: &str, line: &str) -> String {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream
        .write_all(format!("AUTH {token}\n{line}\n").as_bytes())
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(out).unwrap().trim_end().to_owned()
}

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

pub(super) const MAIL: &[ToolClass] = &[ToolClass::Read, ToolClass::Mail, ToolClass::Own];

pub(super) fn tool_names(answer: &Value) -> Vec<String> {
    answer["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

/// `(isError, text)` of a tool answer.
pub(super) fn tool_text(answer: &Value) -> (bool, String) {
    (
        answer["result"]["isError"]
            .as_bool()
            .unwrap_or_else(|| panic!("{answer}")),
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{answer}"))
            .to_owned(),
    )
}
