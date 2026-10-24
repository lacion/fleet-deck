//! The pairing methods of the board API against real station nodes on
//! loopback (M2 P4-2): `POST /api/stations/invite` for a station that dials
//! the deck, `POST /api/stations` to dial a listening station, `POST
//! /api/stations/{station}/remove`; the `admin` scope, CSRF, the refusals,
//! the audit rows without the invitation secret, and the `station.info`
//! report (version, capacity, providers) in `GET /api/stations`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{Secret, SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

struct Station {
    _dir: tempfile::TempDir,
    node: Arc<StationNode>,
}

async fn station(name: &str, max_sessions: u32) -> Station {
    let dir = tempfile::tempdir().unwrap();
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.path().join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(
        Vec::new(),
        max_sessions,
        OfflineApprovalPolicy::Wait,
        // A Codex instance has no adapter before Phase 5: it is reported
        // without a version.
        BTreeMap::from([("codex".into(), fleetdeck_proto::event::ProviderKind::Codex)]),
    );
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let node = StationNode::start(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        name.to_owned(),
        &[],
        fast(),
    )
    .await
    .unwrap();
    Station { _dir: dir, node }
}

struct Deck {
    _dir: tempfile::TempDir,
    node: Arc<DeckNode>,
    board: BoardClient,
}

async fn deck(listen: bool) -> Deck {
    let dir = tempfile::tempdir().unwrap();
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.path().join("deck.db"), clock, ids)
        .await
        .unwrap();
    let addrs = if listen {
        vec!["127.0.0.1:0".parse().unwrap()]
    } else {
        Vec::new()
    };
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &addrs, fast())
        .await
        .unwrap();
    let board = board_with_node(Arc::clone(&node)).await;
    Deck {
        _dir: dir,
        node,
        board,
    }
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

fn refs(h: &[(String, String)]) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

async fn post(c: &BoardClient, h: &[(String, String)], path: &str, body: Value) -> Http {
    c.post(path, &refs(h), body).await
}

/// Waits until `GET /api/stations` holds a reachable station `name` with a
/// `station.info` report; returns its view.
async fn reported(c: &BoardClient, h: &[(String, String)], name: &str) -> Value {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.get("/api/stations", &refs(h)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let found = r
            .body
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name && s["reachable"] == true && !s["info"].is_null())
            .cloned();
        if let Some(view) = found {
            return view;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no report of {name}: {}",
            r.body
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

#[tokio::test]
async fn removing_a_station_answers_500_and_audits_a_failure_when_the_store_write_fails() {
    let d = deck(true).await;
    let h = owner(&d.board).await;
    let alpha = station("alpha", 4).await;
    let text = alpha
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": text.expose(), "name": null }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    // A full disk: the delete of the station row fails.
    let conn = rusqlite::Connection::open(d._dir.path().join("deck.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_station_delete BEFORE DELETE ON stations
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();

    // NV-15: the revocation was not stored, so the route is not a success.
    let r = post(&d.board, &h, "/api/stations/alpha/remove", json!({})).await;
    expect(&r, 500, "revoke_failed");
    assert!(
        r.body["message"]
            .as_str()
            .unwrap()
            .contains("the revocation was not stored"),
        "{}",
        r.body
    );
    let rows = d.board.store.audit_log(100).await.unwrap();
    let failed: Vec<_> = rows
        .iter()
        .filter(|e| e.action == "stations.remove" && e.result == AuditResult::Failed)
        .map(|e| e.detail.clone().unwrap_or_default())
        .collect();
    assert_eq!(failed, vec!["revoke_failed".to_owned()]);
    assert!(
        !rows
            .iter()
            .any(|e| e.action == "stations.remove" && e.result == AuditResult::Ok),
        "no success row"
    );
}

#[tokio::test]
async fn the_board_pairs_both_directions_reports_station_info_and_removes() {
    let d = deck(true).await;
    let h = owner(&d.board).await;

    // A station that dials the deck: the board makes the invitation.
    let r = post(&d.board, &h, "/api/stations/invite", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let invitation = r.body["invitation"].as_str().unwrap().to_owned();
    assert!(!invitation.is_empty());
    assert_eq!(r.body["join_command"], "fleetdeck station join");
    let now = d.board.store.now_ms();
    let expires = r.body["expires_at"].as_i64().unwrap();
    assert!(
        expires > now && expires <= now + 15 * 60 * 1000,
        "{expires}"
    );
    let bravo = station("bravo", 2).await;
    bravo
        .node
        .join(&Secret::new(invitation.clone()))
        .await
        .unwrap();
    let view = reported(&d.board, &h, "bravo").await;
    assert_eq!(view["direction"], "station_dials");
    let info = &view["info"];
    assert_eq!(info["name"], "bravo");
    assert_eq!(info["version"], fleetdeck_core::build::version());
    assert_eq!(info["max_sessions"], 2);
    assert_eq!(
        info["providers"],
        json!([{ "instance": "codex", "provider": "codex", "version": null, "auth": "unknown", "capabilities": null, "version_status": "unknown", "default_mode": "untrusted/workspace-write" }])
    );
    assert!(view["info_at"].as_i64().unwrap() >= now);

    // A listening station: the board dials it from its invitation.
    let alpha = station("alpha", 4).await;
    let text = alpha
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": text.expose(), "name": "alpha-box" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["name"], "alpha-box");
    assert_eq!(r.body["direction"], "deck_dials");
    // At the link up the deck adopts the station's own name.
    let view = reported(&d.board, &h, "alpha").await;
    assert_eq!(view["info"]["name"], "alpha", "the station's own name");
    assert_eq!(view["info"]["max_sessions"], 4);

    // Refusals: a taken name, a bad invitation, an unknown field. A paired
    // station makes no new invitation, so a third station offers one.
    let gamma = station("gamma", 1).await;
    let again = gamma
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": again.expose(), "name": "alpha" }),
    )
    .await;
    expect(&r, 409, "name_taken");
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": "fdinv1_garbage", "name": null }),
    )
    .await;
    expect(&r, 400, "invalid_invitation");
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": again.expose(), "nme": "x" }),
    )
    .await;
    expect(&r, 400, "invalid_body");
    assert!(
        !r.body.to_string().contains(again.expose().as_str()),
        "a refusal never echoes the invitation"
    );
    let r = post(
        &d.board,
        &h,
        "/api/stations",
        json!({ "invitation": "x".repeat(4097), "name": null }),
    )
    .await;
    expect(&r, 400, "invalid_invitation");

    // Remove by name; a second remove finds nothing.
    let r = post(&d.board, &h, "/api/stations/alpha/remove", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["name"], "alpha");
    let r = d.board.get("/api/stations", &refs(&h)).await;
    let names: Vec<_> = r
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("bravo")]);
    let r = post(&d.board, &h, "/api/stations/alpha/remove", json!({})).await;
    expect(&r, 404, "unknown_station");

    // The audit log names each call, never an invitation text.
    assert_eq!(
        d.board
            .audited("stations.invite", AuditResult::Ok)
            .await
            .len(),
        1
    );
    let rows = d.board.store.audit_log(100).await.unwrap();
    let added: Vec<_> = rows
        .iter()
        .filter(|e| e.action == "stations.add" && e.result == AuditResult::Ok)
        .map(|e| e.target.clone())
        .collect();
    assert_eq!(added, vec![Some("alpha-box".to_owned())]);
    let mut failed: Vec<_> = rows
        .iter()
        .filter(|e| e.action.starts_with("stations.") && e.result == AuditResult::Failed)
        .map(|e| (e.action.clone(), e.detail.clone().unwrap_or_default()))
        .collect();
    failed.sort();
    assert_eq!(
        failed,
        vec![
            ("stations.add".to_owned(), "invalid_body".to_owned()),
            ("stations.add".to_owned(), "invalid_invitation".to_owned()),
            ("stations.add".to_owned(), "invalid_invitation".to_owned()),
            ("stations.add".to_owned(), "name_taken".to_owned()),
            ("stations.remove".to_owned(), "unknown_station".to_owned()),
        ]
    );
    for e in &rows {
        let row = format!("{e:?}");
        for secret in [
            invitation.as_str(),
            text.expose().as_str(),
            again.expose().as_str(),
        ] {
            assert!(!row.contains(secret), "audit row holds a secret: {row}");
        }
    }
    d.node.shutdown();
    alpha.node.shutdown();
    bravo.node.shutdown();
    gamma.node.shutdown();
}

