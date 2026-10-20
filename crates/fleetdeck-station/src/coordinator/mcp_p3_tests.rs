//! MCP P3 station tests (`docs/v2/design/mcp-p3.md` §6, tests 7 to 11 and 17):
//! the fleet view tool and the resources, the caller of a view, the errors
//! without a deck, the prompts, the generated instructions, and the mixed
//! versions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::{Value, json};

use super::bindings::{Binding, BindingState, ToolClass, mint};
use super::forward::{LinkForward, Transport};
use super::ingress::Forward;
use super::mcp_fleet;
use super::mcp_p1_rig_tests::{
    Fake, MODERN, NOT_BOUND, Rig, VERSIONS, binding, legacy, modern, tool_names, tool_text,
};
use super::tools;

const OWN: &[ToolClass] = &[ToolClass::Own];
const MAIL_ONLY: &[ToolClass] = &[ToolClass::Mail, ToolClass::Own];
const SPAWN_ONLY: &[ToolClass] = &[ToolClass::Spawn, ToolClass::Own];
const SPAWN_MANAGE: &[ToolClass] = &[ToolClass::Spawn, ToolClass::Manage, ToolClass::Own];
const MAIL_READ_SPAWN: &[ToolClass] = &[
    ToolClass::Mail,
    ToolClass::Read,
    ToolClass::Spawn,
    ToolClass::Own,
];
const FULL: &[ToolClass] = &[
    ToolClass::Read,
    ToolClass::Mail,
    ToolClass::Drive,
    ToolClass::Spawn,
    ToolClass::Inspect,
    ToolClass::Manage,
    ToolClass::Answer,
    ToolClass::Own,
];

const NO_DECK: &str = "unavailable: the deck is not connected";
const OLD_DECK: &str = "unavailable: the deck does not serve the fleet view yet";

fn set(classes: &[ToolClass]) -> BTreeSet<ToolClass> {
    classes.iter().copied().collect()
}

/// A binding without a grant, as a session without a coordinator grant gets.
fn make_fleet_only(rig: &Rig) {
    rig.bindings.update("b-1", |b| {
        b.grant_id = String::new();
        b.control_skill = "SESSION TOOLS.".to_owned();
    });
}

fn contents_text(answer: &Value) -> String {
    answer["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{answer}"))
        .to_owned()
}

