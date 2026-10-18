//! The input limits of the owner caps route (R13 P7 task K1, §9.1):
//! `POST /api/coordinator/grants/{grant_id}/caps` with `input_cap` and
//! `input_character_cap` follows the rules of the recording limits: a raise
//! needs the elevation, a grant without the allowance refuses them, and the
//! ranges have fixed texts. The counters never change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use crate::common::board::{BoardClient, Http, board};
use crate::common::fleet::bound_edit;
use crate::common::*;
use crate::settings_support::*;
use fleetdeck_deck::store::GrantClass;
use serde_json::{Value, json};

const ELEVATION: &str = "x-fleetdeck-elevation";
const INPUT3: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
    /// A grant that allows input (limits 100 and 20,000).
    allowed: String,
    /// A grant of the three classes without the allowance.
    plain: String,
}

async fn rig() -> Rig {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let allowed = bound_edit(&f, &f.station, "coord-1", &INPUT3, |g| {
        g.allow_input = true;
        g.input_cap = 100;
        g.input_character_cap = 20_000;
    })
    .await;
    let plain = bound_edit(&f, &f.station, "coord-2", &INPUT3, |_| {}).await;
    Rig {
        f,
        c,
        h,
        allowed: allowed.grant_id,
        plain: plain.grant_id,
    }
}

impl Rig {
    async fn caps(&self, id: &str, body: Value, elevation: Option<&str>) -> Http {
        let mut headers = refs(&self.h);
        if let Some(token) = elevation {
            headers.push((ELEVATION, token));
        }
        self.c
            .post(
                &format!("/api/coordinator/grants/{id}/caps"),
                &headers,
                body,
            )
            .await
    }
}

#[tokio::test]
async fn an_input_limit_decrease_needs_no_elevation_and_a_raise_does_and_the_counters_stay() {
    let r = rig().await;
    let down = r
        .caps(
            &r.allowed,
            json!({"input_cap": 50, "input_character_cap": 10_000}),
            None,
        )
        .await;
    assert_eq!(down.status, 200, "{}", down.body);
    assert_eq!(
        (
            down.body["input_cap"].as_u64(),
            down.body["input_character_cap"].as_u64()
        ),
        (Some(50), Some(10_000))
    );
    assert_eq!(
        (
            down.body["input_used"].as_u64(),
            down.body["input_character_used"].as_u64()
        ),
        (Some(0), Some(0))
    );
    // A raise of either limit needs the elevation and changes nothing without.
    for body in [
        json!({"input_cap": 60}),
        json!({"input_character_cap": 10_001}),
    ] {
        expect(
            &r.caps(&r.allowed, body, None).await,
            400,
            "elevation_required",
        );
    }
    let g = r.f.store.grant(r.allowed.clone()).await.unwrap().unwrap();
    assert_eq!((g.input_cap, g.input_character_cap), (50, 10_000));
    let token = elevate(&r.c, &r.h).await;
    let up = r
        .caps(
            &r.allowed,
            json!({"input_cap": 60, "input_character_cap": 10_001}),
            Some(&token),
        )
        .await;
    assert_eq!(up.status, 200, "{}", up.body);
    assert_eq!(up.body["input_cap"], 60);
    // Zero is allowed (no new accepted call); an empty change keeps all.
    let zero = r.caps(&r.allowed, json!({"input_cap": 0}), None).await;
    assert_eq!(zero.status, 200, "{}", zero.body);
    assert_eq!(zero.body["input_cap"], 0);
    let same = r.caps(&r.allowed, json!({}), None).await;
    assert_eq!(same.body["input_character_cap"], 10_001);
    // The audit row holds the old and the new input limits, never a count of
    // use.
    let rows = r.f.store.audit_log(50).await.unwrap();
    let row = rows
        .iter()
        .find(|e| e.action == "coordinator.grant_caps" && e.target.as_deref() == Some(&r.allowed))
        .expect("an audit row");
    let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
    assert!(detail["old"].get("input_cap").is_some(), "{detail}");
    assert!(
        detail["new"].get("input_character_cap").is_some(),
        "{detail}"
    );
}

#[tokio::test]
async fn the_input_limit_ranges_have_fixed_texts_and_a_grant_without_the_allowance_refuses_them() {
    let r = rig().await;
    let token = elevate(&r.c, &r.h).await;
    for (body, text) in [
        (json!({"input_cap": 5001}), "input_cap is 0 to 5000"),
        (
            json!({"input_character_cap": 1_000_001}),
            "input_character_cap is 0 to 1000000",
        ),
    ] {
        let resp = r.caps(&r.allowed, body, Some(&token)).await;
        expect(&resp, 400, "invalid_grant");
        assert!(
            resp.body.to_string().contains(text),
            "{text}: {}",
            resp.body
        );
    }
    // The limits of a grant without the allowance are refused, also with the
    // elevation and also at zero; its view shows no input field.
    for body in [
        json!({"input_cap": 0}),
        json!({"input_character_cap": 5}),
        json!({"input_cap": 1, "input_character_cap": 1}),
    ] {
        let resp = r.caps(&r.plain, body, Some(&token)).await;
        expect(&resp, 400, "invalid_grant");
        assert!(
            resp.body
                .to_string()
                .contains("input limits need a grant that allows input"),
            "{}",
            resp.body
        );
    }
    // The other caps of that grant still change, and its view has no input
    // field.
    let ok = r.caps(&r.plain, json!({"desktop_cap": 10}), None).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    for key in [
        "allow_input",
        "input_cap",
        "input_used",
        "input_character_cap",
    ] {
        assert!(ok.body.get(key).is_none(), "{key}: {}", ok.body);
    }
    // A decode error is the generic body error.
    expect(
        &r.caps(&r.allowed, json!({"input_cap": -1}), None).await,
        400,
        "invalid_body",
    );
    expect(
        &r.caps(&r.allowed, json!({"input_cap": "many"}), None).await,
        400,
        "invalid_body",
    );
}
