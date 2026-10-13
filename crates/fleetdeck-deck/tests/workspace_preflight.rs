//! The Git access check over a real link on loopback (DL-144): `POST
//! /api/stations/{station}/workspace/preflight` forwards the URL to the
//! station's `workspace.preflight` method; the scope, the CSRF check, the
//! refusals of the station and of the deck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
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

async fn station(dir: &Path, root: PathBuf) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(vec![root], 2, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(store, policy, Adapters::new(), ids)
        .await
        .unwrap();
    StationNode::start(
        Arc::new(manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
    )
    .await
    .unwrap()
}

async fn deck(dir: &Path) -> (Arc<DeckNode>, BoardClient) {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &["127.0.0.1:0".parse().unwrap()], fast())
        .await
        .unwrap();
    let board = board_with_node(Arc::clone(&node)).await;
    (node, board)
}

fn refs(h: &[(String, String)]) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
}

const PATH: &str = "/api/stations/alpha/workspace/preflight";

async fn check(c: &BoardClient, h: &[(String, String)], body: Value) -> Http {
    c.post(PATH, &refs(h), body).await
}

/// Waits until the board can reach `alpha` (the deck learns the name from
/// `station.info` on link up).
async fn reachable(c: &BoardClient, h: &[(String, String)], url: &str) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = check(c, h, json!({ "url": url })).await;
        if r.status != 404 && r.status != 409 {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_preflight_is_relayed_to_the_station_and_its_refusals_are_mapped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(root.join("plain")).unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let root = root.canonicalize().unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-qm", "one"]);

    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone()).await;
    let invitation = node.invite().unwrap();
    st.join(&invitation).await.unwrap();

    let (cookie, csrf) = board.login().await;
    let h = vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ];

    // A repository that the station can read.
    let r = reachable(&board, &h, &format!("file://{}", repo.display())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["ok"], true, "{}", r.body);
    assert_eq!(r.body["message"], "");
    assert!(r.body["hint"].is_null());

    // A directory that is no repository is an answer, with a hint.
    let plain = root.join("plain");
    let r = check(
        &board,
        &h,
        json!({ "url": format!("file://{}", plain.display()) }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["ok"], false, "{}", r.body);
    assert!(r.body["hint"].as_str().unwrap().contains("does not exist"));

    // The URL rules of a clone: credentials are forbidden, odd text is invalid.
    let r = check(
        &board,
        &h,
        json!({ "url": "https://user:token@example.com/o/r.git" }),
    )
    .await;
    expect(&r, 403, "forbidden");
    let r = check(
        &board,
        &h,
        json!({ "url": format!("file://{}", tmp.path().display()) }),
    )
    .await;
    expect(&r, 403, "forbidden");
    let r = check(&board, &h, json!({ "url": "https://example.com/a b" })).await;
    expect(&r, 400, "invalid_url");

    // The deck refuses an unknown station and a body with another field.
    let r = board
        .post(
            "/api/stations/zulu/workspace/preflight",
            &refs(&h),
            json!({ "url": "https://example.com/o/r.git" }),
        )
        .await;
    expect(&r, 404, "unknown_station");
    let r = check(
        &board,
        &h,
        json!({ "url": "https://example.com/o/r.git", "x": 1 }),
    )
    .await;
    assert!((400..500).contains(&r.status), "{}", r.body);

    // The cookie login needs its CSRF token: the route mutates nothing, but
    // it is a POST of the owner and audited like one.
    let no_csrf = vec![h[0].clone()];
    let r = check(
        &board,
        &no_csrf,
        json!({ "url": "https://example.com/o/r.git" }),
    )
    .await;
    expect(&r, 403, "csrf");

    // The drive scope is needed.
    let day = 86_400_000;
    let token = board
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), day)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = board
        .post(
            PATH,
            &[("Authorization", bearer.as_str())],
            json!({ "url": "https://example.com/o/r.git" }),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    st.shutdown();
    node.shutdown();
}
