//! MCP P1 bridge-socket tests (`docs/v2/design/mcp-p1.md` §6, tests 11, 16,
//! 17 and 18, and the header encoding of §3.4).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::bindings::{BindingState, ToolClass};
use super::bridge::relay;
use super::mcp_p1_cancel_tests::{Durable, until, wait_call};
use super::mcp_p1_rig_tests::{MAIL, MODERN, Rig, binding, forbidden, legacy, modern, tool_text};
use super::mcp_service::bridge_headers;
use crate::store::coordinator_outbox::OutboxState;

fn initialize(id: i64, version: &str) -> Value {
    legacy(
        id,
        "initialize",
        json!({"protocolVersion": version, "capabilities": {},
               "clientInfo": {"name": "t", "version": "1"}}),
    )
}

fn header<'a>(headers: &'a http::HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().unwrap())
}

/// Connects, sends `AUTH <token>` and the message, and keeps the stream open.
async fn open_socket(rig: &Rig, message: &Value) -> UnixStream {
    let mut stream = UnixStream::connect(&rig.path).await.unwrap();
    stream
        .write_all(format!("AUTH {}\n{message}\n", rig.token).as_bytes())
        .await
        .unwrap();
    stream
}

#[tokio::test]
async fn the_bridge_serves_both_eras_and_remembers_the_legacy_version() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    assert_eq!(r.bindings.legacy_version("b-1"), None);
    let got = r.sock_json(&r.token, &initialize(1, "2025-06-18")).await;
    assert_eq!(got["result"]["protocolVersion"], "2025-06-18", "{got}");
    assert_eq!(got["result"]["instructions"], "SKILL v1");
    assert_eq!(
        r.bindings.legacy_version("b-1").as_deref(),
        Some("2025-06-18")
    );
    // A `tools/list` line without `_meta` is answered in the legacy era: the
    // bridge adds the remembered version as the header.
    let list = legacy(2, "tools/list", json!({}));
    let headers = bridge_headers(&list, Some("2025-06-18"));
    assert_eq!(header(&headers, "mcp-protocol-version"), Some("2025-06-18"));
    assert_eq!(header(&headers, "mcp-method"), None);
    let got = r.sock_json(&r.token, &list).await;
    assert!(got["result"]["tools"].is_array(), "{got}");
    for key in ["resultType", "ttlMs", "cacheScope"] {
        assert!(got["result"].get(key).is_none(), "{key}: {got}");
    }
    // A modern line carries its version in `_meta`; the memory does not
    // change it. The answer has `resultType` `complete`.
    let (call, _) = modern(
        MODERN,
        3,
        "tools/call",
        json!({"name": "list_sessions", "arguments": {}}),
    );
    let headers = bridge_headers(&call, Some("2025-06-18"));
    assert_eq!(header(&headers, "mcp-protocol-version"), Some(MODERN));
    assert_eq!(header(&headers, "mcp-method"), Some("tools/call"));
    assert_eq!(header(&headers, "mcp-name"), Some("list_sessions"));
    let got = r.sock_json(&r.token, &call).await;
    assert_eq!(got["result"]["resultType"], "complete", "{got}");
    assert_eq!(got["result"]["isError"], false);
    // An `initialize` never gets a version header, also with a memory.
    let headers = bridge_headers(&initialize(4, "2025-11-25"), Some("2025-06-18"));
    assert_eq!(header(&headers, "mcp-protocol-version"), None);
    // A notification gets no line.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(r.sock(&r.token, &note.to_string()).await, "");
}

