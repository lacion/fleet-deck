//! Station ingress negative tests (ADR-006 §8.1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(Caller, String, Value)>>,
    delay: Option<Duration>,
}

#[async_trait]
impl Forward for Recorder {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((caller, tool.to_owned(), args));
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        Ok(json!({"state": "accepted"}))
    }
}

struct Rig {
    ingress: Ingress,
    bindings: Bindings,
    forward: Arc<Recorder>,
    token: String,
}

fn binding(id: &str, state: BindingState, classes: &[ToolClass]) -> Binding {
    Binding {
        binding_id: id.to_owned(),
        grant_id: format!("grant-{id}"),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state != BindingState::Pending).then(|| "native-1".to_owned()),
        generation: u32::from(state != BindingState::Pending),
        classes: classes.iter().copied().collect(),
        state,
        control_skill: "SKILL v1".to_owned(),
    }
}

async fn rig_with(
    state: BindingState,
    classes: &[ToolClass],
    limits: Limits,
    delay: Option<Duration>,
) -> Rig {
    let bindings = Bindings::default();
    let (token, d) = mint().unwrap();
    bindings.insert(d, binding("b-1", state, classes));
    let forward = Arc::new(Recorder {
        delay,
        ..Recorder::default()
    });
    let ingress = Ingress::start(bindings.clone(), forward.clone(), limits)
        .await
        .unwrap();
    Rig {
        ingress,
        bindings,
        forward,
        token: token.into_exposed(),
    }
}

async fn rig(state: BindingState, classes: &[ToolClass]) -> Rig {
    rig_with(state, classes, Limits::default(), None).await
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

async fn raw(addr: SocketAddr, request: String) -> Reply {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

fn request(addr: SocketAddr, token: Option<&str>, extra: &str, body: &str) -> String {
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n{auth}{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        body.len()
    )
}

async fn rpc(r: &Rig, token: Option<&str>, msg: Value) -> Reply {
    raw(
        r.ingress.addr,
        request(r.ingress.addr, token, "", &msg.to_string()),
    )
    .await
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
           "params": {"name": name, "arguments": args}})
}

fn list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

