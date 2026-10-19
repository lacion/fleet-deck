//! The installer repair routes of a station (`board-gaps.md` §3, BG-2) over
//! loopback HTTPS with a fake station link: the three reads, the three
//! actions, the feature gate, the owner rule, the elevation, the binding of a
//! plan to the owner login, the texts of the six codes and the audit row
//! without a path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_deck::store::{AuditEntry, AuditResult};
use fleetdeck_link::LiveLink;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use settings_support::*;

const PATH: &str = "/api/stations/fake/omarchy";
const PLAN_PATH: &str = "/api/stations/fake/omarchy/install-plan";
const REPAIRS_PATH: &str = "/api/stations/fake/omarchy/repairs";
const REPAIR: &str = "0123456789abcdef0123456789abcdef";
const SECRET_TEXT: &str = "SECRET-STATION-MESSAGE /home/ann/.config stderr: boom";
const UNSUPPORTED: &str = "This station's build cannot manage Omarchy from the board.";
const PLAN_CHANGED: &str =
    "The files changed since this review. Review the replacement plan again.";

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

/// A plan ID that no other test uses: the plans of the deck are in one map
/// for the whole process.
fn plan_for(n: u8) -> String {
    format!("{n:02x}{}", "cd".repeat(15))
}

fn restore_path(repair: &str) -> String {
    format!("/api/stations/fake/omarchy/repairs/{repair}/restore-plan")
}

fn status_json() -> Value {
    json!({
        "omarchy": { "present": true, "version": "3.1" },
        "install": null, "screensaver": null, "provision": null, "running": null,
    })
}

fn plan_view(plan: &str) -> Value {
    json!({
        "plan_id": plan,
        "expires_at": 1_792_400_120_000_i64,
        "files": [{
            "asset_id": "fleet_bindings",
            "path": "/home/ann/.config/hypr/fleet.lua",
            "classification": "recorded_edit",
            "replaceable": true,
        }],
        "blocked": null,
    })
}

fn restore_view(plan: &str) -> Value {
    json!({
        "plan_id": plan,
        "expires_at": 1_792_400_120_000_i64,
        "repair_id": REPAIR,
        "files": [{
            "asset_id": "fleet_bindings",
            "path": "/home/ann/.config/hypr/fleet.lua",
            "action": "copy_backup",
        }],
    })
}

/// A `RepairResult`: two files, the first in `state`, with paths.
fn repair_json(code: Value, state: &str) -> Value {
    json!({
        "code": code,
        "repair_id": REPAIR,
        "files": [
            {"asset_id": "fleet_bindings", "path": "/home/ann/.config/hypr/fleet.lua",
             "backup_path": "/home/ann/.config/hypr/fleet.lua.kept", "state": state},
            {"asset_id": "hook_theme_set", "path": "/home/ann/hook",
             "backup_path": null, "state": "unchanged"},
        ],
    })
}

fn act_result(outcome: &str, code: Value, repair: Value) -> Value {
    json!({
        "outcome": outcome, "code": code, "message": null,
        "status": status_json(), "repair": repair,
    })
}

async fn elevated_for(c: &BoardClient, h: &Headers) -> Headers {
    let mut h = h.clone();
    let token = elevate(c, &h).await;
    h.push(("X-Fleetdeck-Elevation".to_owned(), token));
    h
}

async fn elevated(w: &World) -> Headers {
    elevated_for(&w.c, &w.h).await
}

async fn repair_rows(c: &BoardClient) -> Vec<AuditEntry> {
    c.store
        .audit_log(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "station.omarchy_repair")
        .collect()
}