#[tokio::test]
async fn the_bridge_reinitializes_with_another_version_and_bindings_do_not_share_the_memory() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let other = r.add("b-2", BindingState::Active, MAIL);
    let version = |token: &str| {
        let digest = super::bindings::digest(token);
        let id = r.bindings.get_digest(&digest).unwrap().binding_id;
        r.bindings.legacy_version(&id)
    };
    let negotiated = |got: &Value| {
        got["result"]["protocolVersion"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let a = r.sock_json(&r.token, &initialize(1, "2025-06-18")).await;
    assert_eq!(negotiated(&a), "2025-06-18");
    // Interleaved: the other binding has no memory yet.
    assert_eq!(version(&other), None);
    let b = r.sock_json(&other, &initialize(1, "2025-03-26")).await;
    assert_eq!(negotiated(&b), "2025-03-26");
    assert_eq!(version(&r.token).as_deref(), Some("2025-06-18"));
    assert_eq!(version(&other).as_deref(), Some("2025-03-26"));
    // A second `initialize` with another version replaces the memory (the
    // earlier version is not sent as a header with it).
    let again = r.sock_json(&r.token, &initialize(2, "2025-11-25")).await;
    assert_eq!(negotiated(&again), "2025-11-25");
    assert_eq!(version(&r.token).as_deref(), Some("2025-11-25"));
    assert_eq!(version(&other).as_deref(), Some("2025-03-26"));
    // An unsupported version falls back; the fallback is what is remembered.
    let fallback = r.sock_json(&r.token, &initialize(3, "2099-01-01")).await;
    assert_eq!(negotiated(&fallback), "2025-11-25");
    assert_eq!(version(&r.token).as_deref(), Some("2025-11-25"));
    let down = r.sock_json(&other, &initialize(2, "2024-11-05")).await;
    assert_eq!(negotiated(&down), "2025-11-25");
    assert_eq!(version(&other).as_deref(), Some("2025-11-25"));
    // A failed `initialize` (no client info) leaves the old value.
    let bad = legacy(4, "initialize", json!({"protocolVersion": "2025-06-18"}));
    let failed = r.sock_json(&r.token, &bad).await;
    assert!(failed["error"].is_object(), "{failed}");
    assert_eq!(version(&r.token).as_deref(), Some("2025-11-25"));
    // A new credential for the binding starts without memory.
    let (token, d) = super::bindings::mint().unwrap();
    r.bindings
        .insert(d, binding("b-1", BindingState::Active, MAIL));
    assert_eq!(version(token.expose()), None);
    assert_eq!(r.bindings.legacy_version("b-1"), None);
}

#[tokio::test]
async fn a_binding_limit_on_the_socket_gets_a_correlated_32000_line_and_the_bridge_maps_limited_to_32000()
 {
    let r = Rig::start(BindingState::Active, MAIL).await;
    for n in 0..20 {
        let got = r.sock_json(&r.token, &legacy(n, "ping", json!({}))).await;
        assert_eq!(got["result"], json!({}), "{n}: {got}");
    }
    // The 21st message in the second is over the rate limit.
    let line = r
        .sock(&r.token, &legacy(99, "ping", json!({})).to_string())
        .await;
    let got: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        got,
        json!({"jsonrpc": "2.0", "id": 99,
               "error": {"code": -32000, "message": "Too many calls; try again in a moment."}})
    );
    let text = json!({"jsonrpc": "2.0", "id": "abc", "method": "tools/list"}).to_string();
    let got: Value = serde_json::from_str(&r.sock(&r.token, &text).await).unwrap();
    assert_eq!(got["id"], "abc");
    assert_eq!(got["error"]["code"], -32000);
    // A notification gets no line.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    assert_eq!(r.sock(&r.token, &note).await, "");
    // A body that is not JSON, an array and a scalar get `ERR bad_request`.
    for body in ["not json", "[1]", "[]", "5", "\"x\"", "null"] {
        assert_eq!(r.sock(&r.token, body).await, "ERR bad_request", "{body}");
    }
    assert_eq!(r.fake.forwards(), 0);

    // The bridge client maps `ERR limited` (the unauthenticated limit, before
    // the body) to the same code.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.s");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("AUTH ")
        );
        lines.next_line().await.unwrap().unwrap();
        write.write_all(b"ERR limited\n").await.unwrap();
    });
    let (mut stdin_w, stdin_r) = tokio::io::duplex(4096);
    let (stdout_w, mut stdout_r) = tokio::io::duplex(4096);
    stdin_w
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\"}\n")
        .await
        .unwrap();
    drop(stdin_w);
    relay(&path, "fdc1_x", stdin_r, stdout_w).await;
    server.await.unwrap();
    let mut out = String::new();
    stdout_r.read_to_string(&mut out).await.unwrap();
    let got: Value = serde_json::from_str(out.trim_end()).unwrap();
    assert_eq!(got["id"], 7);
    assert_eq!(got["error"]["code"], -32000);
    assert_eq!(
        got["error"]["message"],
        "Too many calls; try again in a moment."
    );
}