fn uris(answer: &Value) -> Vec<String> {
    answer["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_owned())
        .collect()
}

fn prompt_names(answer: &Value) -> Vec<String> {
    answer["result"]["prompts"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The code of a resource that does not exist: `-32002`, and `-32602` for a
/// peer of the modern era (`rmcp` upgrades it).
fn not_found(version: &str) -> i64 {
    if version == MODERN { -32602 } else { -32002 }
}

/// The two transports and the two eras: HTTP and the bridge socket, a legacy
/// version and the modern one.
fn ways() -> Vec<(bool, &'static str)> {
    let mut out = Vec::new();
    for http in [true, false] {
        for version in [VERSIONS[1], MODERN] {
            out.push((http, version));
        }
    }
    out
}

#[tokio::test]
async fn every_binding_has_the_fleet_view_tool_and_the_four_resources() {
    for fleet_only in [false, true] {
        let r = Rig::start(BindingState::Active, OWN).await;
        for (n, (http, version)) in ways().into_iter().enumerate() {
            let label = format!("fleet_only={fleet_only} http={http} {version}");
            // A binding for each group: a binding has a rate limit.
            let id = format!("g-{n}");
            let token = r.add(&id, BindingState::Active, OWN);
            if fleet_only {
                r.bindings.update(&id, |b| {
                    b.grant_id = String::new();
                    b.control_skill = "SESSION TOOLS.".to_owned();
                });
            }
            let tools = tool_names(&r.rpc(http, &token, version, "tools/list", json!({})).await);
            assert!(
                tools.contains(&"fleet_view".to_owned()),
                "{label}: {tools:?}"
            );
            // The tool carries its read-only annotation and its schema.
            let list = r.rpc(http, &token, version, "tools/list", json!({})).await;
            let tool = list["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == "fleet_view")
                .unwrap()
                .clone();
            assert_eq!(
                tool["annotations"],
                json!({"readOnlyHint": true, "openWorldHint": false}),
                "{label}"
            );
            assert_eq!(
                tool["inputSchema"]["additionalProperties"], false,
                "{label}"
            );
            assert_eq!(
                tool["inputSchema"]["properties"]["view"]["enum"],
                json!(["stations", "projects", "sessions", "me"]),
                "{label}"
            );
            // Exactly five resources, in a fixed order.
            let listed = r
                .rpc(http, &token, version, "resources/list", json!({}))
                .await;
            assert_eq!(
                uris(&listed),
                [
                    "fleet://guide",
                    "fleet://stations",
                    "fleet://projects",
                    "fleet://sessions",
                    "fleet://me"
                ],
                "{label}"
            );
            let entries = listed["result"]["resources"].as_array().unwrap();
            assert_eq!(entries[0]["mimeType"], "text/markdown", "{label}");
            assert!(
                entries[1..]
                    .iter()
                    .all(|e| e["mimeType"] == "application/json")
            );
            let templates = r
                .rpc(http, &token, version, "resources/templates/list", json!({}))
                .await;
            assert_eq!(
                templates["result"]["resourceTemplates"],
                json!([]),
                "{label}"
            );
            if version == MODERN {
                for answer in [&listed, &templates] {
                    assert_eq!(answer["result"]["ttlMs"], 60_000, "{label}");
                    assert_eq!(answer["result"]["cacheScope"], "private", "{label}");
                }
            } else {
                assert!(listed["result"].get("ttlMs").is_none(), "{label}");
            }
            // The guide needs no deck: nothing was forwarded.
            let guide = r
                .rpc(
                    http,
                    &token,
                    version,
                    "resources/read",
                    json!({"uri": "fleet://guide"}),
                )
                .await;
            let text = contents_text(&guide);
            if fleet_only {
                assert!(text.starts_with("SESSION TOOLS. "), "{label}");
                assert!(
                    text.ends_with("ask your coordinator or the owner."),
                    "{label}"
                );
            } else {
                assert_eq!(text, "SKILL v1", "{label}");
            }
            assert_eq!(guide["result"]["contents"][0]["mimeType"], "text/markdown");
            assert_eq!(r.fake.views.lock().unwrap().len(), 0, "{label}");
            // Each view is one `fleet.view` with the compact page.
            for (name, ttl) in [
                ("stations", 30_000),
                ("projects", 30_000),
                ("sessions", 10_000),
                ("me", 10_000),
            ] {
                let read = r
                    .rpc(
                        http,
                        &token,
                        version,
                        "resources/read",
                        json!({"uri": format!("fleet://{name}")}),
                    )
                    .await;
                assert_eq!(
                    contents_text(&read),
                    r#"{"items":[],"next_cursor":null}"#,
                    "{label} {name}"
                );
                assert_eq!(
                    read["result"]["contents"][0]["mimeType"],
                    "application/json"
                );
                if version == MODERN {
                    assert_eq!(read["result"]["ttlMs"], ttl, "{label} {name}");
                    assert_eq!(read["result"]["cacheScope"], "private", "{label} {name}");
                }
            }
            let before = r.fake.views.lock().unwrap().len();
            assert_eq!(before, 4, "{label}");
            // Any other URI is not found, and sends nothing.
            for bad in [
                "fleet://other",
                "file:///etc/passwd",
                "fleet://guide?x=1",
                "fleet://me?caller=victim-1",
                "fleet://sessions?station=x",
            ] {
                let answer = r
                    .rpc(http, &token, version, "resources/read", json!({"uri": bad}))
                    .await;
                assert!(answer.get("error").is_some(), "{label} {bad}: {answer}");
                assert_eq!(
                    answer["error"]["code"],
                    not_found(version),
                    "{label} {bad}: {answer}"
                );
            }
            assert_eq!(r.fake.views.lock().unwrap().len(), before, "{label}");
            r.fake.views.lock().unwrap().clear();
        }
    }
    // The tool answers the compact page, and a deck refusal is a tool error.
    let r = Rig::start(BindingState::Active, OWN).await;
    let call = |args: Value| json!({"name": "fleet_view", "arguments": args});
    let answer = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "tools/call",
            call(json!({"view": "stations"})),
        )
        .await;
    assert_eq!(
        tool_text(&answer),
        (false, r#"{"items":[],"next_cursor":null}"#.to_owned())
    );
    *r.fake.view_answer.lock().unwrap() =
        Err("forbidden: the session is not known on this station".to_owned());
    let answer = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "tools/call",
            call(json!({"view": "me"})),
        )
        .await;
    assert_eq!(
        tool_text(&answer),
        (
            true,
            "forbidden: the session is not known on this station".to_owned()
        )
    );
    // The resource shows the same refusal as `{"error": ...}`.
    let read = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "resources/read",
            json!({"uri": "fleet://me"}),
        )
        .await;
    assert_eq!(
        contents_text(&read),
        r#"{"error":"forbidden: the session is not known on this station"}"#
    );
    // The arguments are checked before anything is sent.
    let sent = r.fake.views.lock().unwrap().len();
    for bad in [
        json!({}),
        json!({"view": "everything"}),
        json!({"view": "me", "extra": 1}),
        json!({"view": "stations", "cursor": "x".repeat(2049)}),
        json!({"view": "projects", "station": "x".repeat(129)}),
        json!({"view": "stations", "cursor": 5}),
    ] {
        let answer = r
            .rpc(true, &r.token, MODERN, "tools/call", call(bad.clone()))
            .await;
        let (error, text) = tool_text(&answer);
        assert!(error && text.starts_with("invalid_params"), "{bad}: {text}");
    }
    assert_eq!(r.fake.views.lock().unwrap().len(), sent);
}

/// The binding of a second session.
fn second(rig: &Rig, id: &str, session: &str) -> String {
    let (token, digest) = mint().unwrap();
    let mut b: Binding = binding(id, BindingState::Active, OWN);
    b.session_id = session.into();
    rig.bindings.insert(digest, b);
    token.into_exposed()
}

#[tokio::test]
async fn the_fleet_view_forwards_with_the_binding_caller_only() {
    let r = Rig::start(BindingState::Active, OWN).await;
    let token_b = second(&r, "b-2", "coord-2");
    let call = |args: Value| json!({"name": "fleet_view", "arguments": args});
    for (http, version) in ways() {
        let label = format!("http={http} {version}");
        r.fake.views.lock().unwrap().clear();
        // A binding for each group: a binding has a rate limit.
        let token = r.fresh(BindingState::Active, OWN);
        // A forged caller in the arguments is refused; no request leaves.
        let forged = r
            .rpc(
                http,
                &token,
                version,
                "tools/call",
                call(json!({"view": "stations", "caller": "victim-1", "session_id": "victim-1"})),
            )
            .await;
        let (error, text) = tool_text(&forged);
        assert!(
            error && text.starts_with("invalid_params"),
            "{label}: {text}"
        );
        assert_eq!(r.fake.views.lock().unwrap().len(), 0, "{label}");
        // A forged `_meta` and `clientInfo` change nothing.
        let mut params = call(json!({"view": "sessions"}));
        if version == MODERN {
            params["_meta"] = json!({"caller": "victim-1", "session_id": "victim-1",
                "io.modelcontextprotocol/clientInfo": {"name": "victim-1", "version": "1"}});
        } else {
            params["_meta"] = json!({"caller": "victim-1", "session_id": "victim-1"});
        }
        let answer = r.rpc(http, &token, version, "tools/call", params).await;
        assert!(!tool_text(&answer).0, "{label}: {answer}");
        // A forged session in a resource URI is not a resource.
        for uri in [
            "fleet://sessions?caller=victim-1",
            "fleet://sessions?session_id=victim-1",
        ] {
            let read = r
                .rpc(http, &token, version, "resources/read", json!({"uri": uri}))
                .await;
            assert_eq!(read["error"]["code"], not_found(version), "{label} {uri}");
        }
        let calls = r.fake.views.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{label}");
        assert_eq!(calls[0].0.session_id.as_str(), "coord-1", "{label}");
        assert!(calls[0].0.binding_id.starts_with("fresh-"), "{label}");
    }
    // The cursor and the station filter pass through; nothing else does.
    r.fake.views.lock().unwrap().clear();
    let answer = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "tools/call",
            call(json!({"view": "projects", "cursor": "abc", "station": "st-1"})),
        )
        .await;
    assert!(!tool_text(&answer).0);
    r.rpc(
        true,
        &r.token,
        MODERN,
        "resources/read",
        json!({"uri": "fleet://projects?station=st-2&cursor=def"}),
    )
    .await;
    let calls = r.fake.views.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].2.as_deref(), Some("abc"));
    assert_eq!(calls[0].3.as_ref().map(|s| s.as_str()), Some("st-1"));
    assert_eq!(calls[1].2.as_deref(), Some("def"));
    assert_eq!(calls[1].3.as_ref().map(|s| s.as_str()), Some("st-2"));
    // Parallel bindings of two sessions stay isolated.
    r.fake.views.lock().unwrap().clear();
    let (a, b) = tokio::join!(
        r.rpc(
            true,
            &r.token,
            MODERN,
            "tools/call",
            call(json!({"view": "me"}))
        ),
        r.rpc(
            true,
            &token_b,
            MODERN,
            "tools/call",
            call(json!({"view": "me"}))
        ),
    );
    assert!(!tool_text(&a).0 && !tool_text(&b).0);
    let mut seen: Vec<(String, String)> = r
        .fake
        .views
        .lock()
        .unwrap()
        .iter()
        .map(|c| (c.0.session_id.to_string(), c.0.binding_id.clone()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("coord-1".to_owned(), "b-1".to_owned()),
            ("coord-2".to_owned(), "b-2".to_owned())
        ]
    );
    // A missing, wrong or revoked credential gets the uniform 401 before any
    // read.
    r.fake.views.lock().unwrap().clear();
    let (body, headers) = modern(MODERN, 1, "tools/call", call(json!({"view": "stations"})));
    assert_eq!(r.http(None, &headers, &body).await.status, 401);
    assert_eq!(r.http(Some("fdc1_nope"), &headers, &body).await.status, 401);
    let revoked = second(&r, "b-3", "coord-3");
    r.bindings
        .update("b-3", |b| b.state = BindingState::Revoked);
    let first = r.http(Some(&revoked), &headers, &body).await;
    let missing = r.http(None, &headers, &body).await;
    assert_eq!((first.status, &first.body), (401, &missing.body));
    let (body, headers) = modern(
        MODERN,
        2,
        "resources/read",
        json!({"uri": "fleet://stations"}),
    );
    assert_eq!(r.http(None, &headers, &body).await.status, 401);
    assert_eq!(r.http(Some(&revoked), &headers, &body).await.status, 401);
    assert_eq!(r.fake.views.lock().unwrap().len(), 0);
}

