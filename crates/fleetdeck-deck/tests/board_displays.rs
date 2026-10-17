//! The display principal over loopback HTTPS (R13 P2a, DL-201): pairing by
//! an on-screen code, the one-time display link and the deny-by-default
//! guard. The reduced cards, the WebSocket and the owner API are in
//! `board_displays_more.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::time::Duration;

use displays_support::*;
use fleetdeck_deck::http::methods;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::displays::DISPLAY_MAX_MS;
use serde_json::{Value, json};
use share_support::*;

#[tokio::test]
async fn the_whole_pairing_flow_gives_a_read_only_display_login() {
    let (f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let p = start_pairing(&c).await;

    // The kiosk polls before the approval: still pending.
    let r = poll(&f, &c, &p, Some(&p.pending)).await;
    assert_eq!(
        (r.status, &r.body["state"]),
        (200, &json!("pending")),
        "{}",
        r.body
    );
    assert!(cookie_pair(&r, "fd_display").is_none());

    // The owner sees the pairing with its code, source and user agent.
    let list = get(&c, &h, "/api/displays").await;
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.body["pending"][0]["code"], p.code.as_str());
    assert_eq!(list.body["pending"][0]["pairing_id"], p.id.as_str());
    assert!(list.body["pending"][0]["source"].as_str().is_some());
    assert_eq!(list.body["displays"], json!([]));

    // An approval needs an elevation, the right code and a valid body.
    expect(
        &approve(&c, &h, &p, &[&st], None).await,
        400,
        "elevation_required",
    );
    let wrong = Pairing {
        id: p.id.clone(),
        code: if p.code == "000000" {
            "111111"
        } else {
            "000000"
        }
        .to_owned(),
        pending: p.pending.clone(),
    };
    expect(
        &approve(&c, &h, &wrong, &[&st], Some(&e)).await,
        409,
        "pairing_mismatch",
    );
    expect(
        &approve(&c, &h, &p, &[], Some(&e)).await,
        400,
        "invalid_display",
    );
    expect(
        &approve(&c, &h, &p, &["nope"], Some(&e)).await,
        400,
        "invalid_display",
    );
    let bad_code = Pairing {
        code: "12ab".to_owned(),
        ..Pairing {
            id: p.id.clone(),
            code: String::new(),
            pending: p.pending.clone(),
        }
    };
    expect(
        &approve(&c, &h, &bad_code, &[&st], Some(&e)).await,
        400,
        "invalid_display",
    );
    let r = approve(&c, &h, &p, &[&st], Some(&e)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["name"], "Wall");
    assert_eq!(r.body["ceiling"], json!([st]));
    assert_eq!(r.body["show_diffs"], false);
    assert_eq!(r.body["wake_on_needs_you"], true);
    // A second approval is refused.
    expect(
        &approve(&c, &h, &p, &[&st], Some(&e)).await,
        404,
        "pairing_unknown",
    );

    // A wrong pending cookie and a missing cookie (a code alone) claim nothing.
    expect(
        &poll(&f, &c, &p, Some("fd_display_pending=fdpend1_wrong")).await,
        404,
        "pairing_unknown",
    );
    expect(&poll(&f, &c, &p, None).await, 404, "pairing_unknown");
    // A session cookie does not claim either.
    expect(
        &poll(&f, &c, &p, Some(&h[0].1)).await,
        404,
        "pairing_unknown",
    );

    // The claim turns the pending cookie into the display cookie, once.
    let r = poll(&f, &c, &p, Some(&p.pending)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "approved_claimed");
    let display = cookie_pair(&r, "fd_display").expect("the display cookie");
    assert!(display.starts_with("fd_display=fdisp1_"), "{display}");
    let set = set_cookies(&r).join("|");
    assert!(
        set.contains(&format!("Max-Age={}", DISPLAY_MAX_MS / 1000)),
        "{set}"
    );
    assert!(
        set.contains("fd_display_pending=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"),
        "{set}"
    );
    expect(
        &poll(&f, &c, &p, Some(&p.pending)).await,
        404,
        "pairing_unknown",
    );

    // The display cookie works: self, and the session view.
    let me = get_with(&c, "/api/displays/self", &display).await;
    assert_eq!(me.status, 200, "{}", me.body);
    assert_eq!(me.body["name"], "Wall");
    assert_eq!(me.body["ceiling"], json!([st]));
    assert_eq!(me.body["binding"], Value::Null);
    let s = get_with(&c, "/api/auth/session", &display).await;
    assert_eq!(s.body["actor"], "display:Wall", "{}", s.body);
    assert_eq!(s.body["scopes"], json!(["read"]));
    assert_eq!(s.body["csrf"], Value::Null);

    // No audit row holds the code, a cookie or a secret.
    let rows = format!("{:?}", c.store.audit_log(200).await.unwrap());
    for secret in [
        p.code.as_str(),
        "fdpend1_",
        "fdisp1_",
        display.split('=').nth(1).unwrap(),
    ] {
        assert!(!rows.contains(secret), "{secret} in {rows}");
    }
    assert!(
        !c.audited("display.pair_start", AuditResult::Ok)
            .await
            .is_empty()
    );
    assert!(
        !c.audited("display.pair_approve", AuditResult::Ok)
            .await
            .is_empty()
    );
    assert!(
        !c.audited("display.pair_claim", AuditResult::Ok)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn an_expired_pairing_cannot_be_approved_or_claimed() {
    let (f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let p = start_pairing(&c).await;
    f.det.advance(Duration::from_secs(5 * 60 + 1));
    expect(
        &approve(&c, &h, &p, &[&st], Some(&e)).await,
        404,
        "pairing_unknown",
    );
    expect(
        &poll(&f, &c, &p, Some(&p.pending)).await,
        404,
        "pairing_unknown",
    );
    // An approved pairing that nobody claims expires after 2 minutes.
    let e = elevate(&c, &h).await;
    let q = start_pairing(&c).await;
    assert_eq!(approve(&c, &h, &q, &[&st], Some(&e)).await.status, 200);
    f.det.advance(Duration::from_secs(2 * 60 + 1));
    expect(
        &poll(&f, &c, &q, Some(&q.pending)).await,
        404,
        "pairing_unknown",
    );
    let list = get(&c, &h, "/api/displays").await;
    assert_eq!(list.body["pending"], json!([]));
}

#[tokio::test]
async fn two_concurrent_claims_give_one_display_session() {
    let (f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let p = start_pairing(&c).await;
    assert_eq!(approve(&c, &h, &p, &[&st], Some(&e)).await.status, 200);
    f.det.advance(Duration::from_secs(2));
    let path = format!("/api/displays/pair/{}", p.id);
    let h1 = [("Cookie", p.pending.as_str())];
    let (a, b) = tokio::join!(c.get(&path, &h1), c.get(&path, &h1));
    let claimed = [&a, &b]
        .iter()
        .filter(|r| r.status == 200 && r.body["state"] == "approved_claimed")
        .count();
    assert_eq!(claimed, 1, "{} / {}", a.body, b.body);
    let other = if a.status == 200 { &b } else { &a };
    assert!([404, 429].contains(&other.status), "{}", other.body);
}

#[tokio::test]
async fn the_pairing_limits_are_503_at_ten_rows_and_429_per_source() {
    let (_f, c, _h, _st) = setup_deck().await;
    // 3 starts in a minute are fine; the fourth waits.
    for _ in 0..3 {
        start_pairing(&c).await;
    }
    let r = c.post("/api/displays/pair", &[], json!({})).await;
    expect(&r, 429, "too_many_requests");
    assert!(
        r.body["message"]
            .as_str()
            .unwrap()
            .starts_with("Too many display pairings")
    );
    assert!(
        !c.audited("display.pair_start", AuditResult::Refused)
            .await
            .is_empty()
    );

    // Ten pairing rows in the store: the next start is 503. (The source
    // limit is another deck here, so it does not answer first.)
    let (_f3, c3, _h3, _st3) = setup_deck().await;
    for i in 0..10 {
        c3.store
            .pairing_create_with("x".to_owned(), String::new(), move || Ok(format!("{i:06}")))
            .await
            .unwrap();
    }
    let r = c3.post("/api/displays/pair", &[], json!({})).await;
    expect(&r, 503, "pairing_busy");
    assert_eq!(
        r.body["message"],
        "Too many display pairings; try again later"
    );
}

#[tokio::test]
async fn a_display_link_works_once_and_not_after_five_minutes() {
    let (f, c, h, st) = setup_deck().await;
    let body = |e: Option<&str>| {
        let mut b = json!({ "name": "Hall", "ceiling": [st] });
        if let Some(e) = e {
            b["elevation"] = json!(e);
        }
        b
    };
    expect(
        &post(&c, &h, "/api/displays/link", body(None)).await,
        400,
        "elevation_required",
    );
    let e = elevate(&c, &h).await;
    // A bad ceiling creates nothing.
    expect(
        &post(
            &c,
            &h,
            "/api/displays/link",
            json!({ "name": "Hall", "ceiling": ["nope"], "elevation": e }),
        )
        .await,
        400,
        "invalid_display",
    );
    assert_eq!(
        get(&c, &h, "/api/displays").await.body["displays"],
        json!([])
    );
    let r = post(&c, &h, "/api/displays/link", body(Some(&e))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("https://"), "{url}");
    let token = url.split("#display=").nth(1).unwrap().to_owned();
    assert_eq!(
        r.body["expires_at"].as_i64().unwrap() - f.store.now_ms(),
        5 * 60 * 1000
    );
    // The deck never stores the token.
    assert!(
        !c.store
            .audit_log(100)
            .await
            .unwrap()
            .iter()
            .any(|a| format!("{a:?}").contains(&token))
    );

    let ok = c
        .post("/api/auth/display-link", &[], json!({ "token": token }))
        .await;
    assert_eq!(ok.status, 204, "{}", ok.text);
    let cookie = cookie_pair(&ok, "fd_display").unwrap();
    assert_eq!(
        get_with(&c, "/api/displays/self", &cookie).await.status,
        200
    );
    expect(
        &c.post("/api/auth/display-link", &[], json!({ "token": token }))
            .await,
        401,
        "invalid_credentials",
    );

    // A link that waits 5 minutes is dead.
    let r = post(&c, &h, "/api/displays/link", body(Some(&e))).await;
    let late = r.body["url"]
        .as_str()
        .unwrap()
        .split("#display=")
        .nth(1)
        .unwrap()
        .to_owned();
    f.det.advance(Duration::from_secs(5 * 60 + 1));
    expect(
        &c.post("/api/auth/display-link", &[], json!({ "token": late }))
            .await,
        401,
        "invalid_credentials",
    );
    // A display cookie is not a login link and a login link is not a display link.
    let login = c.store.create_login_link().await.unwrap();
    expect(
        &c.post(
            "/api/auth/display-link",
            &[],
            json!({ "token": login.expose() }),
        )
        .await,
        401,
        "invalid_credentials",
    );
}

#[tokio::test]
async fn the_guard_refuses_a_display_on_every_row_without_the_display_marker() {
    let (_f, c, _h, st) = setup_deck().await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let origin = c.origin();
    let mut refused = 0;
    let mut unrouted = Vec::new();
    for api in methods::api() {
        if api.scope.is_none() || api.display {
            continue;
        }
        let mut path = methods::route_of(api);
        while let Some(i) = path.find('{') {
            let j = path[i..].find('}').unwrap() + i;
            path.replace_range(i..=j, "x");
        }
        let body = api.mutating.then(|| json!({}));
        let r = c
            .req(
                api.method.as_str(),
                &path,
                &[("Cookie", &cookie), ("Origin", &origin)],
                body,
            )
            .await;
        if r.status == 404 && r.code() == "not_found" {
            unrouted.push(api.name);
            continue;
        }
        assert_eq!(
            (r.status, r.code()),
            (403, "display_read_only"),
            "{} {}",
            api.name,
            r.body
        );
        refused += 1;
    }
    assert!(refused > 100, "{refused}");
    assert!(unrouted.is_empty(), "rows without a route: {unrouted:?}");
    // The refusals of mutating rows are audited as one action.
    assert!(
        !c.audited("display.refused", AuditResult::Refused)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_display_reads_and_logs_out_with_its_header_and_writes_nothing_else() {
    let (_f, c, h, st) = setup_deck().await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let origin = c.origin();
    for path in [
        "/api/sessions",
        "/api/stations",
        "/api/auth/session",
        "/api/displays/self",
    ] {
        let r = c
            .get(path, &[("Cookie", &cookie), ("Origin", &origin)])
            .await;
        assert_eq!(r.status, 200, "{path} {}", r.body);
    }
    // The ticket route works for a display.
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    assert_eq!(t.status, 200, "{}", t.body);
    // The owner-only reads are closed to it.
    for path in ["/api/displays", "/api/audit", "/api/commands"] {
        expect(
            &c.get(path, &[("Cookie", &cookie)]).await,
            403,
            "display_read_only",
        );
    }
    // The changes row is built (R13 P2b changes): an unknown session is a
    // 404. The artifact row is open to a display, but its projection belongs
    // to the P2b artifacts task: until then a display gets the defined
    // refusal and never the owner payload (R13 P2b/P3b step 1).
    expect(
        &c.get(
            "/api/displays/self/sessions/x/y/changes",
            &[("Cookie", &cookie)],
        )
        .await,
        404,
        "unknown_session",
    );
    expect(
        &c.get(
            "/api/displays/self/artifact/x/y/1/index.html",
            &[("Cookie", &cookie)],
        )
        .await,
        404,
        "unknown_artifact",
    );
    // The display cookie does not log in the owner's rows, and the owner's
    // cookie is not a display cookie.
    let owner_cookie = &h[0].1;
    let as_display = owner_cookie.replace("fd_session", "fd_display");
    expect(
        &c.get("/api/displays/self", &[("Cookie", &as_display)])
            .await,
        401,
        "login_required",
    );
    expect(
        &c.get("/api/displays/self", &[("Cookie", owner_cookie)])
            .await,
        401,
        "login_required",
    );

    // Logout needs the display header and clears only fd_display.
    expect(
        &c.post("/api/auth/logout", &[("Cookie", &cookie)], json!({}))
            .await,
        403,
        "csrf",
    );
    let out = c
        .post(
            "/api/auth/logout",
            &[("Cookie", &cookie), ("X-Fleetdeck-Display", "1")],
            json!({}),
        )
        .await;
    assert_eq!(out.status, 204);
    assert_eq!(set_cookies(&out).len(), 1);
    assert!(
        set_cookies(&out)[0].starts_with("fd_display=; "),
        "{:?}",
        set_cookies(&out)
    );
    expect(
        &c.get("/api/displays/self", &[("Cookie", &cookie)]).await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn both_cookies_in_one_browser_each_route_sees_the_right_principal() {
    let (_f, c, h, st) = setup_deck().await;
    let (_, display) = display_login(&c, "Wall", &[&st]).await;
    let both = format!("{}; {display}", h[0].1);
    // A board route answers the owner, the display route the display.
    let s = c.get("/api/auth/session", &[("Cookie", &both)]).await;
    assert_eq!(s.body["actor"], "owner", "{}", s.body);
    let me = c.get("/api/displays/self", &[("Cookie", &both)]).await;
    assert_eq!(me.body["name"], "Wall", "{}", me.body);
    // The display header selects the display for the session view.
    let s = c
        .get(
            "/api/auth/session",
            &[("Cookie", &both), ("X-Fleetdeck-Display", "1")],
        )
        .await;
    assert_eq!(s.body["actor"], "display:Wall", "{}", s.body);
    // The owner's logout clears only fd_session.
    let out = c
        .post(
            "/api/auth/logout",
            &[("Cookie", &both), ("X-CSRF-Token", &h[1].1)],
            json!({}),
        )
        .await;
    assert_eq!(out.status, 204);
    assert!(
        set_cookies(&out)[0].starts_with("fd_session=; "),
        "{:?}",
        set_cookies(&out)
    );
    // The display is still logged in; its logout clears only fd_display.
    let (owner2, _csrf) = c.login().await;
    let both = format!("{owner2}; {display}");
    let out = c
        .post(
            "/api/auth/logout",
            &[("Cookie", &both), ("X-Fleetdeck-Display", "1")],
            json!({}),
        )
        .await;
    assert_eq!(out.status, 204, "{}", out.text);
    assert!(
        set_cookies(&out)[0].starts_with("fd_display=; "),
        "{:?}",
        set_cookies(&out)
    );
    assert_eq!(
        c.get("/api/auth/session", &[("Cookie", &owner2)])
            .await
            .status,
        200
    );
    expect(
        &c.get("/api/displays/self", &[("Cookie", &display)]).await,
        401,
        "login_required",
    );
}
