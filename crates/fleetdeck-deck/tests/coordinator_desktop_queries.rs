//! The status tool, the query stubs and the rate of the P6 deck handlers, and
//! the audit rows of the mutating tools (R13 P6 task T1b). The calls are in
//! `coordinator_desktop_calls.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;

use coordinator_desktop_support::*;
use fleetdeck_deck::store::coordinator_desktop::{OpTool, request_key};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

async fn op(w: &World, request_id: &str) -> fleetdeck_deck::store::coordinator_desktop::DesktopOp {
    w.f.store
        .desktop_op_by_key(&w.b.grant_id, &request_key(request_id))
        .await
        .unwrap()
        .expect("an operation row")
}

#[tokio::test]
async fn desktop_audit_rows_name_only_identifiers_and_never_the_request_url_or_text() {
    let w = world().await;
    let text = "SENTINEL-TITLE-4711";
    let rid = "SENTINEL-REQUEST-4711";
    assert_eq!(w.call("c1", notify(rid, text)).await["status"], "ok");
    let url = "http://localhost/SENTINEL-URL-4711";
    assert_eq!(w.call("c2", preview("req-2", url)).await["status"], "ok");
    assert_eq!(w.call("c3", close("req-3", WINDOW)).await["status"], "ok");
    let dump = w.audit_text().await;
    for secret in [text, rid, "SENTINEL-URL-4711", "body text"] {
        assert!(!dump.contains(secret), "{secret} in {dump}");
    }
    // The stored rows never hold the request ID, the URL or the text either.
    let ops =
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 10)
            .await
            .unwrap();
    assert!(!format!("{ops:?}").contains("SENTINEL"));
    let row = op(&w, "req-2").await;
    assert!(!format!("{row:?}").contains("SENTINEL"));
    // Two rows for each operation: the attempt at acceptance, then the outcome.
    for (action, count) in [
        ("desktop.notify", 2),
        ("desktop.preview_open", 2),
        ("desktop.window_close", 2),
    ] {
        let rows = w.audit_rows(Some(action)).await;
        assert_eq!(rows.len(), count, "{action}");
        let stages: Vec<String> = rows
            .iter()
            .map(|r| {
                serde_json::from_str::<Value>(&r.2).unwrap()["stage"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(stages, ["attempt", "outcome"], "{action}");
    }
    // The fixed keys, the grant, the generation, the caller and the target.
    let rows = w.audit_rows(Some("desktop.window_close")).await;
    let row = op(&w, "req-3").await;
    assert_eq!(
        rows[0].1, row.operation_id,
        "the target is the operation ID"
    );
    let detail: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(detail["grant_id"], w.b.grant_id.as_str());
    assert_eq!(detail["generation"], 1);
    assert_eq!(detail["caller_station"], w.station().as_str());
    assert_eq!(detail["caller_session"], "coord-1");
    assert_eq!(detail["target_station"], w.station().as_str());
    assert_eq!(detail["operation_id"], row.operation_id.as_str());
    assert_eq!(detail["window_id"], WINDOW);
    assert_eq!(detail["charged"], json!({"desktop": 1, "capture": 0}));
    let detail: Value = serde_json::from_str(&rows[1].2).unwrap();
    assert_eq!(detail["code"], Value::Null);
}

#[tokio::test]
async fn desktop_action_status_maps_every_state_and_hides_what_is_not_its_own() {
    let w = world_with(&BOTH, |_| {}).await;
    // Unknown request IDs.
    assert_eq!(
        w.query(status_query("nope")).await,
        json!({"status": "ok", "result": {"request_id": "nope", "state": "unknown"}})
    );
    // A done operation.
    assert_eq!(w.call("c1", close("r1", WINDOW)).await["status"], "ok");
    let row = op(&w, "r1").await;
    assert_eq!(
        w.query(status_query("r1")).await,
        json!({"status": "ok", "result": {"request_id": "r1",
            "operation_id": row.operation_id.as_str(), "station": w.station().as_str(),
            "state": "done", "result": {}}})
    );
    // A failed one, with its code and text.
    w.fake
        .set(except("desktop.act", |_| Some(fail("forbidden", "x"))));
    assert_eq!(
        code(&w.call("c2", close("r2", WINDOW)).await),
        "desktop_forbidden"
    );
    let status = w.query(status_query("r2")).await;
    assert_eq!(status["result"]["state"], "failed");
    assert_eq!(status["result"]["error"], "desktop_forbidden");
    assert_eq!(
        status["result"]["message"],
        "The station refused the desktop action."
    );
    // A screenshot row is invisible to this tool.
    let shot = w.new_op("op-shot", OpTool::Screenshot, "hash", &w.station());
    let mut shot = shot;
    shot.request_key = request_key("shot-1");
    w.attempted(shot).await;
    assert_eq!(
        w.query(status_query("shot-1")).await["result"],
        json!({"request_id": "shot-1", "state": "unknown"})
    );
    // A lost authority never sees a stored result: the old binding is stale.
    w.rebind().await;
    assert_eq!(code(&w.query(status_query("r1")).await), "binding_stale");
    // Each query row of an authorized caller has its audit row.
    assert!(!w.audit_rows(Some("desktop.action_status")).await.is_empty());
}

#[tokio::test]
async fn a_malformed_query_is_the_wire_error_invalid_params() {
    let w = world_with(&BOTH, |_| {}).await;
    // A malformed query is the wire error `invalid_params`.
    let Outcome::Err(e) = w.query_raw(json!({"junk": 1})).await else {
        panic!("a wire error");
    };
    assert_eq!(e.code, "invalid_params");
}

#[tokio::test]
async fn desktop_query_rate_is_sixty_in_a_window() {
    let w = world().await;
    for i in 0..60 {
        let r = w.query(status_query(&format!("q{i}"))).await;
        assert_eq!(r["status"], "ok", "{i}: {r}");
    }
    let limited = w.query(status_query("q60")).await;
    assert_eq!(code(&limited), "rate_limited");
    assert_eq!(
        limited["message"],
        "This grant's desktop rate limit has been reached. Try again later."
    );
}
