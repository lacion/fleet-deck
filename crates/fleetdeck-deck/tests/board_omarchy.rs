//! The Omarchy routes of a station (design `omarchy-admin.md` revision 5
//! section 4) over loopback HTTPS with a fake station link: the owner-only
//! rule, the elevation of every action, the wire error mapping with fixed
//! texts and the audit row without a message.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_deck::http::omarchy_routes::wait_for;
use fleetdeck_deck::store::{AuditEntry, AuditResult};
use fleetdeck_link::LiveLink;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use fleetdeck_proto::omarchy_admin::OmarchyAction;
use serde_json::{Value, json};
use settings_support::*;

const PATH: &str = "/api/stations/fake/omarchy";
const SECRET_TEXT: &str = "SECRET-STATION-MESSAGE /home/ann/.config stderr: boom";
const UNSUPPORTED: &str = "This station's build cannot manage Omarchy from the board.";

struct World {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
}

async fn setup() -> World {
    let mut f = fixture().await;
    let station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    World { f, station, c, h }
}

fn status_json(running: Value) -> Value {
    json!({
        "omarchy": { "present": true, "version": "3.1" },
        "install": null,
        "screensaver": null,
        "provision": null,
        "running": running,
    })
}

fn result_json(outcome: &str, code: Value, message: Value) -> Value {
    json!({
        "outcome": outcome,
        "code": code,
        "message": message,
        "status": status_json(Value::Null),
    })
}

/// The owner's headers and a live elevation in `X-Fleetdeck-Elevation`.
async fn elevated(w: &World) -> Headers {
    let mut h = w.h.clone();
    h.push((
        "X-Fleetdeck-Elevation".to_owned(),
        elevate(&w.c, &w.h).await,
    ));
    h
}

async fn all_rows(c: &BoardClient) -> Vec<AuditEntry> {
    c.store.audit_log(100).await.unwrap()
}

/// The `station.omarchy` rows, newest first.
async fn audit_rows(c: &BoardClient) -> Vec<AuditEntry> {
    all_rows(c)
        .await
        .into_iter()
        .filter(|e| e.action == "station.omarchy")
        .collect()
}

#[tokio::test]
async fn the_status_passes_through_and_each_station_error_has_a_fixed_text() {
    let mut w = setup().await;
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PATH),
        Outcome::Ok(status_json(json!("install"))),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "omarchy.status");
    assert_eq!(req.params, json!({}));
    assert_eq!(r.body, status_json(json!("install")));
    // A successful status read writes no audit row.
    assert!(audit_rows(&w.c).await.is_empty());

    for (code, status, name, text) in [
        (
            "omarchy_missing",
            409,
            "omarchy_missing",
            "Omarchy was not found on this station.",
        ),
        ("unavailable", 501, "unsupported", UNSUPPORTED),
        ("unknown_method", 501, "unsupported", UNSUPPORTED),
        (
            "some_other",
            502,
            "station_error",
            "the station answered with an error",
        ),
    ] {
        let (r, _) = with_station(
            &mut w.station,
            get(&w.c, &w.h, PATH),
            Outcome::Err(WireError::new(code, SECRET_TEXT)),
        )
        .await;
        expect(&r, status, name);
        assert_eq!(r.body["message"], text, "{code}");
        assert!(!r.text.contains("SECRET-STATION-MESSAGE"), "{}", r.text);
    }

    // An answer that does not parse is a 502.
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PATH),
        Outcome::Ok(json!({ "nope": 1 })),
    )
    .await;
    expect(&r, 502, "station_error");
}

#[tokio::test]
async fn a_station_without_the_feature_an_unknown_and_an_unreachable_station() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    // A second live link that did not negotiate the feature: fail closed.
    w.f.store.set_link_features(&w.f.station, 98, vec![]);
    let r = get(&w.c, &w.h, PATH).await;
    expect(&r, 501, "unsupported");
    assert_eq!(r.body["message"], UNSUPPORTED);
    let r = post(&w.c, &h, PATH, json!({ "action": "install" })).await;
    expect(&r, 501, "unsupported");
    assert_eq!(r.body["message"], UNSUPPORTED);
    no_request(&mut w.station, Duration::from_millis(300)).await;
    w.f.store.clear_link_features(&w.f.station, 98);

    expect(
        &get(&w.c, &w.h, "/api/stations/nobody/omarchy").await,
        404,
        "unknown_station",
    );
    let r = post(
        &w.c,
        &h,
        "/api/stations/nobody/omarchy",
        json!({ "action": "install" }),
    )
    .await;
    expect(&r, 404, "unknown_station");

    w.f.drop_link(w.station).await;
    expect(&get(&w.c, &w.h, PATH).await, 409, "station_unreachable");
    let r = post(&w.c, &h, PATH, json!({ "action": "install" })).await;
    expect(&r, 409, "station_unreachable");
}