#[tokio::test]
async fn pairing_needs_admin_csrf_a_listener_and_a_node() {
    let d = deck(false).await;
    let h = owner(&d.board).await;
    let r = post(&d.board, &h, "/api/stations/invite", json!({})).await;
    expect(&r, 409, "no_listener");

    // Without the CSRF header a cookie call is refused.
    let r = post(&d.board, &h[..1], "/api/stations/invite", json!({})).await;
    expect(&r, 403, "csrf");

    // A token without `admin` cannot pair or remove.
    let day = 24 * 60 * 60 * 1000;
    let (_, drive) = d
        .board
        .store
        .create_token(
            "phone".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve]),
            day,
        )
        .await
        .unwrap();
    let bearer = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", drive.expose()),
    )];
    for path in [
        "/api/stations/invite",
        "/api/stations",
        "/api/stations/x/remove",
    ] {
        let r = post(&d.board, &bearer, path, json!({ "invitation": "x" })).await;
        expect(&r, 403, "forbidden");
    }
    let r = d.board.get("/api/stations", &refs(&bearer)).await;
    assert_eq!(r.status, 200, "read stays open to `read`");

    // An admin token is not the owner's board login (PLAN §9, DL-138).
    let (_, admin) = d
        .board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
            day,
        )
        .await
        .unwrap();
    let bearer = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", admin.expose()),
    )];
    let before = d.board.get("/api/stations", &refs(&bearer)).await.body;
    for path in [
        "/api/stations/invite",
        "/api/stations",
        "/api/stations/x/remove",
    ] {
        let r = post(&d.board, &bearer, path, json!({ "invitation": "x" })).await;
        expect(&r, 403, "owner_only");
    }
    assert_eq!(
        d.board.get("/api/stations", &refs(&bearer)).await.body,
        before
    );
    for method in ["stations.invite", "stations.add", "stations.remove"] {
        let failed = d.board.audited(method, AuditResult::Failed).await;
        assert!(
            failed.contains(&Some("owner_only".to_owned())),
            "{method}: {failed:?}"
        );
    }

    // A board without a link node answers 503.
    let plain = board(Arc::clone(d.node.link())).await;
    let h = owner(&plain).await;
    let r = post(&plain, &h, "/api/stations/invite", json!({})).await;
    expect(&r, 503, "pairing_unavailable");
    d.node.shutdown();
}
