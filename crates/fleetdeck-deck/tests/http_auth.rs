//! The board API guard over real loopback HTTPS (M2 P3-8; PLAN §9, A13,
//! L15, DL-054): plain HTTP gets no answer; login is required also on loopback; the one-time login link; the
//! password login with its failure limit; the strict cookie; CSRF on
//! mutating cookie requests; the Origin check; bearer tokens with
//! per-method scopes; the audit log of refusals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, board};
use fleetdeck_core::Secret;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use fleetdeck_testkit::Det;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Fixture {
    det: Det,
    client: BoardClient,
    _station: common::Fixture,
}

impl std::ops::Deref for Fixture {
    type Target = BoardClient;
    fn deref(&self) -> &BoardClient {
        &self.client
    }
}

async fn fixture() -> Fixture {
    let station = common::fixture().await;
    let det = station.det.clone();
    let client = board(Arc::clone(&station.ctx)).await;
    Fixture {
        det,
        client,
        _station: station,
    }
}

const READS: [&str; 5] = [
    "/api/sessions",
    "/api/stations",
    "/api/commands",
    "/api/audit",
    "/api/auth/session",
];

#[tokio::test]
async fn login_is_required_on_loopback_for_every_method_but_the_login_methods() {
    let f = fixture().await;
    for path in READS {
        let r = f.get(path, &[]).await;
        assert_eq!((r.status, r.code()), (401, "login_required"), "{path}");
    }
    let r = f.post("/api/auth/logout", &[], json!({})).await;
    assert_eq!((r.status, r.code()), (401, "login_required"));
    // A cookie that names no login is refused the same way.
    let r = f
        .get("/api/sessions", &[("Cookie", "fd_session=forged")])
        .await;
    assert_eq!(r.status, 401);
    // No route outside the method table.
    assert_eq!(f.get("/api/nope", &[]).await.status, 404);
    // The refused logout is audited.
    assert_eq!(
        f.audited("auth.logout", AuditResult::Refused).await,
        vec![Some("login_required".to_owned())]
    );
}

#[tokio::test]
async fn a_login_link_works_once_and_sets_a_strict_http_only_cookie() {
    let f = fixture().await;
    let token = f.store.create_login_link().await.unwrap();
    let r = f
        .post("/api/auth/link", &[], json!({ "token": token.expose() }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let set = r.header("set-cookie").unwrap().to_owned();
    for attr in [
        "fd_session=",
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(set.contains(attr), "{set}");
    }
    assert_eq!(r.body["actor"], "owner");
    let cookie = set.split(';').next().unwrap().to_owned();
    for path in READS {
        let r = f.get(path, &[("Cookie", &cookie)]).await;
        assert_eq!(r.status, 200, "{path}: {}", r.body);
    }
    let again = f
        .post("/api/auth/link", &[], json!({ "token": token.expose() }))
        .await;
    assert_eq!((again.status, again.code()), (401, "invalid_credentials"));
    assert_eq!(f.audited("auth.link", AuditResult::Ok).await.len(), 1);
}

#[tokio::test]
async fn password_logins_are_limited_after_five_failures_for_a_minute() {
    let f = fixture().await;
    // No password is set: every password fails.
    let r = f
        .post("/api/auth/login", &[], json!({ "password": "" }))
        .await;
    assert_eq!(r.status, 401);
    f.store
        .set_password(Secret::new("correct horse battery".to_owned()))
        .await
        .unwrap();
    for _ in 0..4 {
        let r = f
            .post("/api/auth/login", &[], json!({ "password": "wrong" }))
            .await;
        assert_eq!((r.status, r.code()), (401, "invalid_credentials"));
    }
    let good = json!({ "password": "correct horse battery" });
    let r = f.post("/api/auth/login", &[], good.clone()).await;
    assert_eq!((r.status, r.code()), (429, "too_many_attempts"));
    f.det.advance(Duration::from_secs(61));
    let r = f.post("/api/auth/login", &[], good).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.header("set-cookie").unwrap().contains("HttpOnly"));
    // An unknown field is refused, so a typo never logs in.
    let r = f
        .post(
            "/api/auth/login",
            &[],
            json!({ "password": "x", "user": "y" }),
        )
        .await;
    assert_eq!(r.status, 422);
    let refused = f.audited("auth.login", AuditResult::Refused).await;
    assert_eq!(refused.len(), 6, "{refused:?}");
    assert!(refused.contains(&Some("too_many_attempts".to_owned())));
}

#[tokio::test]
async fn a_mutating_cookie_request_needs_the_csrf_token_of_its_login() {
    let f = fixture().await;
    let (cookie, csrf) = f.login().await;
    let (_, other_csrf) = f.login().await;
    for headers in [
        vec![("Cookie", cookie.as_str())],
        vec![
            ("Cookie", cookie.as_str()),
            ("X-CSRF-Token", other_csrf.as_str()),
        ],
    ] {
        let r = f.post("/api/auth/logout", &headers, json!({})).await;
        assert_eq!((r.status, r.code()), (403, "csrf"));
    }
    let r = f
        .post(
            "/api/auth/logout",
            &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
            json!({}),
        )
        .await;
    assert_eq!(r.status, 204);
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=0"));
    let r = f.get("/api/sessions", &[("Cookie", &cookie)]).await;
    assert_eq!(r.status, 401, "the login has ended");
    assert_eq!(
        f.audited("auth.logout", AuditResult::Refused).await,
        vec![Some("csrf".to_owned()), Some("csrf".to_owned())]
    );
}

#[tokio::test]
async fn only_the_origins_of_the_board_addresses_and_the_tailnet_name_are_allowed() {
    let f = fixture().await;
    let (cookie, _) = f.login().await;
    let port = f.addr.port();
    for origin in [
        format!("https://127.0.0.1:{port}"),
        format!("https://localhost:{port}"),
        format!("https://deck.tail1234.ts.net:{port}"),
    ] {
        let r = f
            .get("/api/sessions", &[("Cookie", &cookie), ("Origin", &origin)])
            .await;
        assert_eq!(r.status, 200, "{origin}");
    }
    for origin in [
        "http://evil.example".to_owned(),
        // The board serves HTTPS only; port 443 is not the board's port.
        format!("http://127.0.0.1:{port}"),
        "https://deck.tail1234.ts.net".to_owned(),
        format!("https://127.0.0.1:{}", port.wrapping_add(1)),
        "null".to_owned(),
    ] {
        let r = f
            .get("/api/sessions", &[("Cookie", &cookie), ("Origin", &origin)])
            .await;
        assert_eq!((r.status, r.code()), (403, "origin"), "{origin}");
    }
    // Also a login from a foreign page is refused.
    let token = f.store.create_login_link().await.unwrap();
    let r = f
        .post(
            "/api/auth/link",
            &[("Origin", "http://evil.example")],
            json!({ "token": token.expose() }),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "origin"));
}