#[tokio::test]
async fn a_token_and_a_member_are_refused_with_the_audit_row_of_the_owner_rule() {
    let mut w = setup().await;
    let (_, t) =
        w.c.store
            .create_token(
                "ops".into(),
                BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
                86_400_000,
            )
            .await
            .unwrap();
    let auth = format!("Bearer {}", t.expose());
    let bearer: Headers = vec![("Authorization".to_owned(), auth)];
    let share = post(
        &w.c,
        &w.h,
        "/api/shares",
        json!({ "kind": "board", "role": "approver" }),
    )
    .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let invite = share.body["url"].as_str().unwrap().to_owned();
    let join =
        w.c.post(
            "/api/auth/join",
            &[],
            json!({ "token": invite.split("#join=").nth(1).unwrap(), "display_name": "Ann" }),
        )
        .await;
    assert_eq!(join.status, 200, "{}", join.body);
    let member: Headers = vec![
        (
            "Cookie".to_owned(),
            join.header("set-cookie")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
        ),
        (
            "X-CSRF-Token".to_owned(),
            join.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ];
    for who in [&bearer, &member] {
        let r = w.c.req("GET", PATH, &refs(who), None).await;
        expect(&r, 403, "owner_only");
        let body = Some(json!({ "action": "install" }));
        let r = w.c.req("POST", PATH, &refs(who), body).await;
        expect(&r, 403, "owner_only");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // The admin token reaches the handler: its refusal is the audit row of
    // the owner rule, one for each method.
    let status =
        w.c.audited("station.omarchy_status", AuditResult::Refused)
            .await;
    assert!(
        status.contains(&Some("owner_only".to_owned())),
        "{status:?}"
    );
    let act =
        w.c.audited("station.omarchy_act", AuditResult::Refused)
            .await;
    assert!(act.contains(&Some("owner_only".to_owned())), "{act:?}");
    // The refusal of the owner rule is the row of that POST: no second row.
    assert!(audit_rows(&w.c).await.is_empty());
}

#[tokio::test]
async fn an_action_needs_the_elevation_and_a_valid_body() {
    let mut w = setup().await;
    // No header, a header that is not an elevation: 400 and nothing sent.
    let mut wrong = w.h.clone();
    wrong.push(("X-Fleetdeck-Elevation".to_owned(), "not-a-token".to_owned()));
    for h in [w.h.clone(), wrong] {
        let r = post(&w.c, &h, PATH, json!({ "action": "install" })).await;
        expect(&r, 400, "elevation_required");
        assert_eq!(
            r.body["message"],
            "This action needs the owner's elevation."
        );
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;

    // A bad body: an unknown key, an unknown action, a missing action.
    let h = elevated(&w).await;
    for body in [
        json!({ "action": "install", "elevated": true }),
        json!({ "action": "reboot" }),
        json!({}),
    ] {
        let r = post(&w.c, &h, PATH, body).await;
        expect(&r, 400, "invalid_body");
        assert_eq!(r.body["message"], "the request body is not valid");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;

    // The audit rows: one for each POST, refused, with the error code.
    let rows = audit_rows(&w.c).await;
    assert_eq!(rows.len(), 5);
    for e in &rows {
        assert_eq!(e.result, AuditResult::Refused);
        assert_eq!(e.target.as_deref(), Some("fake"));
        assert_eq!(e.actor, "owner");
    }
    let details: Vec<_> = rows.iter().filter_map(|e| e.detail.as_deref()).collect();
    let count = |text: &str| details.iter().filter(|d| **d == text).count();
    assert_eq!(count("action=install outcome=elevation_required code=-"), 2);
    assert_eq!(count("action=- outcome=invalid_body code=-"), 3);
}

#[tokio::test]
async fn an_elevated_action_reaches_the_station_with_the_params_and_its_result_passes() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let token = h.last().unwrap().1.clone();
    for (action, outcome) in [
        ("install", "done"),
        ("provision_retry", "started"),
        ("screensaver_rebaseline", "done"),
    ] {
        let (r, req) = with_station(
            &mut w.station,
            post(&w.c, &h, PATH, json!({ "action": action })),
            Outcome::Ok(result_json(outcome, Value::Null, Value::Null)),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(req.method, "omarchy.act");
        assert_eq!(req.params, json!({ "action": action, "elevated": true }));
        assert!(!req.params.to_string().contains(&token));
        assert_eq!(r.body["outcome"], outcome);
        let rows = audit_rows(&w.c).await;
        assert_eq!(rows[0].result, AuditResult::Ok);
        assert_eq!(rows[0].target.as_deref(), Some("fake"));
        assert_eq!(
            rows[0].detail.as_deref(),
            Some(format!("action={action} outcome={outcome} code=-").as_str())
        );
    }

    // A `failed` result is a 200 with its code and message; the audit row
    // is `failed` and holds the code, never the message.
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, json!({ "action": "uninstall" })),
        Outcome::Ok(result_json(
            "failed",
            json!("uninstall_incomplete"),
            json!(SECRET_TEXT),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "failed");
    assert_eq!(r.body["code"], "uninstall_incomplete");
    assert_eq!(r.body["message"], SECRET_TEXT);
    let rows = audit_rows(&w.c).await;
    assert_eq!(rows[0].result, AuditResult::Failed);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("action=uninstall outcome=failed code=uninstall_incomplete")
    );

    // A code that the contract does not name never reaches the row.
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, json!({ "action": "install" })),
        Outcome::Ok(result_json("failed", json!(SECRET_TEXT), Value::Null)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        audit_rows(&w.c).await[0].detail.as_deref(),
        Some("action=install outcome=failed code=failed")
    );

    // An answer that does not parse is a 502.
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, json!({ "action": "install" })),
        Outcome::Ok(json!({ "outcome": "maybe" })),
    )
    .await;
    expect(&r, 502, "station_error");

    for e in all_rows(&w.c).await {
        let text = format!("{e:?}");
        assert!(!text.contains("SECRET-STATION-MESSAGE"), "{text}");
        assert!(!text.contains("/home/ann"), "{text}");
        assert!(!text.contains(&token), "{text}");
    }
}