fn names(reply: &Reply) -> Vec<String> {
    let v: Value = serde_json::from_str(&reply.body).unwrap();
    v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

fn tool_result(reply: &Reply) -> (bool, String) {
    let v: Value = serde_json::from_str(&reply.body).unwrap();
    (
        v["result"]["isError"].as_bool().unwrap(),
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

const MAIL: &[ToolClass] = &[ToolClass::Read, ToolClass::Mail];

#[tokio::test]
async fn a_request_without_a_credential_gets_401() {
    let r = rig(BindingState::Active, MAIL).await;
    let reply = rpc(&r, None, list()).await;
    assert_eq!(reply.status, 401);
    assert!(reply.head.contains("www-authenticate: bearer"));
    assert_eq!(reply.body, r#"{"error":"unauthorized"}"#);
    assert!(r.forward.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_an_expired_and_a_revoked_credential_get_the_same_401_as_a_missing_one() {
    let r = rig(BindingState::Active, MAIL).await;
    let missing = rpc(&r, None, list()).await;
    let (unknown, _) = mint().unwrap();
    let mut replies = vec![
        rpc(&r, Some(unknown.expose()), list()).await,
        rpc(&r, Some("fdc1_short"), list()).await,
    ];
    for state in [BindingState::Expired, BindingState::Revoked] {
        r.bindings.update("b-1", |b| b.state = state);
        replies.push(rpc(&r, Some(&r.token), list()).await);
    }
    for reply in replies {
        assert_eq!(reply.status, missing.status);
        assert_eq!(reply.body, missing.body);
        assert!(reply.head.contains("www-authenticate: bearer"));
    }
}

#[tokio::test]
async fn a_request_with_an_origin_header_or_a_foreign_host_is_rejected_before_authentication() {
    let r = rig(BindingState::Active, MAIL).await;
    let addr = r.ingress.addr;
    let body = list().to_string();
    let origin = request(
        addr,
        Some(&r.token),
        "Origin: http://evil.example\r\n",
        &body,
    );
    assert_eq!(raw(addr, origin).await.status, 403);
    let foreign = request(addr, Some(&r.token), "", &body).replace(
        &format!("Host: 127.0.0.1:{}", addr.port()),
        "Host: evil.example",
    );
    assert_eq!(raw(addr, foreign).await.status, 403);
    let text = request(addr, Some(&r.token), "", &body)
        .replace("Content-Type: application/json", "Content-Type: text/plain");
    assert_eq!(raw(addr, text).await.status, 415);
    let get = format!(
        "GET /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    );
    assert_eq!(raw(addr, get).await.status, 405);
    let other = request(addr, Some(&r.token), "", &body).replace("POST /mcp", "POST /other");
    assert_eq!(raw(addr, other).await.status, 404);
    // `localhost` is a valid host.
    let local =
        request(addr, Some(&r.token), "", &body).replace("Host: 127.0.0.1", "Host: localhost");
    assert_eq!(raw(addr, local).await.status, 200);
}

#[tokio::test]
async fn a_tool_parameter_that_names_another_session_does_not_change_the_caller() {
    let r = rig(BindingState::Active, MAIL).await;
    let args = json!({"to": {"station": "alpha", "session_id": "worker-1"}, "text": "hi",
                      "caller": "victim-1", "session_id": "victim-1", "binding": {"binding_id": "b-9"}});
    let reply = rpc(&r, Some(&r.token), call("send_mail", args)).await;
    assert!(!tool_result(&reply).0, "{}", reply.body);
    let calls = r.forward.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.session_id.as_str(), "coord-1");
    assert_eq!(calls[0].0.binding_id, "b-1");
    assert_eq!(calls[0].0.native_id, "native-1");
}

#[tokio::test]
async fn a_pending_binding_discovers_the_fixed_granted_catalog_and_every_execution_returns_not_bound()
 {
    let r = rig(BindingState::Pending, MAIL).await;
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18"}});
    let reply = rpc(&r, Some(&r.token), init).await;
    let v: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(v["result"]["instructions"], "SKILL v1");
    let bad = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                     "params": {"protocolVersion": "2024-11-05"}});
    let v: Value = serde_json::from_str(&rpc(&r, Some(&r.token), bad).await.body).unwrap();
    assert!(v["error"].is_object());
    let tools = names(&rpc(&r, Some(&r.token), list()).await);
    assert_eq!(
        tools,
        [
            "list_sessions",
            "wait_task",
            "wait_tasks",
            "send_mail",
            "assign",
            "assign_auto",
            "complete_task",
            "ask_coordinator"
        ]
    );
    for tool in &tools {
        let reply = rpc(&r, Some(&r.token), call(tool, json!({}))).await;
        assert_eq!(tool_result(&reply), (true, "not_bound".to_owned()));
    }
    assert!(r.forward.calls.lock().unwrap().is_empty());
}

async fn snapshot(r: &Rig) -> (Value, Vec<String>) {
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-03-26"}});
    let i: Value = serde_json::from_str(&rpc(r, Some(&r.token), init).await.body).unwrap();
    (
        i["result"]["instructions"].clone(),
        names(&rpc(r, Some(&r.token), list()).await),
    )
}

#[tokio::test]
async fn the_tool_catalog_and_the_cached_control_skill_are_unchanged_across_activation_and_renewal()
{
    let r = rig(BindingState::Pending, MAIL).await;
    let pending = snapshot(&r).await;
    assert_eq!(
        r.bindings.native_identity("b-1", "native-1"),
        Some(BindingState::Active)
    );
    let active = snapshot(&r).await;
    // Renewal: expired → active with the next generation.
    r.bindings.update("b-1", |b| {
        b.state = BindingState::Active;
        b.generation = 2;
    });
    let renewed = snapshot(&r).await;
    assert_eq!(pending, active);
    assert_eq!(active, renewed);
}

#[tokio::test]
async fn an_accepted_notification_gets_202_without_a_body_and_a_batch_is_refused() {
    let r = rig(BindingState::Active, MAIL).await;
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let reply = rpc(&r, Some(&r.token), note).await;
    assert_eq!(reply.status, 202);
    assert!(reply.body.is_empty());
    let batch = json!([call("send_mail", json!({})), call("send_mail", json!({}))]);
    let reply = rpc(&r, Some(&r.token), batch).await;
    assert_eq!(reply.status, 400);
    assert!(r.forward.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tool_outside_the_granted_classes_is_not_listed_and_is_refused_without_a_forward() {
    let r = rig(BindingState::Active, MAIL).await;
    let tools = names(&rpc(&r, Some(&r.token), list()).await);
    for absent in ["spawn", "stop", "interrupt"] {
        assert!(!tools.iter().any(|t| t == absent));
        let reply = rpc(&r, Some(&r.token), call(absent, json!({}))).await;
        let (is_error, text) = tool_result(&reply);
        assert!(is_error);
        assert!(text.starts_with("forbidden"), "{text}");
    }
    assert!(r.forward.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_idle_binding_expires_and_a_renewal_does_not_authorize_an_operation_queued_before_it() {
    let r = rig(BindingState::Active, MAIL).await;
    // The deck answered an idle expiry: the station marks the binding expired.
    r.bindings
        .update("b-1", |b| b.state = BindingState::Expired);
    let reply = rpc(&r, Some(&r.token), call("send_mail", json!({}))).await;
    assert_eq!(reply.status, 401);
    // Renewal raises the generation; a forwarded call carries the new one,
    // so an envelope of generation 1 can never be authorized again.
    r.bindings.update("b-1", |b| {
        b.state = BindingState::Active;
        b.generation = 2;
    });
    let reply = rpc(&r, Some(&r.token), call("send_mail", json!({}))).await;
    assert_eq!(reply.status, 200);
    let calls = r.forward.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.generation, 2);
}

#[tokio::test]
async fn process_exit_session_stop_and_a_replaced_native_identity_each_revoke_the_binding() {
    let r = rig(BindingState::Active, MAIL).await;
    r.bindings.revoke_session(&"coord-1".into());
    assert_eq!(rpc(&r, Some(&r.token), list()).await.status, 401);
    r.bindings.update("b-1", |b| b.state = BindingState::Active);
    // The same identity again: no change.
    assert_eq!(
        r.bindings.native_identity("b-1", "native-1"),
        Some(BindingState::Active)
    );
    assert_eq!(
        r.bindings.native_identity("b-1", "native-2"),
        Some(BindingState::Revoked)
    );
    assert_eq!(rpc(&r, Some(&r.token), list()).await.status, 401);
    // A revoked binding never returns through a late identity signal.
    assert_eq!(
        r.bindings.native_identity("b-1", "native-1"),
        Some(BindingState::Revoked)
    );
}

#[tokio::test]
async fn stalled_headers_stalled_bodies_oversized_bodies_and_excess_connections_stay_bounded_and_an_authenticated_caller_is_served()
 {
    let limits = Limits {
        connections: 8,
        unauthenticated: 2,
        headers: Duration::from_millis(300),
        body: Duration::from_millis(600),
        body_bytes: 1024,
    };
    let r = rig_with(BindingState::Active, MAIL, limits, None).await;
    let addr = r.ingress.addr;
    // A body above the limit: 413 at once.
    let big = "x".repeat(2048);
    assert_eq!(
        raw(addr, request(addr, Some(&r.token), "", &big))
            .await
            .status,
        413
    );
    // Stalled bodies hold the unauthenticated slots; the next request gets
    // 429 at once; the stalled ones close after the body time.
    let mut stalled = Vec::new();
    for _ in 0..2 {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let head = request(addr, Some(&r.token), "", "")
            .replace("Content-Length: 0", "Content-Length: 100");
        s.write_all(head.as_bytes()).await.unwrap();
        stalled.push(s);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    let reply = rpc(&r, Some(&r.token), list()).await;
    assert_eq!(reply.status, 429);
    assert!(started.elapsed() < Duration::from_millis(300));
    for mut s in stalled {
        let mut buf = Vec::new();
        let t = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
        assert!(t.is_ok(), "a stalled body is closed");
    }
    // Stalled headers close after the header time.
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"POST /mcp HTTP/1.1\r\nHost: ").await.unwrap();
    let mut buf = Vec::new();
    let t = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut buf)).await;
    assert!(t.is_ok(), "stalled headers are closed");
    // Fill every connection; the next one closes at once.
    let mut open = Vec::new();
    for _ in 0..7 {
        open.push(TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut extra = TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = extra.write_all(b"POST /mcp HTTP/1.1\r\n").await;
    let mut buf = Vec::new();
    let n = tokio::time::timeout(Duration::from_millis(500), extra.read_to_end(&mut buf)).await;
    assert!(
        matches!(n, Ok(Ok(0)) | Ok(Err(_))),
        "the connection above the limit is closed"
    );
    drop(open);
    tokio::time::sleep(Duration::from_millis(400)).await;
    // An authenticated caller is served from a free connection.
    assert_eq!(rpc(&r, Some(&r.token), list()).await.status, 200);
}

#[tokio::test]
async fn the_per_binding_limit_counts_requests_that_wait_for_the_deck() {
    let r = rig_with(
        BindingState::Active,
        MAIL,
        Limits::default(),
        Some(Duration::from_millis(500)),
    )
    .await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let (addr, req) = (
            r.ingress.addr,
            request(
                r.ingress.addr,
                Some(&r.token),
                "",
                &call("send_mail", json!({})).to_string(),
            ),
        );
        set.spawn(async move { raw(addr, req).await.status });
    }
    let mut statuses = Vec::new();
    while let Some(s) = set.join_next().await {
        statuses.push(s.unwrap());
    }
    statuses.sort_unstable();
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        bindings::BINDING_CONCURRENCY
    );
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 4);
    assert_eq!(
        r.forward.calls.lock().unwrap().len(),
        bindings::BINDING_CONCURRENCY
    );
}

#[test]
fn a_minted_credential_is_well_formed_and_only_its_digest_is_kept() {
    let (token, d) = mint().unwrap();
    assert!(bindings::well_formed(token.expose()));
    assert_eq!(d, digest(token.expose()));
    assert_eq!(d.len(), 64);
    assert!(!d.contains(token.expose().as_str()));
}

#[derive(Default)]
struct OwnTools {
    calls: Mutex<Vec<(Caller, String, Value)>>,
}

#[async_trait]
impl FleetTools for OwnTools {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((caller, tool.to_owned(), args));
        Ok(json!({"ok": true}))
    }
}

/// DL-165: a session without a grant sees only its own tools, the station
/// answers them (the deck forward is not asked), and a coordinator tool is
/// refused. R2.3: `ask_coordinator` is the one tool that goes to the deck
/// forward.
#[tokio::test]
async fn a_fleet_only_binding_sees_its_own_tools_and_the_station_answers_them() {
    let bindings = Bindings::default();
    let (token, d) = mint().unwrap();
    let mut b = binding("s1:fleet", BindingState::Active, &[ToolClass::Own]);
    b.grant_id = String::new();
    bindings.insert(d, b);
    let forward = Arc::new(Recorder::default());
    let own = Arc::new(OwnTools::default());
    let ingress = Ingress::start_with(
        bindings,
        forward.clone(),
        Some(own.clone()),
        Limits::default(),
    )
    .await
    .unwrap();
    let token = token.into_exposed();
    let send = |msg: Value| {
        raw(
            ingress.addr,
            request(ingress.addr, Some(&token), "", &msg.to_string()),
        )
    };
    assert_eq!(
        names(&send(list()).await),
        ["ask_coordinator", "publish_artifact", "set_focus"]
    );
    let reply = send(call("set_focus", json!({"text": "reading the tests"}))).await;
    assert!(!tool_result(&reply).0, "{}", reply.body);
    let reply = send(call("list_sessions", json!({}))).await;
    assert_eq!(
        tool_result(&reply),
        (true, "forbidden: the grant has no such tool".to_owned())
    );
    assert!(
        forward.calls.lock().unwrap().is_empty(),
        "the deck is not asked"
    );
    let reply = send(call("ask_coordinator", json!({"question": "which key?"}))).await;
    assert!(!tool_result(&reply).0, "{}", reply.body);
    let calls = own.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "ask_coordinator is not a local tool");
    assert_eq!(calls[0].1, "set_focus");
    assert_eq!(calls[0].0.session_id.as_str(), "coord-1");
    let forwarded = forward.calls.lock().unwrap();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0].1, "ask_coordinator");
    assert_eq!(forwarded[0].0.session_id.as_str(), "coord-1");
    assert!(forwarded[0].0.grant_id.is_empty());
    assert_eq!(forwarded[0].2, json!({"question": "which key?"}));
}
