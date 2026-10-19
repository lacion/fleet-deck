//! MCP P1 protocol-core tests (`docs/v2/design/mcp-p1.md` §6): the tests 1 to
//! 5, 7, 12, 14, 15 and 21, and the cache hints of both eras.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{Value, json};

use super::bindings::{BindingState, ToolClass};
use super::desktop_forward::McpToolResult;
use super::ingress::Forward;
use super::mcp_p1_rig_tests::{
    MAIL, MODERN, NOT_BOUND, Rig, SKILL, VERSIONS, binding, forbidden, http_raw, http_request,
    legacy, modern, tool_names, tool_text,
};
use super::mcp_service::{McpService, bridge_headers};

#[tokio::test]
async fn a_modern_discover_serves_the_binding_control_skill_and_the_four_versions() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let (message, headers) = modern(MODERN, 1, "server/discover", json!({}));
    let reply = r.http(Some(&r.token), &headers, &message).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let v = reply.json();
    assert_eq!(v["result"]["resultType"], "complete");
    assert_eq!(v["result"]["supportedVersions"], json!(VERSIONS));
    assert_eq!(v["result"]["instructions"], SKILL);
    assert!(v["result"]["capabilities"]["tools"].is_object());
    assert!(v["result"]["capabilities"].get("resources").is_none());
    assert_eq!(
        v["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "fleetdeck"
    );
    // The bridge socket gives the same answer.
    assert_eq!(r.sock_json(&r.token, &message).await, v);
    // Another binding gets its own skill, never this one.
    let other = r.add("b-2", BindingState::Active, MAIL);
    r.bindings
        .update("b-2", |b| b.control_skill = "OTHER".to_owned());
    let got = r.http(Some(&other), &headers, &message).await.json();
    assert_eq!(got["result"]["instructions"], "OTHER");
}

#[tokio::test]
async fn a_modern_request_with_an_unknown_version_gets_32022_and_a_legacy_initialize_gets_2025_11_25()
 {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let (message, headers) = modern("2099-01-01", 1, "tools/list", json!({}));
    let reply = r.http(Some(&r.token), &headers, &message).await;
    assert_eq!(reply.status, 400, "{}", reply.body);
    let v = reply.json();
    assert_eq!(v["error"]["code"], -32022);
    assert_eq!(v["error"]["data"]["requested"], "2099-01-01");
    assert_eq!(v["error"]["data"]["supported"], json!(VERSIONS));
    // The same on the bridge socket.
    assert_eq!(
        r.sock_json(&r.token, &message).await["error"]["code"],
        -32022
    );
    // A legacy `initialize` with an unknown version negotiates down to the
    // newest legacy version, with the control skill.
    for version in ["2099-01-01", "2024-11-05", MODERN] {
        let init = legacy(
            1,
            "initialize",
            json!({"protocolVersion": version, "capabilities": {},
                   "clientInfo": {"name": "t", "version": "1"}}),
        );
        let v = r.http(Some(&r.token), &[], &init).await.json();
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25", "{version}");
        assert_eq!(v["result"]["instructions"], SKILL);
        assert_eq!(v["result"]["serverInfo"]["name"], "fleetdeck");
        assert!(v["result"]["capabilities"]["tools"].is_object());
    }
}

#[tokio::test]
async fn a_header_that_does_not_match_the_body_gets_32020() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let (message, _) = modern(
        MODERN,
        1,
        "tools/call",
        json!({"name": "list_sessions", "arguments": {}}),
    );
    let wrong_name = [
        ("MCP-Protocol-Version".to_owned(), MODERN.to_owned()),
        ("Mcp-Method".to_owned(), "tools/call".to_owned()),
        ("Mcp-Name".to_owned(), "send_mail".to_owned()),
    ];
    let wrong_method = [
        ("MCP-Protocol-Version".to_owned(), MODERN.to_owned()),
        ("Mcp-Method".to_owned(), "tools/list".to_owned()),
        ("Mcp-Name".to_owned(), "list_sessions".to_owned()),
    ];
    let wrong_version = [
        ("MCP-Protocol-Version".to_owned(), "2025-11-25".to_owned()),
        ("Mcp-Method".to_owned(), "tools/call".to_owned()),
        ("Mcp-Name".to_owned(), "list_sessions".to_owned()),
    ];
    let no_headers: [(String, String); 0] = [];
    for headers in [
        &wrong_name[..],
        &wrong_method[..],
        &wrong_version[..],
        &no_headers[..],
    ] {
        let reply = r.http(Some(&r.token), headers, &message).await;
        assert_eq!(reply.status, 400, "{headers:?} {}", reply.body);
        assert_eq!(reply.json()["error"]["code"], -32020, "{headers:?}");
    }
    assert_eq!(r.fake.forwards(), 0, "nothing reached the tool");
}

