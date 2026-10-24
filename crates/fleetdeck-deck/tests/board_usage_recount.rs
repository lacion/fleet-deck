//! The usage recount (B20, `docs/v2/design/usage-cost.md` §4.3) over loopback
//! HTTPS: `session.usage_recount` goes through the generic command route.
//! Only the owner's board login sends it, and only to a station that
//! negotiated `usage_recount.v1`. The audit rows of the method hold the
//! session target and a stage: `submitted` or `refused` from the route, and
//! one `outcome` row from a watcher. The station is a fake that answers by
//! hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_deck::http::execution_routes::StopWatch;
use fleetdeck_deck::store::{AuditEntry, AuditResult};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::command::{Receipt, ReceiptState, Reject};
use fleetdeck_proto::event::{Event, EventBody, Usage, UsageSource};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::{CommandId, EventEnvelope, RejectCode, SessionSummary};
use serde_json::{Value, json};
use settings_support::*;

const DAY: i64 = 24 * 60 * 60 * 1000;
const RECOUNT: &str = "/api/commands/session.usage_recount";

struct T {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
}

/// A deck with one session `s1` and a watcher that gives up after `limit`.
async fn setup(limit: Duration) -> T {
    let mut f = fixture().await;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![SessionSummary::new("s1".into())],
        )
        .await
        .unwrap();
    let extras = Extras {
        stop_watch: Some(StopWatch {
            poll: Duration::from_millis(20),
            limit,
        }),
        ..Extras::default()
    };
    let c = board_with_extras(Arc::clone(&f.ctx), extras).await;
    set_password(&c).await;
    let h = owner(&c).await;
    T { f, station, c, h }
}

async fn admin_token(c: &BoardClient) -> Headers {
    let scopes = BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]);
    let (_, t) = c
        .store
        .create_token("ops".into(), scopes, DAY)
        .await
        .unwrap();
    vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))]
}

fn body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": { "op": "usage_recount", "session_id": "s1" },
    })
}

/// The `session.usage_recount` rows, newest first.
async fn rows(c: &BoardClient) -> Vec<AuditEntry> {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "session.usage_recount")
        .collect()
}

/// The rows whose detail starts with `stage`.
async fn stage_rows(c: &BoardClient, stage: &str) -> Vec<AuditEntry> {
    rows(c)
        .await
        .into_iter()
        .filter(|e| {
            e.detail
                .as_deref()
                .is_some_and(|d| d.split(' ').next() == Some(&format!("stage={stage}")))
        })
        .collect()
}

/// Waits (at most 3 s) until a row of `stage` exists.
async fn wait_stage(c: &BoardClient, stage: &str) {
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while stage_rows(c, stage).await.is_empty() {
        assert!(
            std::time::Instant::now() < until,
            "no `{stage}` row in 3 s: {:?}",
            rows(c).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Answers the next request (the recount command) with `answer(id)`.
async fn answer_with(station: &mut LiveLink, answer: fn(&CommandId) -> Outcome) -> InboundRequest {
    let req = next_request(station).await;
    let id = req.command_id.clone().expect("a command id");
    station.control.respond(req.id, answer(&id)).await.unwrap();
    req
}

/// A receipt that rejects the command with a message of the form
/// `<code>: <text>`.
fn refusal(id: &CommandId) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Rejected,
        station_seq: None,
        result: None,
        reject: Some(Reject::new(
            RejectCode::InvalidState,
            "session_running: This session may still run (a live process)."
        )),
        at: 0,
    }))
}

/// The `usage.set` event that the station writes for the command `id`.
fn usage_set(id: &str) -> EventEnvelope {
    let usage = Usage {
        input_tokens: Some(120),
        output_tokens: Some(30),
        cost_usd: Some(0.5),
        scheme: Some(2),
        tokens_partial: true,
        cost_partial: true,
        ..Usage::default()
    };
    EventEnvelope {
        station_seq: 1,
        session_id: "s1".into(),
        session_seq: 1,
        at: 5_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::UsageSet {
            usage,
            previous: Box::new(Usage {
                input_tokens: Some(9_999),
                ..Usage::default()
            }),
            source: UsageSource::ClaudeTranscript,
            command_id: CommandId::from(id),
        }),
    }
}

