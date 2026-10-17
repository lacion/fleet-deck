//! The foundation of P2b/P3b (R13 step 1) over loopback HTTPS: the display
//! document, the managed launch and the existing-display approval, the
//! binding settings, and the defined refusal of the stub routes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::sync::Arc;

use common::board::{BoardClient, board_with_assets};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::http::assets::BoardAssets;
use fleetdeck_deck::store::displays::NewDisplay;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use serde_json::json;
use share_support::*;

async fn plain_deck(dir: &std::path::Path, assets: BoardAssets) -> (Arc<DeckNode>, BoardClient) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let node = DeckNode::start(
        store,
        Arc::new(Identity::generate().unwrap()),
        &["127.0.0.1:0".parse().unwrap()],
        BackoffPolicy::default(),
    )
    .await
    .unwrap();
    let board = board_with_assets(Arc::clone(&node), assets).await;
    (node, board)
}

#[tokio::test]
async fn the_display_document_has_its_own_policy_and_the_slash_form_redirects() {
    let tmp = tempfile::tempdir().unwrap();
    let dist = tmp.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("index.html"), "<p>board</p>").unwrap();
    let (_node, c) = plain_deck(tmp.path(), BoardAssets::Dir(dist)).await;
    let doc = c.get("/display", &[]).await;
    assert_eq!(doc.status, 200);
    assert_eq!(doc.text, "<p>board</p>");
    let csp = doc.header("content-security-policy").unwrap();
    let frame = format!("frame-src https://{}/api/displays/self/artifact/;", c.addr);
    assert!(csp.contains(&frame), "{csp}");
    assert!(!csp.contains("frame-src 'self'"), "{csp}");
    assert!(csp.contains("script-src 'self'"), "{csp}");
    // The board at `/` keeps its own policy.
    let board = c.get("/", &[]).await;
    assert!(
        board
            .header("content-security-policy")
            .unwrap()
            .contains("frame-src 'self'")
    );
    // `/display/` answers 308 to `/display`.
    let slash = c.get("/display/", &[]).await;
    assert_eq!(slash.status, 308);
    assert_eq!(slash.header("location"), Some("/display"));
}

#[tokio::test]
async fn a_deck_without_board_files_answers_the_display_document_with_a_404() {
    let (_f, c, _h, _st) = setup_deck().await;
    assert_eq!(c.get("/display", &[]).await.status, 404);
}