#[tokio::test]
async fn a_tool_outside_the_grant_is_refused_on_call_even_when_the_client_skips_tools_list() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let outside = [
        (
            "spawn",
            json!({"station": "s", "spec": {"provider_instance": "claude", "cwd": "/w"}}),
        ),
        ("interrupt", json!({"station": "s", "session_id": "w"})),
        (
            "notify",
            json!({"request_id": "r1", "title": "t", "body": "b"}),
        ),
        (
            "type_text",
            json!({"request_id": "r1", "window_id": "w1", "text": "x"}),
        ),
        ("no_such_tool", json!({})),
    ];
    for version in VERSIONS {
        for http in [true, false] {
            let token = r.fresh(BindingState::Active, MAIL);
            for (name, args) in &outside {
                let params = json!({"name": name, "arguments": args});
                let v = r.rpc(http, &token, version, "tools/call", params).await;
                assert_eq!(
                    tool_text(&v),
                    (true, forbidden(name)),
                    "{version} {name} {http}"
                );
            }
        }
    }
    assert_eq!(r.fake.forwards(), 0);
    assert!(r.fleet.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_pending_binding_lists_its_tools_and_every_call_is_not_bound() {
    let r = Rig::start(BindingState::Pending, MAIL).await;
    for version in VERSIONS {
        for http in [true, false] {
            let token = r.fresh(BindingState::Pending, MAIL);
            let listed = r.rpc(http, &token, version, "tools/list", json!({})).await;
            let names = tool_names(&listed);
            assert_eq!(
                names.first().map(String::as_str),
                Some("list_sessions"),
                "{version}"
            );
            assert!(names.len() > 5, "{names:?}");
            for name in &names {
                let params = json!({"name": name, "arguments": {}});
                let v = r.rpc(http, &token, version, "tools/call", params).await;
                assert_eq!(
                    tool_text(&v),
                    (true, NOT_BOUND.to_owned()),
                    "{version} {name}"
                );
            }
        }
    }
    assert_eq!(r.fake.forwards(), 0);
    assert!(r.fleet.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_tools_list_carries_cache_hints_in_the_modern_era_only() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let modern = r.rpc(true, &r.token, MODERN, "tools/list", json!({})).await;
    assert_eq!(modern["result"]["cacheScope"], "private");
    assert_eq!(modern["result"]["ttlMs"], 60_000);
    assert_eq!(modern["result"]["resultType"], "complete");
    for version in &VERSIONS[..3] {
        let v = r
            .rpc(true, &r.token, version, "tools/list", json!({}))
            .await;
        for key in ["cacheScope", "ttlMs", "resultType"] {
            assert!(v["result"].get(key).is_none(), "{version} {key}: {v}");
        }
        assert_eq!(tool_names(&v), tool_names(&modern), "{version}");
    }
}

/// A tiny check of a value against the object schemas of §4.2: the required
/// keys and the `type` of each property.
fn matches_shape(value: &Value, schema: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let required = schema["required"].as_array().unwrap();
    let properties = schema["properties"].as_object().unwrap();
    required
        .iter()
        .all(|k| object.contains_key(k.as_str().unwrap()))
        && object.iter().all(|(k, v)| {
            properties
                .get(k)
                .is_none_or(|p| match p["type"].as_str().unwrap() {
                    "string" => v.as_str().is_some_and(|s| {
                        p["maxLength"]
                            .as_u64()
                            .is_none_or(|m| s.chars().count() as u64 <= m)
                    }),
                    "integer" => v
                        .as_u64()
                        .is_some_and(|n| p["minimum"].as_u64().is_none_or(|m| n >= m)),
                    "boolean" => v.is_boolean(),
                    _ => false,
                })
        })
}

#[tokio::test]
async fn publish_artifact_and_set_focus_return_structured_content_that_matches_the_output_schema() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    for version in VERSIONS {
        let listed = r
            .rpc(true, &r.token, version, "tools/list", json!({}))
            .await;
        let schema = |name: &str| {
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap()["outputSchema"]
                .clone()
        };
        for (name, args) in [
            (
                "publish_artifact",
                json!({"name": "r.html", "content": "<p>x</p>", "title": "T"}),
            ),
            ("set_focus", json!({"text": "reading"})),
        ] {
            let params = json!({"name": name, "arguments": args});
            let v = r.rpc(true, &r.token, version, "tools/call", params).await;
            assert_eq!(v["result"]["isError"], false, "{v}");
            let structured = &v["result"]["structuredContent"];
            assert!(
                matches_shape(structured, &schema(name)),
                "{version} {name}: {v}"
            );
            // The text block keeps the same value for a client without
            // structured content.
            let text: Value =
                serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(&text, structured);
        }
    }
    // The other tools carry no structured content.
    let v = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "tools/call",
            json!({"name": "list_sessions", "arguments": {}}),
        )
        .await;
    assert!(v["result"].get("structuredContent").is_none(), "{v}");
}

