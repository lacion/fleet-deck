//! MCP P1 authorization tests (`docs/v2/design/mcp-p1.md` §6, tests 20 and
//! 23): the credential of an ended binding and the matrix of what no
//! protocol field can change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use fleetdeck_proto::coordinator_desktop_text::{code, text};
use futures_util::future::join_all;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};

use super::bindings::{Binding, BindingState, Caller, ToolClass, digest, mint};
use super::desktop_forward::McpToolResult;
use super::ingress::Forward;
use super::mcp_p1_rig_tests::{
    MAIL, MODERN, NOT_BOUND, Rig, VERSIONS, binding, forbidden, legacy, tool_text,
};
use super::mcp_server::StationMcp;

const OFF: &str = "not_granted: fleet control is off on this station. Ask the owner to turn on \"Allow fleet control\" in the station settings.";

#[tokio::test]
async fn a_held_credential_whose_binding_ended_gets_the_uniform_401() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let held = digest(&r.token);
    let list = legacy(1, "tools/list", json!({}));
    // The answers of a missing credential are the reference.
    let missing = r.http(None, &[], &list).await;
    assert_eq!(missing.status, 401);
    assert_eq!(missing.body, r#"{"error":"unauthorized"}"#);
    assert!(missing.head.contains("www-authenticate: bearer"));
    // An active held credential is "starting".
    r.bindings.hold([held.clone()]);
    assert_eq!(r.http(Some(&r.token), &[], &list).await.status, 503);
    assert_eq!(r.sock(&r.token, &list.to_string()).await, "ERR starting");
    // The same credential, once its binding ended, is the uniform answer.
    for state in [BindingState::Revoked, BindingState::Expired] {
        r.bindings.update("b-1", |b| b.state = state);
        assert!(r.bindings.is_held(&held));
        let got = r.http(Some(&r.token), &[], &list).await;
        assert_eq!(got.status, missing.status, "{state:?}");
        assert_eq!(got.body, missing.body, "{state:?}");
        assert!(got.head.contains("www-authenticate: bearer"), "{state:?}");
        assert_eq!(
            r.sock(&r.token, &list.to_string()).await,
            "ERR unauthorized",
            "{state:?}"
        );
    }
}

/// Echoes who called and what, so that a mix-up between bindings shows.
#[derive(Default)]
struct Echo {
    calls: Mutex<Vec<(Caller, String)>>,
}

#[async_trait]
impl Forward for Echo {
    async fn call(&self, caller: Caller, tool: &str, args: Value) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((caller.clone(), tool.to_owned()));
        tokio::task::yield_now().await;
        Ok(
            json!({"caller": caller.binding_id, "native": caller.native_id,
                  "session": caller.session_id.as_str(), "n": args["n"]}),
        )
    }
}

fn error_text(code: &str) -> String {
    json!({"error": code, "message": text(code).unwrap()}).to_string()
}

