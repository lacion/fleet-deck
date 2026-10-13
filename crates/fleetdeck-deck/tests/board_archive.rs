//! The archived sessions of the board (DL-149): `GET /api/archive` needs
//! `read`; `POST /api/archive` needs `drive` and the CSRF token of a cookie
//! login, is audited, and a resend with the same `mutation_id` changes
//! nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use serde_json::json;

const DAY: i64 = 24 * 60 * 60 * 1000;

async fn bearer(c: &BoardClient, name: &str, scopes: &[Scope]) -> (String, String) {
    let (_, t) = c
        .store
        .create_token(
            name.into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    ("Authorization".to_owned(), format!("Bearer {}", t.expose()))
}

fn body(id: &str, items: &[(&str, &str, bool)]) -> serde_json::Value {
    json!({
        "mutation_id": id,
        "changes": items
            .iter()
            .map(|(st, se, a)| json!({"station_id": st, "session_id": se, "archived": a}))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn the_owner_archives_and_a_resend_changes_nothing() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    let r = c.get("/api/archive", &h[..1]).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({"archived": []}));

    let r = c
        .post(
            "/api/archive",
            &h,
            body("mutation-1", &[("st", "s2", true), ("st", "s1", true)]),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({"archived": [
            {"station_id": "st", "session_id": "s1"},
            {"station_id": "st", "session_id": "s2"},
        ]})
    );
    let r = c
        .post(
            "/api/archive",
            &h,
            body("mutation-2", &[("st", "s1", false)]),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["archived"].as_array().unwrap().len(), 1);

    // A stale resend of mutation-1 changes nothing.
    let r = c
        .post(
            "/api/archive",
            &h,
            body("mutation-1", &[("st", "s2", true), ("st", "s1", true)]),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({"archived": [{"station_id": "st", "session_id": "s2"}]})
    );
    let r = c.get("/api/archive", &h[..1]).await;
    assert_eq!(r.body["archived"].as_array().unwrap().len(), 1);

    let mut details = c.audited("archive.change", AuditResult::Ok).await;
    details.sort();
    assert_eq!(
        details,
        [
            Some("1 changes".to_owned()),
            Some("2 changes".to_owned()),
            Some("2 changes, replayed".to_owned()),
        ]
    );
}

#[tokio::test]
async fn archive_changes_need_drive_csrf_and_a_valid_body() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let read = bearer(&c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    assert_eq!(c.get("/api/archive", &read).await.status, 200);
    let r = c
        .post(
            "/api/archive",
            &read,
            body("mutation-1", &[("st", "s1", true)]),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    let drive = bearer(&c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    let r = c
        .post(
            "/api/archive",
            &drive,
            body("mutation-1", &[("st", "s1", true)]),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);

    for bad in [
        json!({"mutation_id": "short", "changes": [{"station_id": "st", "session_id": "s", "archived": true}]}),
        json!({"mutation_id": "mutation-9", "changes": []}),
        body("mutation-9", &[("", "s1", true)]),
        json!({"mutation_id": "mutation-9", "changes": [], "x": 1}),
        json!({"nope": true}),
    ] {
        let r = c.post("/api/archive", &drive, bad).await;
        assert_eq!((r.status, r.code()), (400, "invalid_body"), "{}", r.body);
    }
    assert_eq!(
        c.audited("archive.change", AuditResult::Failed).await,
        vec![Some("invalid_body".to_owned()); 5]
    );

    // A cookie login without the CSRF token is refused.
    let (cookie, _) = c.login().await;
    let r = c
        .post(
            "/api/archive",
            &[("Cookie", cookie.as_str())],
            body("mutation-3", &[("st", "s1", true)]),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "csrf"), "{}", r.body);
    assert_eq!(c.get("/api/archive", &[]).await.status, 401);
}