#[tokio::test]
async fn the_outer_checks_still_run_before_rmcp() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let list = legacy(1, "tools/list", json!({}));
    // An Origin header is 403 before the credential and before rmcp.
    let origin = [("Origin".to_owned(), "http://evil.example".to_owned())];
    assert_eq!(r.http(Some(&r.token), &origin, &list).await.status, 403);
    assert_eq!(r.http(None, &origin, &list).await.status, 403);
    // A foreign Host is 403 as well, with or without a credential.
    for token in [None, Some(r.token.as_str())] {
        let request = http_request(r.addr(), token, &[], &list.to_string()).replace(
            &format!("Host: 127.0.0.1:{}", r.addr().port()),
            "Host: evil.example",
        );
        assert_eq!(http_raw(r.addr(), request).await.status, 403);
    }
    // A request without `Accept` is served (the station adds it).
    let served = r.http(Some(&r.token), &[], &list).await;
    assert_eq!(served.status, 200, "{}", served.body);
    assert!(!tool_names(&served.json()).is_empty());
    // A non-JSON body and a batch are refused before rmcp.
    let bad = http_request(r.addr(), Some(&r.token), &[], "not json");
    let bad = http_raw(r.addr(), bad).await;
    assert_eq!(
        (bad.status, bad.json()["error"]["code"].as_i64()),
        (400, Some(-32700))
    );
    let batch = http_request(r.addr(), Some(&r.token), &[], &json!([list]).to_string());
    let batch = http_raw(r.addr(), batch).await;
    assert_eq!((batch.status, batch.body.as_str()), (400, ""));
    // The methods whose rmcp default answers success are method-not-found.
    for method in [
        "completion/complete",
        "prompts/list",
        "resources/list",
        "resources/templates/list",
    ] {
        let params = match method {
            "completion/complete" => json!({"ref": {"type": "ref/prompt", "name": "p"},
                                            "argument": {"name": "a", "value": "v"}}),
            _ => json!({}),
        };
        for version in [VERSIONS[1], MODERN] {
            let v = r.rpc(true, &r.token, version, method, params.clone()).await;
            assert_eq!(v["error"]["code"], -32601, "{version} {method}: {v}");
        }
    }
    // `ping` is a success.
    let v = r.rpc(true, &r.token, VERSIONS[2], "ping", json!({})).await;
    assert_eq!(v["result"], json!({}));
}

