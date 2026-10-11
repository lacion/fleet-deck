//! A ticket must not outlive the credential that created it, even within 30 s.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::board;
use common::fixture;
use fleetdeck_proto::link::Scope;
use serde_json::json;

#[tokio::test]
async fn a_revoked_by_password_login_and_an_expired_token_cannot_redeem_tickets() {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    let r = c
        .post(
            "/api/ws/ticket",
            &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
            json!({}),
        )
        .await;
    assert_eq!(r.status, 200);
    c.store
        .set_password(fleetdeck_core::Secret::new(
            "a new long password".to_owned(),
        ))
        .await
        .unwrap();
    let path = format!("/api/ws?ticket={}", r.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));

    let (_, token) = c
        .store
        .create_token("short".into(), BTreeSet::from([Scope::Read]), 1_000)
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .post("/api/ws/ticket", &[("Authorization", &auth)], json!({}))
        .await;
    assert_eq!(r.status, 200);
    f.det.advance(Duration::from_secs(2));
    assert!(
        r.body["expires_at"].as_i64().unwrap() > c.store.now_ms(),
        "the ticket itself is still valid"
    );
    let path = format!("/api/ws?ticket={}", r.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));
}