#[tokio::test]
async fn bearer_tokens_hold_per_method_scopes_and_never_fall_back_to_the_cookie() {
    let f = fixture().await;
    let (cookie, _) = f.login().await;
    let day = 24 * 60 * 60 * 1000;
    let (_, read) = f
        .store
        .create_token("phone".into(), BTreeSet::from([Scope::Read]), day)
        .await
        .unwrap();
    let (_, admin) = f
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Admin]),
            day,
        )
        .await
        .unwrap();
    let read = format!("Bearer {}", read.expose());
    let admin = format!("Bearer {}", admin.expose());

    let r = f.get("/api/sessions", &[("Authorization", &read)]).await;
    assert_eq!(r.status, 200);
    let r = f.get("/api/audit", &[("Authorization", &read)]).await;
    assert_eq!((r.status, r.code()), (403, "forbidden"));
    let r = f.get("/api/audit", &[("Authorization", &admin)]).await;
    assert_eq!(r.status, 200);
    let r = f
        .get("/api/auth/session", &[("Authorization", &read)])
        .await;
    assert_eq!(r.body["actor"], "token:phone");
    assert_eq!(r.body["csrf"], Value::Null);
    assert_eq!(r.body["deck_version"], env!("CARGO_PKG_VERSION"));

    // A bad bearer value is refused even with a valid cookie.
    let r = f
        .get(
            "/api/sessions",
            &[
                ("Authorization", "Bearer fdtok1_forged"),
                ("Cookie", &cookie),
            ],
        )
        .await;
    assert_eq!(r.status, 401);
    // A bearer client needs no CSRF token (no ambient credential).
    let r = f
        .post("/api/auth/logout", &[("Authorization", &read)], json!({}))
        .await;
    assert_eq!(r.status, 204);
    // Revoked and expired tokens stop working.
    assert!(f.store.revoke_token("phone".into()).await.unwrap());
    assert_eq!(
        f.get("/api/sessions", &[("Authorization", &read)])
            .await
            .status,
        401
    );
    f.det.advance(Duration::from_millis(day as u64 + 1));
    assert_eq!(
        f.get("/api/sessions", &[("Authorization", &admin)])
            .await
            .status,
        401
    );
}

#[tokio::test]
async fn plain_http_gets_no_answer_and_the_cookie_is_secure() {
    let f = fixture().await;
    let mut s = TcpStream::connect(f.addr).await.unwrap();
    s.write_all(b"GET /api/sessions HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut raw))
        .await
        .expect("the board closes the connection");
    assert!(
        !String::from_utf8_lossy(&raw).starts_with("HTTP/"),
        "a plain HTTP request got an HTTP answer"
    );

    let token = f.store.create_login_link().await.unwrap();
    let r = f
        .post("/api/auth/link", &[], json!({ "token": token.expose() }))
        .await;
    assert_eq!(r.status, 200);
    let cookie = r.header("set-cookie").unwrap();
    for attr in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(attr), "{cookie}");
    }
}
