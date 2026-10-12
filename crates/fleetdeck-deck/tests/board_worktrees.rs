//! The worktree inventory over a real link on loopback (DL-142): the route
//! is the owner's (a token with the `admin` scope gets 403 `owner_only`) and
//! relays the station's list.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board_with_node};
use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::{DeckNode, DeckStore};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::Event;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};

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

/// A station whose store holds one session with the worktree `worktree`.
async fn station(dir: &Path, root: PathBuf, worktree: &Path) -> Arc<StationNode> {
    let (clock, ids) = clock_ids();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    store
        .create_session(NewSession {
            session_id: "wt-session".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: root.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: Some("Fix the parser".to_owned()),
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    // The summary learns its worktree from `session.named`.
    let named: NewEvent = Event::SessionNamed {
        callsign: "gone".to_owned(),
        worktree: Some(worktree.display().to_string()),
    }
    .into();
    store
        .append("wt-session".into(), vec![named])
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

/// Asks until the deck has a live link to `alpha`.
async fn worktrees(c: &BoardClient, headers: &[(&str, &str)]) -> Http {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c.get("/api/stations/alpha/worktrees", headers).await;
        let waiting = [(404, "unknown_station"), (409, "station_unreachable")];
        if !waiting.contains(&(r.status, r.code())) {
            return r;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_owner_lists_the_worktrees_of_a_station_and_a_token_cannot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::create_dir_all(tmp.path().join("s")).unwrap();
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    let worktree = root.join("repo--fd-gone");
    let (node, board) = deck(&tmp.path().join("d")).await;
    let st = station(&tmp.path().join("s"), root.clone(), &worktree).await;
    st.join(&node.invite().unwrap()).await.unwrap();
    let (cookie, _csrf) = board.login().await;

    let r = worktrees(&board, &[("Cookie", cookie.as_str())]).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let list = r.body["worktrees"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{}", r.body);
    assert_eq!(list[0]["session_id"], "wt-session");
    assert_eq!(list[0]["title"], "Fix the parser");
    assert_eq!(list[0]["path"], worktree.to_str().unwrap());
    assert_eq!(list[0]["exists"], false);
    assert_eq!(list[0]["verdict"], "missing");

    // An admin token is not the owner's board login (DL-138).
    let token = board
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.1.expose());
    let r = board
        .get(
            "/api/stations/alpha/worktrees",
            &[("Authorization", bearer.as_str())],
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "owner_only"), "{}", r.body);
}