/// A deck transport that records the requests and answers a fixed outcome.
struct Deck {
    sent: Mutex<Vec<(String, Value)>>,
    feature: bool,
    outcome: Mutex<Outcome>,
}

impl Deck {
    fn new(feature: bool, outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::default(),
            feature,
            outcome: Mutex::new(outcome),
        })
    }
}

#[async_trait]
impl Transport for Deck {
    async fn request(
        &self,
        method: &str,
        _command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        self.sent.lock().unwrap().push((method.to_owned(), params));
        Ok(self.outcome.lock().unwrap().clone())
    }

    fn has_feature(&self, name: &str) -> bool {
        self.feature && name == feature::FLEET_VIEW
    }
}

/// A rig whose forwarder is a [`LinkForward`] with `deck` connected (or no
/// deck at all).
async fn linked(deck: Option<Arc<Deck>>, state: BindingState) -> Rig {
    Rig::start_with(
        move |_| {
            let forward = LinkForward::default();
            if let Some(deck) = deck {
                forward.connect_transport(deck, IncarnationId::from("deck-inc".to_owned()));
            }
            Arc::new(forward) as Arc<dyn Forward>
        },
        Fake::new(),
        state,
        OWN,
    )
    .await
}

async fn view_answers(r: &Rig, http: bool, version: &str) -> (String, String) {
    let call = r
        .rpc(
            http,
            &r.token,
            version,
            "tools/call",
            json!({"name": "fleet_view", "arguments": {"view": "stations"}}),
        )
        .await;
    let (error, tool) = tool_text(&call);
    assert!(error, "{call}");
    let read = r
        .rpc(
            http,
            &r.token,
            version,
            "resources/read",
            json!({"uri": "fleet://stations"}),
        )
        .await;
    (tool, contents_text(&read))
}

