//! The board side of the read-only file service (M3 DL-088): the
//! `/api/stations/{station}/files/*` routes forward to the station's
//! `files.*` methods over a real link on loopback; the station's refusals
//! keep their codes, and the routes need the `drive` scope.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_assets};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::http::assets::BoardAssets;
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

fn ws(root: &std::path::Path, rest: &str) -> String {
    format!("workspace={}{rest}", query(root))
}

#[tokio::test]
async fn the_file_routes_browse_read_and_search_through_the_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(root.join("app/src")).unwrap();
    std::fs::create_dir_all(root.join("app/.ssh")).unwrap();
    std::fs::write(root.join("app/src/lib.rs"), "pub fn needle() {}\n").unwrap();
    std::fs::write(root.join("app/.ssh/key"), "needle secret\n").unwrap();
    std::fs::write(tmp.path().join("outside.txt"), "needle outside\n").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside.txt"), root.join("app/link")).unwrap();
    let root = root.canonicalize().unwrap();
    let app = root.join("app");
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let (node, board) = deck(&tmp.path().join("d"), BoardAssets::None).await;
    let st = station(&tmp.path().join("s"), root.clone()).await;
    let invitation = node.invite().unwrap();
    st.join(&invitation).await.unwrap();
    let (cookie, _) = board.login().await;
    let h = vec![("Cookie".to_owned(), cookie)];
    roots(&board, &h).await;

    let r = board
        .get(
            &format!("/api/stations/alpha/files/list?{}", ws(&app, "")),
            &refs(&h),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let names: Vec<_> = r.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(names, [("src", "dir"), ("link", "symlink")]);

    let r = board
        .get(
            &format!(
                "/api/stations/alpha/files/read?{}",
                ws(&app, "&path=src/lib.rs")
            ),
            &refs(&h),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["text"], "pub fn needle() {}\n");

    let r = board
        .get(
            &format!(
                "/api/stations/alpha/files/search?{}",
                ws(&app, "&q=needle&mode=content")
            ),
            &refs(&h),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["backend"], "walk");
    let hits = r.body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{}", r.body);
    assert_eq!(hits[0]["path"], "src/lib.rs");

    // The station's refusals keep their codes.
    for (q, status, code) in [
        ("&path=link", 404, "not_found"),
        ("&path=.ssh/key", 404, "not_found"),
        ("&path=../outside.txt", 400, "invalid_path"),
        ("&path=src", 400, "wrong_kind"),
    ] {
        let r = board
            .get(
                &format!("/api/stations/alpha/files/read?{}", ws(&app, q)),
                &refs(&h),
            )
            .await;
        expect(&r, status, code);
    }
    let r = board
        .get(
            &format!("/api/stations/alpha/files/list?{}", ws(tmp.path(), "")),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "outside_roots");
    let r = board
        .get("/api/stations/alpha/files/list?session=nope", &refs(&h))
        .await;
    expect(&r, 404, "unknown_session");
    let r = board
        .get(
            "/api/stations/alpha/sessions/nope/turns/turn-1/diff",
            &refs(&h),
        )
        .await;
    expect(&r, 404, "unknown_session");
    // The deck refuses a query without exactly one root, and a bad mode.
    let r = board.get("/api/stations/alpha/files/list", &refs(&h)).await;
    expect(&r, 400, "invalid_root");
    let r = board
        .get(
            &format!("/api/stations/alpha/files/list?session=x&{}", ws(&app, "")),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "invalid_root");
    let r = board
        .get(
            &format!(
                "/api/stations/alpha/files/search?{}",
                ws(&app, "&q=ab&mode=regex")
            ),
            &refs(&h),
        )
        .await;
    expect(&r, 400, "invalid_query");
    let r = board
        .get(
            &format!("/api/stations/zulu/files/list?{}", ws(&app, "")),
            &refs(&h),
        )
        .await;
    expect(&r, 404, "unknown_station");

    // The file routes need the drive scope.
    let day = 86_400_000;
    let token = board
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), day)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    for path in [
        format!("/api/stations/alpha/files/list?{}", ws(&app, "")),
        format!(
            "/api/stations/alpha/files/read?{}",
            ws(&app, "&path=src/lib.rs")
        ),
        format!("/api/stations/alpha/files/search?{}", ws(&app, "&q=needle")),
        "/api/stations/alpha/sessions/s/turns/t/diff".to_owned(),
    ] {
        let r = board
            .get(&path, &[("Authorization", bearer.as_str())])
            .await;
        assert_eq!(r.status, 403, "{path}: {}", r.body);
    }

    st.shutdown();
    node.shutdown();
}
