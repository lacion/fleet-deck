//! The owner routes of the P6 grants (R13 P6 task T1b): the cap change with
//! its elevation rule and its audit row, and the recent desktop operations of
//! a grant.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;

use common::board::{Http, board};
use common::fleet::bound_edit;
use common::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::coordinator_desktop::{NewOp, OpOutcome, OpTool, request_key};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use serde_json::{Value, json};
use settings_support::*;

const ELEVATION: &str = "x-fleetdeck-elevation";

struct Rig {
    f: Fixture,
    c: common::board::BoardClient,
    h: Headers,
    grant_id: String,
}

async fn rig() -> Rig {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let binding = bound_edit(
        &f,
        &f.station,
        "coord-1",
        &[GrantClass::Desktop, GrantClass::Capture],
        |_| {},
    )
    .await;
    Rig {
        f,
        c,
        h,
        grant_id: binding.grant_id,
    }
}

fn caps_path(id: &str) -> String {
    format!("/api/coordinator/grants/{id}/caps")
}

fn ops_path(id: &str) -> String {
    format!("/api/coordinator/grants/{id}/desktop-ops")
}

impl Rig {
    async fn caps(&self, body: Value, elevation: Option<&str>) -> Http {
        let mut headers = refs(&self.h);
        if let Some(token) = elevation {
            headers.push((ELEVATION, token));
        }
        self.c
            .post(&caps_path(&self.grant_id), &headers, body)
            .await
    }
}

#[tokio::test]
async fn a_decrease_needs_no_elevation_and_an_increase_does() {
    let r = rig().await;
    // The defaults are 300 actions, 20 screenshots and 64 MiB of images.
    let down = r
        .caps(json!({"desktop_cap": 100, "capture_cap": 5}), None)
        .await;
    assert_eq!(down.status, 200, "{}", down.body);
    assert_eq!(down.body["desktop_cap"], 100);
    assert_eq!(down.body["capture_cap"], 5);
    assert_eq!(down.body["image_byte_cap"], 67_108_864);
    // The counters are not touched.
    assert_eq!(
        (
            down.body["desktop_used"].as_i64(),
            down.body["capture_used"].as_i64()
        ),
        (Some(0), Some(0))
    );
    // An increase without the elevation.
    let up = r.caps(json!({"desktop_cap": 200}), None).await;
    expect(&up, 400, "elevation_required");
    assert!(
        up.body
            .to_string()
            .contains("a fleet-control grant needs the owner's elevation")
    );
    let g = r.f.store.grant(r.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(g.desktop_cap, Some(100), "nothing changed");
    // With a fresh elevation.
    let token = elevate(&r.c, &r.h).await;
    let up = r
        .caps(
            json!({"desktop_cap": 200, "image_byte_cap": 134_217_728_u64}),
            Some(&token),
        )
        .await;
    assert_eq!(up.status, 200, "{}", up.body);
    assert_eq!(up.body["desktop_cap"], 200);
    assert_eq!(up.body["image_byte_cap"], 134_217_728_u64);
    // A bad elevation token is no elevation.
    let up = r
        .caps(json!({"desktop_cap": 300}), Some("not-a-token"))
        .await;
    expect(&up, 400, "elevation_required");
    // An empty change keeps everything.
    let same = r.caps(json!({}), None).await;
    assert_eq!(same.status, 200, "{}", same.body);
    assert_eq!(same.body["desktop_cap"], 200);
}

#[tokio::test]
async fn invalid_caps_are_refused_with_the_fixed_text_even_with_the_elevation() {
    let r = rig().await;
    let token = elevate(&r.c, &r.h).await;
    for (body, text) in [
        (json!({"desktop_cap": 5001}), "desktop_cap is 0 to 5000"),
        (json!({"capture_cap": 5001}), "capture_cap is 0 to 5000"),
        (
            json!({"image_byte_cap": 1_048_577}),
            "image_byte_cap is a multiple of 1048576",
        ),
        (
            json!({"image_byte_cap": 1_073_741_825_u64}),
            "image_byte_cap is 0 to 1073741824",
        ),
    ] {
        let resp = r.caps(body, Some(&token)).await;
        expect(&resp, 400, "invalid_grant");
        assert!(
            resp.body.to_string().contains(text),
            "{text}: {}",
            resp.body
        );
    }
    // A cap of a class the grant lacks.
    let f = &r.f;
    let only = bound_edit(f, &f.station, "coord-2", &[GrantClass::Desktop], |_| {}).await;
    let resp =
        r.c.post(
            &caps_path(&only.grant_id),
            &[refs(&r.h), vec![(ELEVATION, token.as_str())]].concat(),
            json!({"capture_cap": 1}),
        )
        .await;
    expect(&resp, 400, "invalid_grant");
    assert!(
        resp.body
            .to_string()
            .contains("capture_cap and image_byte_cap need the capture class"),
        "{}",
        resp.body
    );
}

#[tokio::test]
async fn an_unknown_or_revoked_grant_a_bad_body_and_a_non_owner_are_refused() {
    let r = rig().await;
    let none =
        r.c.post(&caps_path("nope"), &refs(&r.h), json!({"desktop_cap": 1}))
            .await;
    expect(&none, 404, "not_found");
    assert!(none.body.to_string().contains("No such grant."));
    for body in [
        json!({"surprise": 1}),
        json!({"desktop_cap": "many"}),
        json!({"desktop_cap": -1}),
    ] {
        expect(&r.caps(body, None).await, 400, "invalid_body");
    }
    r.f.store
        .revoke_grant(r.grant_id.clone(), "test".into())
        .await
        .unwrap();
    expect(
        &r.caps(json!({"desktop_cap": 1}), None).await,
        404,
        "not_found",
    );
    // No login at all.
    let anonymous =
        r.c.post(&caps_path(&r.grant_id), &[], json!({"desktop_cap": 1}))
            .await;
    assert!(
        anonymous.status == 401 || anonymous.status == 403,
        "{}",
        anonymous.status
    );
    let anonymous = r.c.get(&ops_path(&r.grant_id), &[]).await;
    assert!(
        anonymous.status == 401 || anonymous.status == 403,
        "{}",
        anonymous.status
    );
}

#[tokio::test]
async fn a_cap_change_has_an_audit_row_with_the_old_and_the_new_caps() {
    let r = rig().await;
    let resp = r.caps(json!({"desktop_cap": 50}), None).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let rows: Vec<_> =
        r.f.store
            .audit_log(50)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == "coordinator.grant_caps")
            .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, "owner");
    assert_eq!(rows[0].target.as_deref(), Some(r.grant_id.as_str()));
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!(detail["old"]["desktop_cap"], 300);
    assert_eq!(detail["new"]["desktop_cap"], 50);
    assert_eq!(detail["old"]["capture_cap"], 20);
    assert_eq!(detail["new"]["image_byte_cap"], 67_108_864);
    // A refusal has a row with its code and no values.
    let refused = r.caps(json!({"desktop_cap": 90}), None).await;
    expect(&refused, 400, "elevation_required");
    let refused_rows =
        r.c.audited(
            "coordinator.grant_caps",
            fleetdeck_deck::store::AuditResult::Refused,
        )
        .await;
    assert_eq!(refused_rows, vec![Some("elevation_required".to_owned())]);
}

