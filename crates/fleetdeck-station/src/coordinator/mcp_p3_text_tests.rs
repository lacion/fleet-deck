//! MCP P3 station tests (`docs/v2/design/mcp-p3.md` §6, tests 10 and 11): the
//! prompts and the generated instructions. The rig and the helpers are those
//! of `mcp_p3_tests.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::bindings::{BindingState, ToolClass, mint};
use super::mcp_fleet;
use super::mcp_p1_rig_tests::{MODERN, Rig, binding, legacy, modern};
use super::mcp_p3_tests::{
    FULL, MAIL_ONLY, MAIL_READ_SPAWN, OWN, SPAWN_MANAGE, SPAWN_ONLY, make_fleet_only, prompt_names,
    set, ways,
};
use super::tools;

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
