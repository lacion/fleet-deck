//! The spawn form's back end (M2 P4-4): `GET
//! /api/stations/{station}/workspaces` forwards the directory picker to the
//! station's `workspace.list` over a real link on loopback; the scope, the
//! refusals of the station and of the deck; and the board files that the
//! deck serves with a strict content security policy.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_assets};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::http::assets::{BoardAssets, CSP, NOT_BUILT};
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};

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

async fn station(dir: &std::path::Path, root: PathBuf) -> Arc<StationNode> {
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
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::new(),
        ids,
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
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

async fn deck(dir: &std::path::Path, assets: BoardAssets) -> (Arc<DeckNode>, BoardClient) {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let node = DeckNode::start(store, identity, &["127.0.0.1:0".parse().unwrap()], fast())
        .await
        .unwrap();
    let board = board_with_assets(Arc::clone(&node), assets).await;
    (node, board)
}

fn refs(h: &[(String, String)]) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

fn query(path: &std::path::Path) -> String {
    let mut q = String::new();
    for b in path.to_str().unwrap().bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            q.push(char::from(b));
        } else {
            q.push_str(&format!("%{b:02X}"));
        }
    }
    q
}

/// Waits until the board can reach `alpha` and lists its roots.
async fn roots(c: &BoardClient, h: &[(String, String)]) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.get("/api/stations/alpha/workspaces", &refs(h)).await;
        if r.status == 200 {
            return r;
        }
        // The deck learns the name `alpha` from `station.info` on link up.
        assert!(
            [(409, "station_unreachable"), (404, "unknown_station")]
                .contains(&(r.status, r.code())),
            "{}",
            r.body
        );
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_directory_picker_lists_the_station_roots_through_the_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(root.join("app/.git")).unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    std::fs::create_dir_all(tmp.path().join("outside")).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d"), BoardAssets::None).await;
    let st = station(&tmp.path().join("s"), root.clone()).await;
    let invitation = node.invite().unwrap();
    st.join(&invitation).await.unwrap();

    let (cookie, _) = board.login().await;
    let h = vec![("Cookie".to_owned(), cookie)];
    let r = roots(&board, &h).await;
    assert!(r.body["path"].is_null(), "{}", r.body);
    assert_eq!(r.body["entries"][0]["path"], root.to_str().unwrap());

    let path = format!("/api/stations/alpha/workspaces?path={}", query(&root));
    let r = board.get(&path, &refs(&h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let names: Vec<_> = r.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["repository"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(names, [("app", true), ("notes", false)]);
    assert!(r.body["parent"].is_null(), "a root has no parent");
    assert_eq!(r.body["truncated"], false);

    let app = root.join("app");
    let r = board
        .get(
            &format!("/api/stations/alpha/workspaces?path={}", query(&app)),
            &refs(&h),
        )
        .await;
    assert_eq!(r.body["parent"], root.to_str().unwrap(), "{}", r.body);

    // The station refuses paths outside its roots and missing paths.
    let out = tmp.path().join("outside");
    let r = board
        .get(
            &format!("/api/stations/alpha/workspaces?path={}", query(&out)),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "outside_roots");
    let r = board
        .get(
            &format!(
                "/api/stations/alpha/workspaces?path={}",
                query(&root.join("nope"))
            ),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "missing_workspace");
    let r = board
        .get(
            &format!(
                "/api/stations/alpha/workspaces?path={}",
                query(&root.join("../outside"))
            ),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "outside_roots");

    // The deck refuses an unknown station and a path that is too long.
    let r = board.get("/api/stations/zulu/workspaces", &refs(&h)).await;
    expect(&r, 404, "unknown_station");
    let long = "a".repeat(4097);
    let r = board
        .get(
            &format!("/api/stations/alpha/workspaces?path={long}"),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "invalid_path");

    // The picker needs the drive scope.
    let day = 86_400_000;
    let token = board
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), day)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = board
        .get(
            "/api/stations/alpha/workspaces",
            &[("Authorization", bearer.as_str())],
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    st.shutdown();
    node.shutdown();
}

#[tokio::test]
async fn the_deck_serves_the_board_files_with_a_strict_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let dist = tmp.path().join("dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::write(dist.join("index.html"), "<div id=\"root\"></div>").unwrap();
    std::fs::write(dist.join("assets/index-abc.js"), "console.log(1)").unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d"), BoardAssets::Dir(dist)).await;

    for path in ["/", "/fleet", "/sessions/s-1"] {
        let r = board.get(path, &[]).await;
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.text, "<div id=\"root\"></div>", "{path}");
        assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
        assert_eq!(r.header("content-security-policy"), Some(CSP));
        assert_eq!(r.header("cache-control"), Some("no-cache"));
        assert_eq!(r.header("x-frame-options"), Some("DENY"));
    }
    let r = board.get("/assets/index-abc.js", &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.text, "console.log(1)");
    assert_eq!(
        r.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    // A missing file is a 404, never the board page.
    assert_eq!(board.get("/assets/missing.js", &[]).await.status, 404);
    assert_eq!(board.get("/..%2F..%2Fetc%2Fpasswd", &[]).await.status, 404);
    // An unknown API path stays a JSON error and needs no board file.
    let r = board.get("/api/nothing", &[]).await;
    expect(&r, 404, "not_found");
    // The board files never accept a write.
    let r = board.post("/fleet", &[], serde_json::json!({})).await;
    assert_eq!(r.status, 405);
    node.shutdown();

    // A deck without board files says how to build them.
    let tmp2 = tempfile::tempdir().unwrap();
    let (node, board) = deck(tmp2.path(), BoardAssets::None).await;
    let r = board.get("/", &[]).await;
    assert_eq!(r.status, 404);
    assert_eq!(r.text, NOT_BUILT);
    node.shutdown();
}