#[tokio::test]
async fn a_token_cannot_recount() {
    let mut t = setup(Duration::from_millis(200)).await;
    let admin = admin_token(&t.c).await;
    let r = post(&t.c, &admin, RECOUNT, body("rc-t1")).await;
    expect(&r, 403, "owner_only");
    let rows = rows(&t.c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Refused);
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("{}/s1", t.f.station.0).as_str())
    );
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=refused code=owner_only")
    );
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "rc-t1");
    assert!(t.f.store.commands().await.unwrap().is_empty());
    no_request(&mut t.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_station_without_the_feature_answers_not_supported() {
    let mut t = setup(Duration::from_millis(200)).await;
    t.f.store.set_link_features(&t.f.station, 98, vec![]);
    let r = post(&t.c, &t.h, RECOUNT, body("rc-n1")).await;
    expect(&r, 409, "not_supported");
    assert_eq!(r.body["message"], "this station cannot recount usage");
    let rows = rows(&t.c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Refused);
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("{}/s1", t.f.station.0).as_str())
    );
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=refused code=not_supported")
    );
    assert!(t.f.store.commands().await.unwrap().is_empty());
    no_request(&mut t.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_owner_recount_is_audited_and_folded() {
    let mut t = setup(Duration::from_secs(5)).await;
    let call = post(&t.c, &t.h, RECOUNT, body("rc-o1"));
    let (r, req) = tokio::join!(call, answer_with(&mut t.station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "accepted");
    assert_eq!(req.method, "session.usage_recount");
    assert_eq!(
        req.params["command"],
        json!({ "op": "usage_recount", "session_id": "s1" })
    );
    // The station writes the `usage.set` event; the deck folds it.
    t.f.store
        .apply_events(
            t.f.station.clone(),
            t.f.inc.clone(),
            vec![usage_set("rc-o1")],
        )
        .await
        .unwrap();
    let sessions = t.f.store.sessions().await.unwrap();
    let summary = &sessions
        .iter()
        .find(|s| s.summary.session_id.as_str() == "s1")
        .expect("session s1")
        .summary;
    assert_eq!(summary.usage.input_tokens, Some(120));
    assert_eq!(summary.usage.scheme, Some(2));
    let recount = summary.usage_recount.as_ref().expect("a recount");
    assert_eq!(recount.command_id.as_str(), "rc-o1");
    assert_eq!(recount.source, UsageSource::ClaudeTranscript);
    assert_eq!(recount.previous.input_tokens, Some(9_999));
    // One `submitted` row and one `outcome` row, both with the session target.
    wait_stage(&t.c, "outcome").await;
    let target = format!("{}/s1", t.f.station.0);
    let submitted = stage_rows(&t.c, "submitted").await;
    assert_eq!(submitted.len(), 1, "{submitted:?}");
    assert_eq!(submitted[0].result, AuditResult::Ok);
    assert_eq!(submitted[0].detail.as_deref(), Some("stage=submitted"));
    assert_eq!(submitted[0].target.as_deref(), Some(target.as_str()));
    assert_eq!(submitted[0].actor, "owner");
    let outcome = stage_rows(&t.c, "outcome").await;
    assert_eq!(outcome.len(), 1, "{outcome:?}");
    assert_eq!(outcome[0].result, AuditResult::Ok);
    assert_eq!(outcome[0].detail.as_deref(), Some("stage=outcome"));
    assert_eq!(outcome[0].target.as_deref(), Some(target.as_str()));
    assert_eq!(outcome[0].command_id.as_ref().unwrap().as_str(), "rc-o1");
    assert_eq!(outcome[0].actor, "owner");
}

#[tokio::test]
async fn a_station_refusal_gets_a_refused_outcome_row() {
    let mut t = setup(Duration::from_secs(5)).await;
    let call = post(&t.c, &t.h, RECOUNT, body("rc-r1"));
    let (r, _) = tokio::join!(call, answer_with(&mut t.station, refusal));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "rejected");
    wait_stage(&t.c, "outcome").await;
    let outcome = stage_rows(&t.c, "outcome").await;
    assert_eq!(outcome.len(), 1, "{outcome:?}");
    assert_eq!(outcome[0].result, AuditResult::Refused);
    assert_eq!(
        outcome[0].detail.as_deref(),
        Some("stage=outcome code=session_running")
    );
    assert_eq!(
        outcome[0].target.as_deref(),
        Some(format!("{}/s1", t.f.station.0).as_str())
    );
}

#[tokio::test]
async fn an_unanswered_recount_gets_outcome_unobserved() {
    let mut t = setup(Duration::from_millis(200)).await;
    let call = post(&t.c, &t.h, RECOUNT, body("rc-u1"));
    tokio::pin!(call);
    // The station gets the command and never answers it.
    let _req = tokio::select! {
        _ = &mut call => panic!("the call ended before the station got the command"),
        req = next_request(&mut t.station) => req,
    };
    wait_stage(&t.c, "outcome_unobserved").await;
    let rows = stage_rows(&t.c, "outcome_unobserved").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Failed);
    assert_eq!(rows[0].detail.as_deref(), Some("stage=outcome_unobserved"));
    assert_eq!(
        rows[0].target.as_deref(),
        Some(format!("{}/s1", t.f.station.0).as_str())
    );
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "rc-u1");
    assert!(stage_rows(&t.c, "outcome").await.is_empty());
}

#[tokio::test]
async fn a_duplicate_command_writes_no_second_outcome() {
    let mut t = setup(Duration::from_secs(5)).await;
    let call = post(&t.c, &t.h, RECOUNT, body("rc-d1"));
    let (r, _) = tokio::join!(call, answer_with(&mut t.station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    wait_stage(&t.c, "outcome").await;
    // The same command ID again: the stored command comes back, nothing is
    // sent, and the deck writes a `submitted duplicate` row but no outcome.
    let again = post(&t.c, &t.h, RECOUNT, body("rc-d1")).await;
    assert_eq!(again.status, 200, "{}", again.body);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(stage_rows(&t.c, "outcome").await.len(), 1);
    let submitted = stage_rows(&t.c, "submitted").await;
    assert_eq!(submitted.len(), 2, "{submitted:?}");
    assert_eq!(
        submitted[0].detail.as_deref(),
        Some("stage=submitted duplicate")
    );
    assert_eq!(submitted[1].detail.as_deref(), Some("stage=submitted"));
    no_request(&mut t.station, Duration::from_millis(300)).await;
}