#[tokio::test]
async fn a_pending_binding_no_deck_or_an_old_deck_reads_an_error_and_sends_nothing() {
    // A pending binding: `not_bound`, for the tool and for the resource.
    let deck = Deck::new(true, Outcome::Ok(json!({"items": [], "next_cursor": null})));
    let r = linked(Some(deck.clone()), BindingState::Pending).await;
    for (http, version) in ways() {
        let (tool, resource) = view_answers(&r, http, version).await;
        assert_eq!(tool, NOT_BOUND);
        assert_eq!(resource, json!({"error": NOT_BOUND}).to_string());
        // The guide is served in any state.
        let guide = r
            .rpc(
                http,
                &r.token,
                version,
                "resources/read",
                json!({"uri": "fleet://guide"}),
            )
            .await;
        assert_eq!(contents_text(&guide), "SKILL v1");
    }
    assert!(deck.sent.lock().unwrap().is_empty());

    // No deck link.
    let r = linked(None, BindingState::Active).await;
    for (http, version) in ways() {
        let (tool, resource) = view_answers(&r, http, version).await;
        assert_eq!(tool, NO_DECK);
        assert_eq!(resource, json!({"error": NO_DECK}).to_string());
    }

    // A deck link without `fleet_view.v1`: zero `fleet.view` requests.
    let deck = Deck::new(
        false,
        Outcome::Ok(json!({"items": [], "next_cursor": null})),
    );
    let r = linked(Some(deck.clone()), BindingState::Active).await;
    for (http, version) in ways() {
        let (tool, resource) = view_answers(&r, http, version).await;
        assert_eq!(tool, OLD_DECK);
        assert_eq!(resource, json!({"error": OLD_DECK}).to_string());
    }
    assert!(deck.sent.lock().unwrap().is_empty());

    // A deck that serves it: one request per read with the binding caller;
    // a refusal is `<code>: <text>`.
    let deck = Deck::new(
        true,
        Outcome::Err(WireError::new(
            "rate_limited",
            "at most 60 fleet views per minute",
        )),
    );
    let r = linked(Some(deck.clone()), BindingState::Active).await;
    let (tool, resource) = view_answers(&r, true, MODERN).await;
    assert_eq!(tool, "rate_limited: at most 60 fleet views per minute");
    assert_eq!(
        resource,
        json!({"error": "rate_limited: at most 60 fleet views per minute"}).to_string()
    );
    let sent = deck.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    for (method, params) in &sent {
        assert_eq!(method, "fleet.view");
        assert_eq!(
            params,
            &json!({"caller": "coord-1", "view": "stations",
                                   "cursor": null, "station": null})
        );
    }
}

