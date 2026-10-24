//! The workflow page and the chat of a workflow agent over loopback HTTPS
//! (B12, `docs/v2/design/workflow-agents.md` 4.5): the owner and the members
//! of the session read the page, only the owner reads the chat of an agent,
//! the link feature check, the cursor and id checks, the pass-through of the
//! answer and the audit rows. The station is a fake that answers the link
//! methods itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::*;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use share_support::*;

const OLDER_STATION: &str = "This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service start`.";

fn page_json() -> Value {
    json!({
        "as_of": 12,
        "workflows": [{
            "run_order": 3,
            "state": {
                "task_id": "t1",
                "name": "review",
                "run_id": "wf_abc",
                "status": "running",
                "phases": [{"index": 0, "title": "scan"}],
                "agents": [{"index": 0, "label": "check", "state": "running",
                            "agent_id": "a1b2"}],
                "running_agents": 1,
                "agents_total": 1,
            },
        }],
        "next": {"run_order": 3, "agent_index": 0},
    })
}

fn chat_json() -> Value {
    json!({
        "items": [{"id": "i1", "at": 1000, "kind": "user", "text": "work", "tool": null}],
        "state": {"kind": "complete"},
    })
}

fn workflows_path(st: &str, session: &str) -> String {
    format!("/api/sessions/{st}/{session}/workflows")
}

fn agent_path(st: &str, session: &str, agent: &str) -> String {
    format!("/api/sessions/{st}/{session}/workflow-agents/{agent}")
}

/// One GET that the fake station answers with `outcome`.
async fn read_with(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    path: &str,
    outcome: Outcome,
) -> (Http, InboundRequest) {
    let call = get(c, h, path);
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, outcome).await.unwrap();
        req
    };
    tokio::join!(call, answer)
}

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

/// A viewer of `s1`.
async fn viewer(c: &BoardClient, h: &Headers, st: &str, name: &str) -> Headers {
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    join(c, h, share, name).await.0
}

#[tokio::test]
async fn a_member_or_token_gets_owner_only_for_the_agent_chat() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let path = agent_path(&st, "s1", "a1b2");
    let member = viewer(&c, &h, &st, "Alice").await;
    let r = get(&c, &member, &path).await;
    expect(&r, 403, "owner_only");
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c.get(&path, &[("Authorization", auth.as_str())]).await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_member_of_the_shared_session_reads_the_workflow_page() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let alice = viewer(&c, &h, &st, "Alice").await;
    let (r, req) = read_with(
        &c,
        &alice,
        &mut station,
        &workflows_path(&st, "s1"),
        Outcome::Ok(page_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, page_json());
    assert_eq!(req.method, "session.workflows");
    assert_eq!(req.params, json!({ "session_id": "s1" }));
}

#[tokio::test]
async fn a_member_without_the_session_does_not_read_the_page() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let alice = viewer(&c, &h, &st, "Alice").await;
    // The share covers `s1` only: the answer is the one of `session_of`.
    let r = get(&c, &alice, &workflows_path(&st, "s2")).await;
    expect(&r, 404, "unknown_session");
    let r = get(&c, &alice, &workflows_path(&st, "nope")).await;
    expect(&r, 404, "unknown_session");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_bad_agent_id_is_invalid_params() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let long = "a".repeat(65);
    for agent in ["A1", "a-b", "a_b", "a.b", "a%20b", long.as_str()] {
        let r = get(&c, &h, &agent_path(&st, "s1", agent)).await;
        expect(&r, 400, "invalid_params");
        assert_eq!(r.body["message"], "the agent id is not valid", "{agent}");
    }
    let r = get(&c, &h, &agent_path(&st, &"s".repeat(129), "a1b2")).await;
    expect(&r, 400, "invalid_params");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_half_cursor_is_invalid_params() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let base = workflows_path(&st, "s1");
    for query in [
        "after_order=3",
        "after_index=0",
        "after_order=x&after_index=0",
        "after_order=3&after_index=-1",
        "after_order=3&after_index=4294967296",
        "after_order=3&after_index=0&other=1",
    ] {
        let r = get(&c, &h, &format!("{base}?{query}")).await;
        expect(&r, 400, "invalid_params");
        assert_eq!(r.body["message"], "the page cursor is not valid", "{query}");
    }
    let r = get(&c, &h, &workflows_path(&st, &"s".repeat(129))).await;
    expect(&r, 400, "invalid_params");
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_older_station_gets_409_workflows_unsupported_without_a_forward() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    f.store.set_link_features(&f.station, 98, vec![]);
    for path in [workflows_path(&st, "s1"), agent_path(&st, "s1", "a1b2")] {
        let r = get(&c, &h, &path).await;
        expect(&r, 409, "workflows_unsupported");
        assert_eq!(r.body["message"], OLDER_STATION);
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_answer_of_the_station_is_passed_through() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();

    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &workflows_path(&st, "s1"),
        Outcome::Ok(page_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, page_json());
    assert_eq!(req.method, "session.workflows");
    assert_eq!(req.params, json!({ "session_id": "s1" }));

    // The cursor goes to `WorkflowsQuery.after`.
    let uri = format!("{}?after_order=3&after_index=5", workflows_path(&st, "s2"));
    let (r, req) = read_with(&c, &h, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params,
        json!({ "session_id": "s2", "after": { "run_order": 3, "agent_index": 5 } })
    );

    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &agent_path(&st, "s1", "a1b2"),
        Outcome::Ok(chat_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, chat_json());
    assert_eq!(req.method, "session.workflow_agent");
    assert_eq!(
        req.params,
        json!({ "session_id": "s1", "agent_id": "a1b2" })
    );
}

#[tokio::test]
async fn the_failure_is_audited_with_the_code_only() {
    let (f, mut station, c, h) = setup().await;
    let rows = |f: &Fixture| {
        let store = f.store.clone();
        async move {
            let mut rows = store.audit_log(100).await.unwrap();
            rows.retain(|r| r.action.starts_with("sessions.workflow"));
            rows
        }
    };
    // An owner read writes no row.
    let (r, _) = read_with(
        &c,
        &h,
        &mut station,
        &workflows_path("fake", "s1"),
        Outcome::Ok(page_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(rows(&f).await.is_empty());

    let err = WireError::new("boom", "STATION-TEXT-MUST-NOT-LEAK");
    let (r, _) = read_with(
        &c,
        &h,
        &mut station,
        &workflows_path("fake", "s1"),
        Outcome::Err(err),
    )
    .await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
    let r = get(&c, &h, &agent_path("fake", "s1", "A1")).await;
    expect(&r, 400, "invalid_params");

    let all = rows(&f).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let find = |action: &str| all.iter().find(|r| r.action == action).unwrap();
    let failed = find("sessions.workflows");
    assert_eq!(failed.result, fleetdeck_deck::store::AuditResult::Failed);
    assert_eq!(failed.detail.as_deref(), Some("station_error"));
    assert_eq!(failed.target.as_deref(), Some("fake/s1"));
    let refused = find("sessions.workflow_agent");
    assert_eq!(refused.result, fleetdeck_deck::store::AuditResult::Refused);
    assert_eq!(refused.detail.as_deref(), Some("invalid_params"));
    assert!(!format!("{all:?}").contains("STATION-TEXT-MUST-NOT-LEAK"));
}