#[tokio::test]
async fn each_station_error_of_an_action_maps_to_its_status_code_and_fixed_text() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    for (code, status, name, text) in [
        (
            "elevation_required",
            400,
            "elevation_required",
            "This action needs the owner's elevation.",
        ),
        (
            "busy",
            409,
            "busy",
            "Another Omarchy action is running on this station.",
        ),
        (
            "omarchy_missing",
            409,
            "omarchy_missing",
            "Omarchy was not found on this station.",
        ),
        (
            "not_provisioned",
            409,
            "not_provisioned",
            "Provisioning cannot be retried from the board in this state.",
        ),
        (
            "invalid_request",
            400,
            "invalid_request",
            "The request is not valid.",
        ),
        ("unavailable", 501, "unsupported", UNSUPPORTED),
        ("unsupported", 501, "unsupported", UNSUPPORTED),
        ("feature_not_negotiated", 501, "unsupported", UNSUPPORTED),
        ("unknown_method", 501, "unsupported", UNSUPPORTED),
        (
            "something_else",
            502,
            "station_error",
            "the station answered with an error",
        ),
    ] {
        let (r, _) = with_station(
            &mut w.station,
            post(&w.c, &h, PATH, json!({ "action": "install" })),
            Outcome::Err(WireError::new(code, SECRET_TEXT)),
        )
        .await;
        expect(&r, status, name);
        assert_eq!(r.body["message"], text, "{code}");
        assert!(!r.text.contains("SECRET-STATION-MESSAGE"), "{}", r.text);
        let rows = audit_rows(&w.c).await;
        assert_eq!(rows[0].result, AuditResult::Refused, "{code}");
        assert_eq!(
            rows[0].detail.as_deref(),
            Some(format!("action=install outcome={name} code=-").as_str())
        );
    }
    for e in all_rows(&w.c).await {
        assert!(!format!("{e:?}").contains("SECRET-STATION-MESSAGE"));
    }
}

#[test]
fn a_provision_retry_waits_15_seconds_and_every_other_action_75() {
    assert_eq!(
        wait_for(OmarchyAction::ProvisionRetry),
        Duration::from_secs(15)
    );
    for action in [
        OmarchyAction::Install,
        OmarchyAction::Uninstall,
        OmarchyAction::ScreensaverOn,
        OmarchyAction::ScreensaverOff,
        OmarchyAction::ScreensaverRebaseline,
    ] {
        assert_eq!(wait_for(action), Duration::from_secs(75), "{action:?}");
    }
}

/// The station answers an `install` after 16 s, which is past the 15 s of
/// the settings routes: the board still waits and passes the result.
#[tokio::test]
async fn an_install_waits_past_the_settings_wait_for_the_answer() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let call = post(&w.c, &h, PATH, json!({ "action": "install" }));
    let answering = async {
        let req = next_request(&mut w.station).await;
        tokio::time::sleep(Duration::from_secs(16)).await;
        w.station
            .control
            .respond(
                req.id,
                Outcome::Ok(result_json("done", Value::Null, Value::Null)),
            )
            .await
            .unwrap();
    };
    let (r, ()) = tokio::join!(call, answering);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "done");
}
