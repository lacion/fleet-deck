//! `POST /api/auth/device-link` over real loopback HTTPS (DL-146): the owner
//! mints a one-time login link for a new device; a token cannot; the CSRF
//! check and the rate limit apply. The order and the labels of the links
//! for a board with a LAN address are tested in `http::guard`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::board::{BoardClient, board};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

const PATH: &str = "/api/auth/device-link";

async fn fixture() -> (common::Fixture, BoardClient) {
    let station = common::fixture().await;
    let client = board(Arc::clone(&station.ctx)).await;
    (station, client)
}

async fn link_count(c: &BoardClient) -> i64 {
    c.store
        .db()
        .call(|conn| {
            conn.query_row("SELECT COUNT(*) FROM login_links", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap()
}

fn token_of(url: &str) -> &str {
    url.split_once("/#login=").unwrap().1
}

#[tokio::test]
async fn the_owner_gets_a_link_whose_token_logs_in_once() {
    let (_station, c) = fixture().await;
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    let r = c.post(PATH, &h, Value::Null).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["expires_at"].as_i64().unwrap() > 0);
    assert_eq!(r.body["self_signed"], true);
    assert_eq!(r.body["certificate_sha256"], "00".repeat(32));
    let links = r.body["links"].as_array().unwrap();
    // The test board listens on loopback only: the tailnet name resolves to
    // a tailnet address that the board does not serve, so no link offers it.
    let labels: Vec<&str> = links.iter().map(|l| l["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["This computer", "This computer"]);
    // Each entry carries the certificate it gets and its trust. The test
    // board has only its self-signed certificate.
    let first = links[0]["certificate_sha256"].as_str().unwrap();
    assert_eq!(first.split(':').count(), 32, "{first}");
    for l in links {
        assert_eq!(l["certificate_sha256"], first);
        assert_eq!(l["trust"], "self_signed");
    }
    let port = c.addr.port();
    assert!(links.iter().all(|l| {
        !l["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("https://deck.tail1234.ts.net:{port}/"))
    }));
    // Every link carries the same token.
    let token = token_of(links[0]["url"].as_str().unwrap()).to_owned();
    assert!(!token.is_empty());
    for l in links {
        assert_eq!(token_of(l["url"].as_str().unwrap()), token);
    }
    assert_eq!(link_count(&c).await, 1);

    // The token logs in once.
    let first = c
        .post("/api/auth/link", &[], json!({ "token": token }))
        .await;
    assert_eq!(first.status, 200, "{}", first.body);
    let second = c
        .post("/api/auth/link", &[], json!({ "token": token }))
        .await;
    assert_eq!(second.status, 401, "{}", second.body);

    // The audit row names no target and never the token or a URL.
    let rows = c.store.audit_log(100).await.unwrap();
    let row = rows
        .iter()
        .find(|e| e.action == "auth.device_link")
        .unwrap();
    assert_eq!(row.result, AuditResult::Ok);
    assert_eq!(row.target, None);
    assert!(!format!("{row:?}").contains(&token));
}

#[tokio::test]
async fn a_token_is_refused_and_no_link_is_made() {
    let (_station, c) = fixture().await;
    let token = c
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = c
        .post(PATH, &[("Authorization", bearer.as_str())], Value::Null)
        .await;
    assert_eq!((r.status, r.code()), (403, "owner_only"), "{}", r.body);
    assert_eq!(link_count(&c).await, 0);
    assert_eq!(
        c.audited("auth.device_link", AuditResult::Refused).await,
        vec![Some("owner_only".to_owned())]
    );
}

#[tokio::test]
async fn a_request_without_the_csrf_header_is_refused() {
    let (_station, c) = fixture().await;
    let (cookie, _csrf) = c.login().await;
    let r = c
        .post(PATH, &[("Cookie", cookie.as_str())], Value::Null)
        .await;
    assert_eq!((r.status, r.code()), (403, "csrf"), "{}", r.body);
    assert_eq!(link_count(&c).await, 0);
    let r = c.post(PATH, &[], Value::Null).await;
    assert_eq!((r.status, r.code()), (401, "login_required"), "{}", r.body);
}

#[tokio::test]
async fn the_eleventh_link_in_a_minute_waits() {
    let (_station, c) = fixture().await;
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    for i in 0..10 {
        let r = c.post(PATH, &h, Value::Null).await;
        assert_eq!(r.status, 200, "link {i}: {}", r.body);
    }
    let links = link_count(&c).await;
    let r = c.post(PATH, &h, Value::Null).await;
    assert_eq!(
        (r.status, r.code()),
        (429, "too_many_requests"),
        "{}",
        r.body
    );
    assert_eq!(
        r.body["message"],
        "Wait a minute before you add another device."
    );
    // The refused request made no link.
    assert_eq!(link_count(&c).await, links);
}