#[tokio::test]
async fn many_unknown_tool_names_build_no_state_and_do_not_affect_another_binding() {
    let r = Rig::start(BindingState::Pending, MAIL).await;
    let pending = binding("b-1", BindingState::Pending, MAIL);
    let forward: Arc<dyn Forward> = r.fake.clone();
    let service = McpService::new(r.bindings.clone(), forward, None);
    for n in 0..200 {
        let name = format!("unknown_tool_{n}");
        let (message, _) = modern(
            MODERN,
            n,
            "tools/call",
            json!({"name": name, "arguments": {}}),
        );
        let reply = service
            .serve(
                pending.clone(),
                Bytes::from(message.to_string()),
                &bridge_headers(&message, None),
            )
            .await;
        let v: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(tool_text(&v), (true, forbidden(&name)));
    }
    // Another binding lists and calls as usual, over the real ingress.
    let other = r.add("b-2", BindingState::Active, MAIL);
    let listed = r.http_modern(&other, "tools/list", json!({})).await.json();
    assert_eq!(tool_names(&listed)[0], "list_sessions");
    let called = r
        .http_modern(
            &other,
            "tools/call",
            json!({"name": "list_sessions", "arguments": {}}),
        )
        .await
        .json();
    assert_eq!(tool_text(&called).0, false, "{called}");
    assert_eq!(r.fake.forwards(), 1);
}

#[tokio::test]
async fn desktop_results_carry_result_type_complete_in_the_modern_era() {
    let desktop_classes = [ToolClass::Desktop, ToolClass::Capture, ToolClass::Own];
    let r = Rig::start(BindingState::Active, &desktop_classes).await;
    r.bindings.set_fleet_control(true);
    let image = McpToolResult {
        content: vec![
            json!({"type": "text", "text": "{\"capture_id\":\"cap-1\"}"}),
            json!({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="}),
        ],
        is_error: false,
    };
    let refusal = McpToolResult {
        content: vec![json!({"type": "text", "text": "{\"error\":\"x\"}"})],
        is_error: true,
    };
    let broken = McpToolResult {
        content: vec![json!({"type": "no_such_block"})],
        is_error: false,
    };
    let call = json!({"name": "desktop_info", "arguments": {}});
    for (result, is_error) in [(image, false), (refusal, true), (broken, true)] {
        *r.fake.desktop_result.lock().unwrap() = result;
        for http in [true, false] {
            for version in VERSIONS {
                let token = r.fresh(BindingState::Active, &desktop_classes);
                let v = r
                    .rpc(http, &token, version, "tools/call", call.clone())
                    .await;
                assert_eq!(v["result"]["isError"], is_error, "{version}: {v}");
                let has = v["result"].get("resultType").is_some();
                assert_eq!(has, version == MODERN, "{version} {http}: {v}");
                if has {
                    assert_eq!(v["result"]["resultType"], "complete");
                }
                if v["result"]["content"][0]["text"]
                    == "internal: the desktop result could not be encoded"
                {
                    assert_eq!(v["result"]["content"].as_array().unwrap().len(), 1);
                }
            }
        }
    }
    // The unconvertible result is the fixed tool error.
    let v = r.rpc(true, &r.token, MODERN, "tools/call", call).await;
    assert_eq!(
        tool_text(&v),
        (
            true,
            "internal: the desktop result could not be encoded".to_owned()
        )
    );
}

#[tokio::test]
async fn two_identical_publish_calls_without_artifact_id_make_two_artifacts() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let params = json!({"name": "publish_artifact",
                        "arguments": {"name": "r.html", "content": "<p>x</p>", "title": "T"}});
    let mut ids = Vec::new();
    for _ in 0..2 {
        let v = r
            .rpc(true, &r.token, MODERN, "tools/call", params.clone())
            .await;
        assert_eq!(v["result"]["isError"], false, "{v}");
        assert_eq!(v["result"]["structuredContent"]["created"], true);
        ids.push(v["result"]["structuredContent"]["artifact_id"].clone());
    }
    assert_ne!(ids[0], ids[1], "the station does not merge the two calls");
    assert_eq!(r.fleet.calls.lock().unwrap().len(), 2);
}