/// Reads the install plan as `h` and returns its ID.
async fn read_plan(w: &mut World, h: &Headers, plan: &str) -> String {
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, h, PLAN_PATH),
        Outcome::Ok(plan_view(plan)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "omarchy.install_plan");
    r.body["plan_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn omarchy_repair_reads_pass_through_for_the_owner() {
    let plan = plan_for(1);
    let mut w = setup().await;
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PLAN_PATH),
        Outcome::Ok(plan_view(&plan)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        (req.method.as_str(), req.params),
        ("omarchy.install_plan", json!({}))
    );
    assert_eq!(r.body, plan_view(&plan));

    let journals = json!({"journals": []});
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, REPAIRS_PATH),
        Outcome::Ok(journals.clone()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        (req.method.as_str(), req.params),
        ("omarchy.repair_status", json!({}))
    );
    assert_eq!(r.body, journals);

    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, &restore_path(REPAIR)),
        Outcome::Ok(restore_view(&plan)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "omarchy.restore_plan");
    assert_eq!(req.params, json!({ "repair_id": REPAIR }));
    assert_eq!(r.body, restore_view(&plan));
    // A read writes no audit row.
    assert!(repair_rows(&w.c).await.is_empty());

    // A station error has a fixed text; an unknown answer is a 502.
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PLAN_PATH),
        Outcome::Err(WireError::new("busy", SECRET_TEXT)),
    )
    .await;
    expect(&r, 409, "busy");
    assert!(!r.text.contains("SECRET-STATION-MESSAGE"), "{}", r.text);
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, REPAIRS_PATH),
        Outcome::Ok(json!({ "nope": 1 })),
    )
    .await;
    expect(&r, 502, "station_error");
}

#[tokio::test]
async fn omarchy_repair_routes_need_the_repair_feature() {
    let plan = plan_for(2);
    let mut w = setup().await;
    let h = elevated(&w).await;
    // A second live link that offers the admin feature but not the repair one.
    w.f.store
        .set_link_features(&w.f.station, 98, vec!["omarchy_admin.v1".into()]);
    for path in [PLAN_PATH, REPAIRS_PATH, &restore_path(REPAIR)] {
        let r = get(&w.c, &w.h, path).await;
        expect(&r, 501, "unsupported");
        assert_eq!(r.body["message"], UNSUPPORTED);
    }
    for body in [
        json!({"action": "install_replace", "plan_id": &plan}),
        json!({"action": "install_resume", "repair_id": REPAIR}),
        json!({"action": "restore", "plan_id": &plan, "repair_id": REPAIR, "asset_ids": ["fleet_bindings"]}),
    ] {
        let r = post(&w.c, &h, PATH, body).await;
        expect(&r, 501, "unsupported");
        assert_eq!(r.body["message"], UNSUPPORTED);
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // An old action needs only the admin feature.
    let (r, req) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, json!({ "action": "install" })),
        Outcome::Ok(act_result("done", Value::Null, Value::Null)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params, json!({ "action": "install", "elevated": true }));
    // No feature at all: the same.
    w.f.store.set_link_features(&w.f.station, 98, vec![]);
    expect(&get(&w.c, &w.h, PLAN_PATH).await, 501, "unsupported");
    w.f.store.clear_link_features(&w.f.station, 98);
    expect(
        &get(&w.c, &w.h, "/api/stations/nobody/omarchy/install-plan").await,
        404,
        "unknown_station",
    );
    w.f.drop_link(w.station).await;
    expect(
        &get(&w.c, &w.h, REPAIRS_PATH).await,
        409,
        "station_unreachable",
    );
}

#[tokio::test]
async fn omarchy_repair_reads_and_actions_are_for_the_owner_login_only() {
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
    let bearer: Headers = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
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
        for path in [PLAN_PATH, REPAIRS_PATH, &restore_path(REPAIR)] {
            let r = w.c.req("GET", path, &refs(who), None).await;
            expect(&r, 403, "owner_only");
        }
        let body = Some(json!({ "action": "install_resume", "repair_id": REPAIR }));
        let r = w.c.req("POST", PATH, &refs(who), body).await;
        expect(&r, 403, "owner_only");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // The admin token reaches the handler: one refused row for each route.
    for action in [
        "station.omarchy_install_plan",
        "station.omarchy_repairs",
        "station.omarchy_restore_plan",
    ] {
        let rows = w.c.audited(action, AuditResult::Refused).await;
        assert!(
            rows.contains(&Some("owner_only".to_owned())),
            "{action}: {rows:?}"
        );
    }
}