fn mail_args(extra: Value) -> Value {
    let mut args = json!({"to": {"station": "alpha", "session_id": "worker-1"}, "text": "hi"});
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

#[tokio::test]
async fn the_transport_never_lets_a_field_change_the_caller_or_the_grant() {
    let desktop = [ToolClass::Desktop, ToolClass::Capture, ToolClass::Own];
    let inspect = [ToolClass::Inspect, ToolClass::Own];
    let r = Rig::start(BindingState::Active, MAIL).await;
    for version in VERSIONS {
        for http in [true, false] {
            let label = format!("{version} http={http}");
            // A forged caller in the arguments, in `_meta` and in `clientInfo`.
            let token = r.fresh(BindingState::Active, MAIL);
            let forged = json!({"caller": "victim-1", "session_id": "victim-1",
                                "binding": {"binding_id": "b-9"}, "native_id": "victim-native"});
            let mut params = json!({"name": "send_mail", "arguments": mail_args(forged)});
            if version == MODERN {
                // The modern `rmcp` request carries the extra `_meta` keys.
                params["_meta"] = json!({"caller": "victim-1",
                    "io.modelcontextprotocol/clientInfo": {"name": "victim-1", "version": "1"}});
            }
            let before = r.fake.calls.lock().unwrap().len();
            let answer = r.rpc(http, &token, version, "tools/call", params).await;
            assert_eq!(tool_text(&answer).0, false, "{label}: {answer}");
            let calls = r.fake.calls.lock().unwrap();
            assert_eq!(calls.len(), before + 1, "{label}");
            let caller = &calls[before].0;
            assert_eq!(caller.session_id.as_str(), "coord-1", "{label}");
            assert_eq!(caller.native_id, "native-1", "{label}");
            assert!(caller.binding_id.starts_with("fresh-"), "{label}");
            drop(calls);
            if version != MODERN {
                // A forged `clientInfo` in `initialize`.
                let init = legacy(
                    3,
                    "initialize",
                    json!({"protocolVersion": version, "capabilities": {},
                           "clientInfo": {"name": "victim-1", "version": "binding:b-9"}}),
                );
                let answer = r.http(Some(&token), &[], &init).await.json();
                assert_eq!(answer["result"]["instructions"], "SKILL v1", "{label}");
            }

            // A class denial.
            let spawn = json!({"name": "spawn", "arguments": {"station": "s",
                "spec": {"provider_instance": "claude", "cwd": "/w"}}});
            let before = r.fake.forwards();
            let answer = r.rpc(http, &token, version, "tools/call", spawn).await;
            assert_eq!(tool_text(&answer), (true, forbidden("spawn")), "{label}");
            assert_eq!(r.fake.forwards(), before, "{label}");

            // A pending binding.
            let pending = r.fresh(BindingState::Pending, MAIL);
            let call = json!({"name": "list_sessions", "arguments": {}});
            let answer = r.rpc(http, &pending, version, "tools/call", call).await;
            assert_eq!(tool_text(&answer), (true, NOT_BOUND.to_owned()), "{label}");
            assert_eq!(r.fake.forwards(), before, "{label}");

            // The live switch, turned off between two calls.
            let token = r.fresh(BindingState::Active, &inspect);
            let read = json!({"name": "read_session", "arguments": {"session_id": "s1"}});
            r.bindings.set_fleet_control(true);
            let first = r
                .rpc(http, &token, version, "tools/call", read.clone())
                .await;
            assert_eq!(tool_text(&first).0, false, "{label}: {first}");
            assert_eq!(r.fake.forwards(), before + 1, "{label}");
            r.bindings.set_fleet_control(false);
            let second = r.rpc(http, &token, version, "tools/call", read).await;
            assert_eq!(tool_text(&second), (true, OFF.to_owned()), "{label}");
            assert_eq!(r.fake.forwards(), before + 1, "{label}");
        }
    }
    // A revoked binding and a changed generation while a delayed desktop
    // result is in flight: the result never reaches the client.
    r.bindings.set_fleet_control(true);
    r.fake.gate.store(true, std::sync::atomic::Ordering::SeqCst);
    *r.fake.desktop_result.lock().unwrap() = McpToolResult {
        content: vec![
            json!({"type": "text", "text": "{\"capture_id\":\"cap-1\"}"}),
            json!({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="}),
        ],
        is_error: false,
    };
    let read = json!({"name": "read_capture_image", "arguments":
        {"artifact": {"station_id": "st", "artifact_id": "a1", "version": 1}}});
    for version in VERSIONS {
        for http in [true, false] {
            for change in ["revoked", "generation"] {
                let token = r.fresh(BindingState::Active, &desktop);
                let id = r.bindings.get_digest(&digest(&token)).unwrap().binding_id;
                let call = r.rpc(http, &token, version, "tools/call", read.clone());
                let flip = async {
                    r.fake.entered(1).await;
                    r.bindings.update(&id, |b| match change {
                        "revoked" => b.state = BindingState::Revoked,
                        _ => b.generation += 1,
                    });
                    r.fake.open.send_replace(true);
                };
                let (answer, ()) = tokio::join!(call, flip);
                r.fake.open.send_replace(false);
                let label = format!("{version} http={http} {change}");
                assert_eq!(
                    tool_text(&answer),
                    (true, error_text(code::AUTHORITY_LOST)),
                    "{label}"
                );
                assert_eq!(answer["result"]["content"].as_array().unwrap().len(), 1);
                assert!(!answer.to_string().contains("image/png"), "{label}");
            }
        }
    }
}

#[tokio::test]
async fn a_handler_without_a_binding_extension_fails_closed() {
    let echo = Arc::new(Echo::default());
    let server = StationMcp::new(Default::default(), echo.clone(), None);
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );
    let body = legacy(
        1,
        "tools/call",
        json!({"name": "list_sessions", "arguments": {}}),
    )
    .to_string();
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("/mcp")
        .header(http::header::HOST, "127.0.0.1")
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::ACCEPT, "application/json, text/event-stream")
        .body(http_body_util::Full::new(Bytes::from(body)))
        .unwrap();
    let response = service.handle(request).await;
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], -32603, "{v}");
    assert_eq!(v["error"]["message"], "no binding");
    assert!(echo.calls.lock().unwrap().is_empty(), "no forward");
}