#[tokio::test]
async fn a_display_link_names_the_display_document() {
    let (_f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/displays/link",
        json!({ "name": "Wall", "ceiling": [st], "elevation": e }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap();
    assert!(url.contains("/display#display=fddlink1_"), "{url}");
    assert!(!url.contains("/#display="), "{url}");
}

#[tokio::test]
async fn a_managed_launch_pairs_into_its_display_after_an_elevated_approval() {
    let (f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let station = StationId::new(st.as_str());
    let d = c
        .store
        .create_display(NewDisplay {
            name: "Wall".into(),
            ceiling: vec![station.clone()],
        })
        .await
        .unwrap();
    let (old_cookie, _) = c
        .store
        .create_display_session(d.display_id.clone())
        .await
        .unwrap();
    let ticket = c
        .store
        .display_begin_open(d.display_id.clone(), station, "DP-1".into())
        .await
        .unwrap();
    let started = c
        .post(
            "/api/displays/pair",
            &[],
            json!({ "expected_display_id": d.display_id, "operation_revision": ticket.op_revision }),
        )
        .await;
    assert_eq!(started.status, 200, "{}", started.body);
    let p = Pairing {
        id: started.body["pairing_id"].as_str().unwrap().to_owned(),
        code: started.body["code"].as_str().unwrap().to_owned(),
        pending: cookie_pair(&started, "fd_display_pending").unwrap(),
    };
    // The owner's list names the expected display.
    let list = get(&c, &h, "/api/displays").await;
    assert_eq!(
        list.body["pending"][0]["expected_display_id"],
        d.display_id.as_str()
    );
    let path = format!("/api/displays/pair/{}/approve", p.id);
    // Both forms at once, and neither form, are refused.
    for body in [
        json!({ "code": p.code, "display_id": d.display_id, "name": "x", "ceiling": [st], "elevation": e }),
        json!({ "code": p.code, "elevation": e }),
    ] {
        expect(&post(&c, &h, &path, body).await, 400, "invalid_display");
    }
    // An approval for another display or a new display is a mismatch.
    let other = c
        .store
        .create_display(NewDisplay {
            name: "Other".into(),
            ceiling: vec![StationId::new(st.as_str())],
        })
        .await
        .unwrap();
    expect(
        &post(
            &c,
            &h,
            &path,
            json!({ "code": p.code, "display_id": other.display_id, "elevation": e }),
        )
        .await,
        409,
        "display_mismatch",
    );
    expect(
        &post(&c, &h, &path, approve_body(&p, &[&st], Some(&e))).await,
        409,
        "display_mismatch",
    );
    // The existing display needs an elevation.
    expect(
        &post(
            &c,
            &h,
            &path,
            json!({ "code": p.code, "display_id": d.display_id }),
        )
        .await,
        400,
        "elevation_required",
    );
    let ok = post(
        &c,
        &h,
        &path,
        json!({ "code": p.code, "display_id": d.display_id, "elevation": e }),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["display_id"], d.display_id.as_str());
    // The kiosk claims: a new cookie, and the old session is gone.
    let claimed = poll(&f, &c, &p, Some(&p.pending)).await;
    assert_eq!(claimed.status, 200, "{}", claimed.body);
    assert_eq!(claimed.body["state"], "approved_claimed");
    let fresh = cookie_pair(&claimed, "fd_display").unwrap();
    assert_eq!(get_with(&c, "/api/displays/self", &fresh).await.status, 200);
    expect(
        &get_with(
            &c,
            "/api/displays/self",
            &format!("fd_display={}", old_cookie.expose()),
        )
        .await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn a_launch_that_was_replaced_is_superseded_at_the_claim() {
    let (f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let station = StationId::new(st.as_str());
    let d = c
        .store
        .create_display(NewDisplay {
            name: "Wall".into(),
            ceiling: vec![station.clone()],
        })
        .await
        .unwrap();
    let t = c
        .store
        .display_begin_open(d.display_id.clone(), station.clone(), "DP-1".into())
        .await
        .unwrap();
    let started = c
        .post(
            "/api/displays/pair",
            &[],
            json!({ "expected_display_id": d.display_id, "operation_revision": t.op_revision }),
        )
        .await;
    let p = Pairing {
        id: started.body["pairing_id"].as_str().unwrap().to_owned(),
        code: started.body["code"].as_str().unwrap().to_owned(),
        pending: cookie_pair(&started, "fd_display_pending").unwrap(),
    };
    let ok = post(
        &c,
        &h,
        &format!("/api/displays/pair/{}/approve", p.id),
        json!({ "code": p.code, "display_id": d.display_id, "elevation": e }),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    // A newer launch moves the operation revision on.
    c.store
        .display_begin_open(d.display_id.clone(), station, "DP-1".into())
        .await
        .unwrap();
    let r = poll(&f, &c, &p, Some(&p.pending)).await;
    expect(&r, 409, "superseded");
    assert!(cookie_pair(&r, "fd_display").is_none());
}

#[tokio::test]
async fn the_pair_start_body_is_optional_and_strict() {
    let (_f, c, _h, _st) = setup_deck().await;
    for bad in [
        json!({ "expected_display_id": "short", "operation_revision": 1 }),
        json!({ "expected_display_id": "01J0000000000000000000ABCD" }),
        json!({ "operation_revision": 1, "other": 2 }),
    ] {
        let r = c.post("/api/displays/pair", &[], bad.clone()).await;
        assert_eq!((r.status, r.code()), (400, "invalid_body"), "{bad}");
    }
    assert_eq!(
        c.post("/api/displays/pair", &[], json!({})).await.status,
        200
    );
}

#[tokio::test]
async fn the_owner_sets_clears_and_checks_the_binding_in_the_settings() {
    let (_f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let d = c
        .store
        .create_display(NewDisplay {
            name: "Wall".into(),
            ceiling: vec![StationId::new(st.as_str())],
        })
        .await
        .unwrap();
    let path = format!("/api/displays/{}/settings", d.display_id);
    let bind = json!({ "station_id": st, "monitor": "DP-1" });
    // Setting it needs an elevation.
    expect(
        &post(&c, &h, &path, json!({ "binding": bind })).await,
        400,
        "elevation_required",
    );
    let set = post(&c, &h, &path, json!({ "binding": bind, "elevation": e })).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(set.body["binding"]["monitor"], "DP-1");
    // The same binding again changes nothing and needs no elevation.
    let again = post(&c, &h, &path, json!({ "binding": bind })).await;
    assert_eq!(again.status, 200, "{}", again.body);
    // Bad values.
    for bad in [
        json!({ "station_id": st, "monitor": "a b" }),
        json!({ "station_id": "unknown", "monitor": "DP-1" }),
    ] {
        expect(
            &post(&c, &h, &path, json!({ "binding": bad, "elevation": e })).await,
            400,
            "invalid_display",
        );
    }
    // Clearing needs no elevation.
    let cleared = post(&c, &h, &path, json!({ "binding": null })).await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert!(cleared.body["binding"].is_null());
    // A display login cannot reach the settings.
    let (_, cookie) = display_login(&c, "Other", &[&st]).await;
    expect(
        &c.post(&path, &[("Cookie", &cookie)], json!({})).await,
        403,
        "display_read_only",
    );
}

#[tokio::test]
async fn the_stub_routes_refuse_with_not_built_and_the_self_routes_need_the_display_cookie() {
    let (_f, c, h, st) = setup_deck().await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    // The owner rows of the stubs.
    expect(
        &get(&c, &h, &format!("/api/stations/{st}/desktop")).await,
        501,
        "not_built",
    );
    expect(
        &post(
            &c,
            &h,
            &format!("/api/stations/{st}/desktop/act"),
            json!({}),
        )
        .await,
        501,
        "not_built",
    );
    // A display never reaches an owner row.
    for (method, path) in [
        ("POST", "/api/displays/x/directive".to_owned()),
        ("GET", format!("/api/stations/{st}/desktop")),
        ("POST", format!("/api/stations/{st}/desktop/act")),
    ] {
        let body = (method == "POST").then(|| json!({}));
        let r = c
            .req(
                method,
                &path,
                &[("Cookie", &cookie), ("Origin", &c.origin())],
                body,
            )
            .await;
        assert_eq!((r.status, r.code()), (403, "display_read_only"), "{path}");
    }
    // The owner cookie alone does not log in a `self` row; with both
    // cookies the display principal answers.
    for path in [
        "/api/displays/self/sessions/a/b/changes",
        "/api/displays/self/artifact/a/b/1/index.html",
    ] {
        expect(&get(&c, &h, path).await, 401, "login_required");
        let both = format!("{}; {cookie}", h[0].1);
        expect(&c.get(path, &[("Cookie", &both)]).await, 501, "not_built");
    }
    // The directive row is built (R13 P2b directives): the owner cookie alone
    // is refused, and with both cookies the display answers.
    expect(
        &get(&c, &h, "/api/displays/self/directive").await,
        401,
        "login_required",
    );
    let both = format!("{}; {cookie}", h[0].1);
    let r = c
        .get("/api/displays/self/directive", &[("Cookie", &both)])
        .await;
    assert_eq!(
        (r.status, r.body["directive"]["mode"].as_str()),
        (200, Some("mission"))
    );
}