/// The prompts that a catalog lists.
fn listed(classes: &[ToolClass]) -> Vec<String> {
    let catalog: BTreeSet<String> = tools::catalog(&set(classes))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    mcp_fleet::prompts(&catalog)
        .into_iter()
        .map(|p| p.name)
        .collect()
}

#[tokio::test]
async fn prompts_are_listed_only_with_every_tool_that_they_name() {
    let report = || vec!["report".to_owned()];
    let all = || {
        vec![
            "report".to_owned(),
            "validate_on_platform".to_owned(),
            "delegate".to_owned(),
        ]
    };
    assert_eq!(listed(OWN), report());
    assert_eq!(listed(MAIL_ONLY), report());
    assert_eq!(listed(SPAWN_ONLY), report());
    assert_eq!(listed(SPAWN_MANAGE), report());
    assert_eq!(listed(MAIL_READ_SPAWN), all());
    assert_eq!(listed(FULL), all());
    // `delegate` does not need a spawn tool; `validate_on_platform` does.
    assert_eq!(
        listed(&[ToolClass::Mail, ToolClass::Read, ToolClass::Own]),
        ["report", "delegate"]
    );

    // Over the wire, in both eras and on both transports.
    for classes in [OWN, MAIL_READ_SPAWN] {
        let r = Rig::start(BindingState::Active, classes).await;
        for (http, version) in ways() {
            let answer = r
                .rpc(http, &r.token, version, "prompts/list", json!({}))
                .await;
            assert_eq!(prompt_names(&answer), listed(classes), "{http} {version}");
            if version == MODERN {
                assert_eq!(answer["result"]["ttlMs"], 60_000);
                assert_eq!(answer["result"]["cacheScope"], "private");
            }
            // An unlisted prompt is `unknown prompt`.
            for name in ["validate_on_platform", "delegate", "nope"] {
                let got = r
                    .rpc(
                        http,
                        &r.token,
                        version,
                        "prompts/get",
                        json!({"name": name, "arguments": {"platform": "macos", "task": "t"}}),
                    )
                    .await;
                let listed_here = listed(classes).contains(&name.to_owned());
                if listed_here {
                    assert!(got.get("result").is_some(), "{name}: {got}");
                } else {
                    assert_eq!(got["error"]["code"], -32602, "{name}: {got}");
                    assert_eq!(got["error"]["message"], "unknown prompt", "{name}: {got}");
                }
            }
        }
    }

    // The texts of §3.2.
    let r = Rig::start(BindingState::Active, MAIL_READ_SPAWN).await;
    let text = |answer: &Value| -> String {
        let message = &answer["result"]["messages"][0];
        assert_eq!(message["role"], "user", "{answer}");
        message["content"]["text"].as_str().unwrap().to_owned()
    };
    let get = |name: &'static str, args: Value| {
        let r = &r;
        async move {
            r.rpc(
                true,
                &r.token,
                MODERN,
                "prompts/get",
                json!({"name": name, "arguments": args}),
            )
            .await
        }
    };
    assert_eq!(
        text(&get("report", json!({})).await),
        "Write a short report of what you did and what is open. Publish it with publish_artifact (name report.md), then set your card line with set_focus."
    );
    assert_eq!(
        text(&get("validate_on_platform", json!({"platform": "macos"})).await),
        "Validate the current work on macos. Call fleet_view with view stations and pick an online station with platform macos. Start one session there with spawn, assign it the validation task with assign, wait with wait_tasks, then read its report and tell me the result. If no such station is online, say so and stop."
    );
    assert_eq!(
        text(&get("delegate", json!({"task": "fix it"})).await),
        "Give this task to another session: fix it. Call fleet_view with view sessions and pick an idle session, then assign the task with assign (or use assign_auto). Wait with wait_tasks and report the result."
    );
    assert_eq!(
        text(&get("delegate", json!({"task": "fix it", "station": "alpha"})).await),
        "Give this task to another session: fix it. Call fleet_view with view sessions and pick an idle session on station alpha, then assign the task with assign (or use assign_auto). Wait with wait_tasks and report the result."
    );
    // A missing or non-string argument is `invalid_params`.
    for (name, args) in [
        ("validate_on_platform", json!({})),
        ("validate_on_platform", json!({"platform": 5})),
        ("delegate", json!({"station": "alpha"})),
        ("delegate", json!({"task": ""})),
    ] {
        let got = get(name, args.clone()).await;
        assert_eq!(got["error"]["code"], -32602, "{name} {args}: {got}");
    }
    // The starter follows the catalog; `assign_auto` only when it is there.
    let names =
        |tools: &[&str]| -> BTreeSet<String> { tools.iter().map(|t| (*t).to_owned()).collect() };
    let both = names(&[
        "fleet_view",
        "assign",
        "wait_tasks",
        "spawn",
        "start_in_project",
    ]);
    let got = mcp_fleet::prompt(
        "validate_on_platform",
        &json!({"platform": "linux"}).as_object().unwrap().clone(),
        &both,
    )
    .unwrap();
    let text = serde_json::to_value(&got).unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains("with start_in_project (or spawn when there is no project), assign"));
    let only = names(&["fleet_view", "assign", "wait_tasks", "start_in_project"]);
    let got = mcp_fleet::prompt(
        "validate_on_platform",
        &json!({"platform": "linux"}).as_object().unwrap().clone(),
        &only,
    )
    .unwrap();
    let text = serde_json::to_value(&got).unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains("with start_in_project, assign"), "{text}");
    let none = names(&["fleet_view", "assign", "wait_tasks"]);
    let got = mcp_fleet::prompt(
        "delegate",
        &json!({"task": "t"}).as_object().unwrap().clone(),
        &none,
    )
    .unwrap();
    let text = serde_json::to_value(&got).unwrap()["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!text.contains("assign_auto"), "{text}");
}