#[tokio::test]
async fn two_bindings_in_parallel_never_see_each_others_identity_or_results() {
    let echo = Arc::new(Echo::default());
    let forward: Arc<dyn Forward> = echo.clone();
    let r = Rig::start_with(
        forward,
        super::mcp_p1_rig_tests::Fake::new(),
        BindingState::Active,
        MAIL,
    )
    .await;
    let a = r.add("b-a", BindingState::Active, MAIL);
    let b = r.add("b-b", BindingState::Active, MAIL);
    r.bindings
        .update("b-b", |x| x.native_id = Some("native-b".to_owned()));
    for http in [true, false] {
        let calls = (0..4).flat_map(|n| [(&a, "b-a", "native-1", n), (&b, "b-b", "native-b", n)]);
        let answers = join_all(calls.map(|(token, id, native, n)| {
            let r = &r;
            async move {
                let call = json!({"name": "list_sessions", "arguments": {"n": n}});
                let got = r.rpc(http, token, MODERN, "tools/call", call).await;
                (id, native, n, got)
            }
        }))
        .await;
        for (id, native, n, got) in answers {
            let (is_error, text) = tool_text(&got);
            assert!(!is_error, "{got}");
            let result: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                result,
                json!({"caller": id, "native": native, "session": "coord-1", "n": n})
            );
        }
    }
    assert_eq!(echo.calls.lock().unwrap().len(), 16);
}

#[tokio::test]
async fn the_catalog_and_the_instructions_are_byte_identical_across_the_binding_life() {
    let r = Rig::start(BindingState::Pending, MAIL).await;
    let (list, headers) = super::mcp_p1_rig_tests::modern(MODERN, 1, "tools/list", json!({}));
    let (discover, dheaders) =
        super::mcp_p1_rig_tests::modern(MODERN, 2, "server/discover", json!({}));
    let init = legacy(
        3,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "t", "version": "1"}}),
    );
    let snapshot = |token: String| {
        let (r, list, headers, discover, dheaders, init) =
            (&r, &list, &headers, &discover, &dheaders, &init);
        async move {
            (
                r.http(Some(&token), headers, list).await.body,
                r.http(Some(&token), dheaders, discover).await.body,
                r.http(Some(&token), &[], init).await.body,
            )
        }
    };
    let pending = snapshot(r.token.clone()).await;
    assert!(pending.0.contains("list_sessions"));
    // Active.
    r.bindings.native_identity("b-1", "native-1");
    assert_eq!(r.bindings.get("b-1").unwrap().state, BindingState::Active);
    assert_eq!(snapshot(r.token.clone()).await, pending);
    // Idle expiry: the credential is refused.
    r.bindings
        .update("b-1", |b| b.state = BindingState::Expired);
    assert_eq!(r.http(Some(&r.token), &headers, &list).await.status, 401);
    // Renewal: a new credential under the same binding.
    let (token, d) = mint().unwrap();
    let renewed: Binding = {
        let mut b = binding("b-1", BindingState::Active, MAIL);
        b.generation = 2;
        b
    };
    r.bindings.insert(d, renewed);
    assert_eq!(snapshot(token.into_exposed()).await, pending);
}