fn context(r: &Rig) -> CoordinatorContext {
    CoordinatorContext {
        grant_id: r.grant_id.clone(),
        binding_id: "b-coord-1".to_owned(),
        generation: 1,
        caller_station: r.f.station.clone(),
        caller_session: "coord-1".into(),
        caller_label: "coord-1".to_owned(),
    }
}

#[tokio::test]
async fn the_recent_operations_are_the_newest_fifty_with_no_resource_result_or_hash() {
    let r = rig().await;
    let now = r.f.store.now_ms();
    for i in 0..55 {
        let op = NewOp {
            operation_id: format!("op-{i:03}"),
            grant_id: r.grant_id.clone(),
            request_key: request_key(&format!("req-{i}")),
            payload_hash: "PAYLOAD-HASH-SENTINEL".to_owned(),
            tool: OpTool::Notify,
            class: fleetdeck_deck::store::coordinator_desktop::OpClass::Desktop,
            ctx: context(&r),
            target_station: r.f.station.clone(),
            action_hash: Some("ACTION-HASH-SENTINEL".to_owned()),
            resource: json!({"window_id": "RESOURCE-SENTINEL"}),
            seconds: None,
        };
        // A minute apart: the rolling rates never fill.
        let at = now + i64::from(i) * 61_000;
        r.f.store.desktop_admit(op, at).await.unwrap();
    }
    r.f.store
        .desktop_op_finish(
            "op-054",
            OpOutcome::Done(json!({"window_id": "RESULT-SENTINEL"})),
            now + 4_000_000,
        )
        .await
        .unwrap();
    let resp = get(&r.c, &r.h, &ops_path(&r.grant_id)).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let rows = resp.body.as_array().unwrap();
    assert_eq!(rows.len(), 50);
    assert_eq!(rows[0]["operation_id"], "op-054");
    assert_eq!(rows[49]["operation_id"], "op-005");
    assert_eq!(rows[0]["state"], "done");
    assert_eq!(rows[1]["state"], "prepared");
    let mut keys: Vec<&str> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "code",
            "created_at",
            "operation_id",
            "state",
            "target_station",
            "tool",
            "updated_at"
        ]
    );
    assert_eq!(rows[0]["tool"], "notify");
    assert_eq!(rows[0]["target_station"], r.f.station.as_str());
    let text = resp.body.to_string();
    for secret in ["SENTINEL", "req-5"] {
        assert!(!text.contains(secret), "{secret}");
    }
    // An unknown grant.
    expect(
        &get(&r.c, &r.h, &ops_path("nope")).await,
        404,
        "unknown_grant",
    );
}