#[tokio::test]
async fn a_full_socket_close_cancels_the_call_and_a_half_close_still_gets_its_reply() {
    // A wait: the close drops the call; nothing is retried.
    let r = Rig::start(BindingState::Active, MAIL).await;
    r.fake.gate.store(true, Ordering::SeqCst);
    let stream = open_socket(&r, &wait_call(1)).await;
    r.fake.entered(1).await;
    drop(stream);
    r.fake.dropped(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(r.fake.forwards(), 1, "no retry");
    // The permit is free: the next message is answered.
    r.fake.gate.store(false, Ordering::SeqCst);
    let ok = r
        .sock_json(&r.token, &legacy(2, "tools/list", json!({})))
        .await;
    assert!(ok["result"]["tools"].is_array(), "{ok}");

    // A desktop call: exactly one forward, no retry after the close.
    let desktop = [ToolClass::Desktop, ToolClass::Capture, ToolClass::Own];
    let d = Rig::start(BindingState::Active, &desktop).await;
    d.bindings.set_fleet_control(true);
    d.fake.gate.store(true, Ordering::SeqCst);
    let (call, _) = modern(
        MODERN,
        1,
        "tools/call",
        json!({"name": "desktop_info", "arguments": {}}),
    );
    let stream = open_socket(&d, &call).await;
    d.fake.entered(1).await;
    drop(stream);
    d.fake.dropped(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(d.fake.desktop.lock().unwrap().len(), 1, "one forward");
    assert_eq!(d.fake.calls.lock().unwrap().len(), 0);

    // A durable send: the row stays pending and the next flush sends it.
    let durable = Durable::start().await;
    let stream = open_socket(&durable.rig, &Durable::mail()).await;
    durable.entered().await;
    drop(stream);
    until("the deck request was not dropped", || {
        durable.deck.dropped.load(Ordering::SeqCst) == 1
    })
    .await;
    let id = durable.deck.sent.lock().unwrap()[0].0.clone().unwrap();
    let row = durable
        .store
        .coordinator_row(id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, OutboxState::Pending);
    durable.deck.gate.store(false, Ordering::SeqCst);
    durable.forward.flush().await.unwrap();
    let sent = durable.deck.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the same command id and envelope");
    let row = durable.store.coordinator_row(id).await.unwrap().unwrap();
    assert_eq!(row.state, OutboxState::Done);

    // A half-close (the wire-v1 client closes its write side) is not a
    // cancel: the call runs on and the reply arrives.
    r.fake.gate.store(true, Ordering::SeqCst);
    let message = wait_call(3);
    let mut stream = UnixStream::connect(&r.path).await.unwrap();
    stream
        .write_all(format!("AUTH {}\n{message}\n", r.token).as_bytes())
        .await
        .unwrap();
    stream.shutdown().await.unwrap();
    r.fake.entered(1).await;
    // Several watcher ticks pass; the call is still alive.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        r.fake.dropped.load(Ordering::SeqCst),
        1,
        "only the first drop"
    );
    r.fake.open.send_replace(true);
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(stream).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    let got: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(got["id"], 3);
    assert_eq!(tool_text(&got).0, false, "{got}");
}

#[tokio::test]
async fn a_tool_name_that_is_not_a_plain_header_value_travels_in_the_base64_form() {
    let encoded = |text: &str| format!("=?base64?{}?=", STANDARD.encode(text));
    let names = [
        ("h\u{e9}llo", true),
        ("\u{65e5}\u{672c}", true),
        (" padded", true),
        ("tab\t", true),
        ("=?base64?abcd?=", true),
        ("plain_name-1", false),
    ];
    for (name, wrapped) in names {
        let (call, _) = modern(
            MODERN,
            1,
            "tools/call",
            json!({"name": name, "arguments": {}}),
        );
        let headers = bridge_headers(&call, None);
        let got = header(&headers, "mcp-name").unwrap();
        if wrapped {
            assert_eq!(got, encoded(name), "{name:?}");
        } else {
            assert_eq!(got, name);
        }
    }
    // On the wire: the name reaches the handler, which refuses the unknown
    // tool as a tool error (not as a header mismatch).
    let r = Rig::start(BindingState::Active, MAIL).await;
    for name in ["h\u{e9}llo", " padded", "=?base64?abcd?="] {
        let (call, _) = modern(
            MODERN,
            1,
            "tools/call",
            json!({"name": name, "arguments": {}}),
        );
        let got = r.sock_json(&r.token, &call).await;
        assert_eq!(tool_text(&got), (true, forbidden(name)), "{name:?}: {got}");
    }
}