/// The words of `text` that name a tool.
fn tool_words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c == '_'))
        .filter(|w| tools::tool(w).is_some())
        .map(str::to_owned)
        .collect()
}

fn instructions_of(answer: &Value) -> String {
    answer["result"]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("{answer}"))
        .to_owned()
}

const PREAMBLE: &str = "Never ask for, print or pass on a credential. A refused call (forbidden, expired, revoked) is final: do not retry it in a loop; tell the owner what you need. Use only the tools in your tool list.";

#[tokio::test]
async fn the_instructions_follow_the_catalog_and_always_carry_the_safety_preamble() {
    for classes in [
        OWN,
        MAIL_ONLY,
        SPAWN_ONLY,
        SPAWN_MANAGE,
        MAIL_READ_SPAWN,
        FULL,
    ] {
        let catalog: BTreeSet<String> = tools::catalog(&set(classes))["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let r = Rig::start(BindingState::Pending, classes).await;
        r.bindings
            .set_station("alpha".to_owned(), "omarchy".to_owned());
        let init = |version: &str| {
            legacy(
                1,
                "initialize",
                json!({"protocolVersion": version, "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}}),
            )
        };
        let (discover, dheaders) = modern(MODERN, 2, "server/discover", json!({}));
        let mut seen = Vec::new();
        // Pending, active, and expired-and-renewed.
        let mut token = r.token.clone();
        for state in ["pending", "active", "renewed"] {
            match state {
                "active" => {
                    r.bindings.native_identity("b-1", "native-1");
                }
                "renewed" => {
                    r.bindings
                        .update("b-1", |b| b.state = BindingState::Expired);
                    let (fresh, d) = mint().unwrap();
                    let mut b = binding("b-1", BindingState::Active, classes);
                    b.generation = 2;
                    r.bindings.insert(d, b);
                    token = fresh.into_exposed();
                }
                _ => {}
            }
            let legacy_text =
                instructions_of(&r.http(Some(&token), &[], &init("2025-11-25")).await.json());
            let modern_text =
                instructions_of(&r.http(Some(&token), &dheaders, &discover).await.json());
            for text in [&legacy_text, &modern_text] {
                assert!(
                    text.starts_with("You are a Fleet Deck session on station alpha (omarchy). "),
                    "{state}: {text}"
                );
                assert!(text.contains(PREAMBLE), "{state}: {text}");
                assert!(
                    text.contains("Call fleet_view to see the stations and their platforms"),
                    "{text}"
                );
                // Every tool name in the text is in the catalog of the binding.
                for word in tool_words(text) {
                    assert!(
                        catalog.contains(&word),
                        "{state}: `{word}` is not granted: {text}"
                    );
                }
                // Line 3, line 6 and line 7 follow the catalog and the grant.
                let has = |name: &str| catalog.contains(name);
                assert_eq!(text.contains("command_status"), has("command_status"));
                assert!(text.contains("Read fleet://guide"), "{text}");
                let verbs = text.contains("You may ");
                let any = ["spawn", "start_in_project", "assign", "wait_tasks", "stop"]
                    .iter()
                    .any(|t| has(t));
                assert_eq!(verbs, any, "{text}");
                assert_eq!(
                    text.contains("To get help from another station, ask your coordinator with ask_coordinator"),
                    !any,
                    "{text}"
                );
            }
            assert_eq!(legacy_text, modern_text, "{state}");
            seen.push(legacy_text);
        }
        assert_eq!(seen[0], seen[1]);
        assert_eq!(seen[1], seen[2]);
    }

    // A binding without a grant has no guide line and no station name
    // without recorded facts.
    let r = Rig::start(BindingState::Active, OWN).await;
    make_fleet_only(&r);
    let text = instructions_of(
        &r.http(
            Some(&r.token),
            &[],
            &legacy(
                1,
                "initialize",
                json!({"protocolVersion": "2025-11-25", "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}}),
            ),
        )
        .await
        .json(),
    );
    assert!(
        text.starts_with("You are a Fleet Deck session. Call fleet_view"),
        "{text}"
    );
    assert!(!text.contains("fleet://guide"), "{text}");
    assert!(text.contains(PREAMBLE));
    assert!(text.contains("To get help from another station"));
    // The verbs follow the catalog exactly.
    let full = Rig::start(BindingState::Active, FULL).await;
    let text = instructions_of(
        &full
            .http(
                Some(&full.token),
                &[],
                &legacy(
                    1,
                    "initialize",
                    json!({"protocolVersion": "2025-11-25", "capabilities": {},
                           "clientInfo": {"name": "t", "version": "1"}}),
                ),
            )
            .await
            .json(),
    );
    assert!(
        text.contains(
            "You may start sessions with spawn or start_in_project, give work with assign and wait with wait_tasks, stop the sessions that you started with stop."
        ),
        "{text}"
    );
    assert!(
        text.contains("A call that answers pending is not finished: check it with command_status.")
    );
}

#[tokio::test]
async fn an_n_minus_1_deck_and_an_n_minus_1_station_degrade_cleanly() {
    // A forwarder that does not know the fleet view (the trait default).
    struct Plain;
    #[async_trait]
    impl Forward for Plain {
        async fn call(
            &self,
            _caller: super::bindings::Caller,
            _tool: &str,
            _args: Value,
        ) -> Result<Value, String> {
            Err("no".to_owned())
        }
    }
    let r = Rig::start_with(
        |_| Arc::new(Plain) as Arc<dyn Forward>,
        Fake::new(),
        BindingState::Active,
        OWN,
    )
    .await;
    for (http, version) in ways() {
        let (tool, resource) = view_answers(&r, http, version).await;
        assert_eq!(tool, OLD_DECK);
        assert_eq!(resource, json!({"error": OLD_DECK}).to_string());
    }
    // A deck without the feature: the same answers, nothing is sent, and a
    // later link that has the feature starts to be asked.
    let old = Deck::new(
        false,
        Outcome::Ok(json!({"items": [1], "next_cursor": null})),
    );
    let r = linked(Some(old.clone()), BindingState::Active).await;
    for (http, version) in ways() {
        let (tool, resource) = view_answers(&r, http, version).await;
        assert_eq!(tool, OLD_DECK, "{http} {version}");
        assert_eq!(resource, json!({"error": OLD_DECK}).to_string());
    }
    assert!(old.sent.lock().unwrap().is_empty());
    // The other tools and the guide still work against the old deck.
    let tools = tool_names(&r.rpc(true, &r.token, MODERN, "tools/list", json!({})).await);
    assert!(tools.contains(&"fleet_view".to_owned()));
    let guide = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "resources/read",
            json!({"uri": "fleet://guide"}),
        )
        .await;
    assert_eq!(contents_text(&guide), "SKILL v1");
    // A deck that serves the view answers a page that has the two keys.
    let page = json!({"items": [{"name": "alpha"}], "next_cursor": "c", "extra": 1});
    let deck = Deck::new(true, Outcome::Ok(page));
    let r = linked(Some(deck.clone()), BindingState::Active).await;
    let read = r
        .rpc(
            true,
            &r.token,
            MODERN,
            "resources/read",
            json!({"uri": "fleet://stations"}),
        )
        .await;
    assert_eq!(
        contents_text(&read),
        r#"{"items":[{"name":"alpha"}],"next_cursor":"c"}"#
    );
    // An answer that is not a page is an internal error text.
    *deck.outcome.lock().unwrap() = Outcome::Ok(json!("not a page"));
    let (tool, _) = view_answers(&r, true, MODERN).await;
    assert!(tool.starts_with("internal:"), "{tool}");
}