#[tokio::test]
async fn omarchy_restore_plan_checks_the_repair_id_before_the_feature_and_the_call() {
    let mut w = setup().await;
    w.f.store.set_link_features(&w.f.station, 98, vec![]);
    let long = "a".repeat(33);
    for bad in [
        "ABCDEF0123456789abcdef0123456789",
        "0123456789abcdef",
        "0123456789abcdef0123456789abcdeg",
        long.as_str(),
    ] {
        let r = get(&w.c, &w.h, &restore_path(bad)).await;
        expect(&r, 400, "invalid_request");
        assert_eq!(r.body["message"], "The request is not valid.", "{bad}");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // A valid ID reaches the feature check.
    expect(
        &get(&w.c, &w.h, &restore_path(REPAIR)).await,
        501,
        "unsupported",
    );
}

#[tokio::test]
async fn omarchy_the_repair_actions_need_the_elevation() {
    let plan = plan_for(5);
    let mut w = setup().await;
    let mut wrong = w.h.clone();
    wrong.push(("X-Fleetdeck-Elevation".to_owned(), "not-a-token".to_owned()));
    let bodies = [
        json!({"action": "install_replace", "plan_id": &plan}),
        json!({"action": "install_resume", "repair_id": REPAIR}),
        json!({"action": "restore", "plan_id": &plan, "repair_id": REPAIR, "asset_ids": ["fleet_bindings"]}),
    ];
    for h in [w.h.clone(), wrong] {
        for body in &bodies {
            let r = post(&w.c, &h, PATH, body.clone()).await;
            expect(&r, 400, "elevation_required");
            assert_eq!(
                r.body["message"],
                "This action needs the owner's elevation."
            );
        }
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let rows = repair_rows(&w.c).await;
    assert_eq!(rows.len(), 6);
    let details: Vec<_> = rows.iter().filter_map(|e| e.detail.as_deref()).collect();
    for action in ["install_replace", "install_resume", "restore"] {
        let want = format!(
            "action={action} outcome=elevation_required code=- repair={} assets=0 replaced=0",
            if action == "install_replace" {
                "-"
            } else {
                REPAIR
            }
        );
        assert_eq!(
            details.iter().filter(|d| **d == want).count(),
            2,
            "{want}: {details:?}"
        );
    }
    assert!(rows.iter().all(|e| e.result == AuditResult::Refused));
}

#[tokio::test]
async fn omarchy_install_replace_passes_the_plan_and_the_fields_and_audits_no_path() {
    let mut w = setup().await;
    let plan = plan_for(6);
    let owner_headers = w.h.clone();
    read_plan(&mut w, &owner_headers, &plan).await;
    let h = elevated(&w).await;
    let token = h.last().unwrap().1.clone();
    let body = json!({
        "action": "install_replace", "plan_id": plan,
        "replace_recorded_edits": ["fleet_bindings"], "request_id": "req_1-A",
    });
    let (r, req) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, body),
        Outcome::Ok(act_result(
            "done",
            Value::Null,
            repair_json(Value::Null, "replaced"),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "omarchy.act");
    assert_eq!(
        req.params,
        json!({
            "action": "install_replace", "elevated": true, "plan_id": &plan,
            "replace_recorded_edits": ["fleet_bindings"], "request_id": "req_1-A",
        })
    );
    assert!(!req.params.to_string().contains(&token));
    assert_eq!(r.body["outcome"], "done");
    assert_eq!(r.body["repair"], repair_json(Value::Null, "replaced"));
    let rows = repair_rows(&w.c).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, AuditResult::Ok);
    assert_eq!(rows[0].target.as_deref(), Some("fake"));
    assert_eq!(rows[0].actor, "owner");
    assert_eq!(
        rows[0].detail.as_deref(),
        Some(
            format!(
                "action=install_replace outcome=done code=- repair={REPAIR} assets=2 replaced=1"
            )
            .as_str()
        )
    );
    // The old audit row name is not used by a repair action.
    let all = w.c.store.audit_log(100).await.unwrap();
    assert!(all.iter().all(|e| e.action != "station.omarchy"), "{all:?}");
    for e in all {
        let text = format!("{e:?}");
        assert!(!text.contains("/home/ann"), "{text}");
        assert!(!text.contains(".kept"), "{text}");
        assert!(!text.contains(&token), "{text}");
    }
}

#[tokio::test]
async fn omarchy_a_plan_is_used_once_and_an_unknown_plan_is_plan_changed() {
    let mut w = setup().await;
    let plan = plan_for(7);
    let owner_headers = w.h.clone();
    read_plan(&mut w, &owner_headers, &plan).await;
    let h = elevated(&w).await;
    let body = json!({"action": "install_replace", "plan_id": plan});
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, body.clone()),
        Outcome::Ok(act_result(
            "done",
            Value::Null,
            repair_json(Value::Null, "replaced"),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    for body in [
        body,
        json!({"action": "install_replace", "plan_id": "ab".repeat(16)}),
        json!({"action": "restore", "plan_id": "ab".repeat(16), "repair_id": REPAIR,
               "asset_ids": ["fleet_bindings"]}),
    ] {
        let r = post(&w.c, &h, PATH, body).await;
        expect(&r, 409, "plan_changed");
        assert_eq!(r.body["message"], PLAN_CHANGED);
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let rows = repair_rows(&w.c).await;
    assert_eq!(rows[0].result, AuditResult::Refused);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some(
            "action=restore outcome=plan_changed code=- repair=0123456789abcdef0123456789abcdef assets=0 replaced=0"
        )
    );
}

#[tokio::test]
async fn omarchy_a_plan_belongs_to_the_login_that_read_it() {
    let mut w = setup().await;
    let plan = plan_for(8);
    let owner_headers = w.h.clone();
    read_plan(&mut w, &owner_headers, &plan).await;
    // Another owner login, with its own elevation, cannot use the plan.
    let other = owner(&w.c).await;
    let other = elevated_for(&w.c, &other).await;
    let body = json!({"action": "install_replace", "plan_id": plan});
    let r = post(&w.c, &other, PATH, body.clone()).await;
    expect(&r, 409, "plan_changed");
    assert_eq!(r.body["message"], PLAN_CHANGED);
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // The refusal keeps the plan: the login that read it can still use it.
    let h = elevated(&w).await;
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, body),
        Outcome::Ok(act_result(
            "done",
            Value::Null,
            repair_json(Value::Null, "replaced"),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // A restore plan is bound in the same way.
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, &restore_path(REPAIR)),
        Outcome::Ok(restore_view(&plan)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let restore = json!({"action": "restore", "plan_id": &plan, "repair_id": REPAIR,
                         "asset_ids": ["fleet_bindings"]});
    let r = post(&w.c, &other, PATH, restore).await;
    expect(&r, 409, "plan_changed");
    no_request(&mut w.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn omarchy_restore_and_resume_pass_their_fields_and_count_the_restored_files() {
    let plan = plan_for(9);
    let mut w = setup().await;
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, &restore_path(REPAIR)),
        Outcome::Ok(restore_view(&plan)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let h = elevated(&w).await;
    let body = json!({"action": "restore", "plan_id": &plan, "repair_id": REPAIR,
                      "asset_ids": ["fleet_bindings", "hook_theme_set"]});
    let (r, req) = with_station(
        &mut w.station,
        post(&w.c, &h, PATH, body),
        Outcome::Ok(act_result(
            "done",
            Value::Null,
            repair_json(Value::Null, "restored"),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params,
        json!({"action": "restore", "elevated": true, "plan_id": &plan, "repair_id": REPAIR,
               "asset_ids": ["fleet_bindings", "hook_theme_set"]})
    );
    assert_eq!(
        repair_rows(&w.c).await[0].detail.as_deref(),
        Some(
            format!("action=restore outcome=done code=- repair={REPAIR} assets=2 replaced=1")
                .as_str()
        )
    );
    // A resume needs no plan. A `restored` file does not count as replaced.
    let (r, req) = with_station(
        &mut w.station,
        post(
            &w.c,
            &h,
            PATH,
            json!({"action": "install_resume", "repair_id": REPAIR}),
        ),
        Outcome::Ok(act_result(
            "done",
            Value::Null,
            repair_json(Value::Null, "restored"),
        )),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params,
        json!({"action": "install_resume", "elevated": true, "repair_id": REPAIR})
    );
    assert_eq!(
        repair_rows(&w.c).await[0].detail.as_deref(),
        Some(
            format!(
                "action=install_resume outcome=done code=- repair={REPAIR} assets=2 replaced=0"
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn omarchy_an_action_without_a_plan_goes_to_the_station_which_refuses_it() {
    let plan = plan_for(10);
    let mut w = setup().await;
    let h = elevated(&w).await;
    for body in [
        json!({"action": "install_replace"}),
        json!({"action": "restore", "repair_id": REPAIR}),
    ] {
        let (r, req) = with_station(
            &mut w.station,
            post(&w.c, &h, PATH, body),
            Outcome::Err(WireError::new("invalid_request", SECRET_TEXT)),
        )
        .await;
        expect(&r, 400, "invalid_request");
        assert_eq!(r.body["message"], "The request is not valid.");
        assert!(req.params.get("plan_id").is_none(), "{}", req.params);
    }
    // An old action with a plan ID is not checked against the plans.
    let (r, req) = with_station(
        &mut w.station,
        post(
            &w.c,
            &h,
            PATH,
            json!({"action": "install", "plan_id": &plan}),
        ),
        Outcome::Err(WireError::new("invalid_request", SECRET_TEXT)),
    )
    .await;
    expect(&r, 400, "invalid_request");
    assert_eq!(req.params["plan_id"], plan.as_str());
    // A body with a field that the deck does not know is `invalid_body`.
    let r = post(
        &w.c,
        &h,
        PATH,
        json!({"action": "install_resume", "path": "/x"}),
    )
    .await;
    expect(&r, 400, "invalid_body");
}

#[tokio::test]
async fn omarchy_the_six_repair_codes_have_their_texts() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let texts = [
        ("plan_changed", PLAN_CHANGED),
        (
            "unsafe_path",
            "Fleet Deck cannot safely replace one of these paths.",
        ),
        (
            "backup_failed",
            "Fleet Deck could not keep a copy of a file. The repair stopped.",
        ),
        (
            "repair_incomplete",
            "The repair stopped after changing some files. Review the kept copies and repair status.",
        ),
        (
            "restore_conflict",
            "A current file changed after the repair. Fleet Deck kept both versions and did not overwrite it.",
        ),
        (
            "build_mismatch",
            "This repair was started by another build. Restore the kept copies instead.",
        ),
    ];
    let resume = json!({"action": "install_resume", "repair_id": REPAIR});
    for (code, text) in texts {
        // A wire error: 409 with the code and the fixed text.
        let (r, _) = with_station(
            &mut w.station,
            post(&w.c, &h, PATH, resume.clone()),
            Outcome::Err(WireError::new(code, SECRET_TEXT)),
        )
        .await;
        expect(&r, 409, code);
        assert_eq!(r.body["message"], text, "{code}");
        assert!(!r.text.contains("SECRET-STATION-MESSAGE"), "{}", r.text);
        // A `failed` result keeps its repair field and its code.
        let repair = repair_json(json!(code), "backed_up");
        let (r, _) = with_station(
            &mut w.station,
            post(&w.c, &h, PATH, resume.clone()),
            Outcome::Ok(act_result("failed", json!(code), repair.clone())),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["outcome"], "failed");
        assert_eq!(r.body["code"], code);
        assert_eq!(r.body["repair"], repair);
        let rows = repair_rows(&w.c).await;
        assert_eq!(rows[0].result, AuditResult::Failed);
        assert_eq!(
            rows[0].detail.as_deref(),
            Some(format!("action=install_resume outcome=failed code={code} repair={REPAIR} assets=2 replaced=0").as_str())
        );
        assert_eq!(rows[1].result, AuditResult::Refused);
        assert_eq!(
            rows[1].detail.as_deref(),
            Some(format!("action=install_resume outcome={code} code=- repair={REPAIR} assets=0 replaced=0").as_str())
        );
    }
    // A code that the contract does not name, and a repair ID that is not
    // 32 hex characters, never reach the row.
    let (r, _) = with_station(
        &mut w.station,
        post(
            &w.c,
            &h,
            PATH,
            json!({"action": "install_resume", "repair_id": REPAIR}),
        ),
        Outcome::Ok(json!({
            "outcome": "failed", "code": SECRET_TEXT, "message": null,
            "status": status_json(),
            "repair": {"code": null, "repair_id": SECRET_TEXT, "files": []},
        })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        repair_rows(&w.c).await[0].detail.as_deref(),
        Some(format!("action=install_resume outcome=failed code=failed repair={REPAIR} assets=0 replaced=0").as_str())
    );
    for e in w.c.store.audit_log(200).await.unwrap() {
        assert!(!format!("{e:?}").contains("SECRET-STATION-MESSAGE"));
    }
}

#[tokio::test]
async fn omarchy_a_request_repair_id_that_is_not_hex_is_not_written_to_the_row() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let (r, _) = with_station(
        &mut w.station,
        post(
            &w.c,
            &h,
            PATH,
            json!({"action": "install_resume", "repair_id": "/home/ann/secret"}),
        ),
        Outcome::Err(WireError::new("invalid_request", SECRET_TEXT)),
    )
    .await;
    expect(&r, 400, "invalid_request");
    let rows = repair_rows(&w.c).await;
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("action=install_resume outcome=invalid_request code=- repair=- assets=0 replaced=0")
    );
}
