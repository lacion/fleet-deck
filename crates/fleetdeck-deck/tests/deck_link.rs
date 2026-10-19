//! The deck link runtime against real station nodes on loopback (M2 P3-6b):
//! `add-station` (the deck dials a listening station), `invite` + `join`
//! (the station dials the deck), the sync loop (replay, live events, acks,
//! snapshot on a new station database), a deck restart that resumes from
//! the stored cursors, and `remove-station`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::store::Direction;
use fleetdeck_deck::{DeckNode, DeckStore, NodeError};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::CardStatus;
use fleetdeck_proto::event::{Event, ProviderKind, RunOrigin};
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

/// Polls `check` until it is true or the test times out.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + T;
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct Station {
    _dir: tempfile::TempDir,
    store: Store,
    node: Arc<StationNode>,
}

async fn station(name: &str) -> Station {
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
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store.clone(),
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
    Station {
        _dir: dir,
        store,
        node,
    }
}

impl Station {
    async fn session(&self, id: &str) {
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
    }

    async fn run(&self, id: &str, n: u32) -> u64 {
        let e: NewEvent = Event::RunStarted {
            author: None,
            input_id: None,
            run_id: format!("run-{n}").into(),
            input_summary: "hi".to_owned(),
            origin: RunOrigin::Board,
            input_text: None,
        }
        .into();
        self.store.append(id.into(), vec![e]).await.unwrap()[0].station_seq
    }
}

struct Deck {
    identity: Arc<Identity>,
    store: DeckStore,
    node: Arc<DeckNode>,
}

async fn deck(dir: &Path, identity: &Arc<Identity>, listen: &[SocketAddr]) -> Deck {
    let (clock, ids) = clock_ids();
    let store = DeckStore::open(&dir.join("deck.db"), clock, ids)
        .await
        .unwrap();
    // A restarted deck binds the same port; the old socket closes
    // asynchronously after the abort.
    let mut tries = 0;
    let node = loop {
        match DeckNode::start(store.clone(), Arc::clone(identity), listen, fast()).await {
            Ok(node) => break node,
            Err(NodeError::Listen { .. }) if tries < 50 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("deck node: {e}"),
        }
    };
    Deck {
        identity: Arc::clone(identity),
        store,
        node,
    }
}

impl Deck {
    /// Waits until the read model holds `seq` of the station `name`.
    async fn synced(&self, name: &str, seq: u64) {
        let store = self.store.clone();
        let name = name.to_owned();
        eventually(&format!("{name} at {seq}"), || {
            let store = store.clone();
            let name = name.clone();
            async move {
                matches!(store.find_station(name).await.unwrap(), Some(st) if st.cursor >= seq)
            }
        })
        .await;
    }

    async fn runs(&self, name: &str, session: &str) -> Vec<String> {
        let st = self
            .store
            .find_station(name.to_owned())
            .await
            .unwrap()
            .unwrap();
        self.store
            .timeline(st.station_id, session.into())
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                fleetdeck_proto::EventBody::Known(Event::RunStarted { run_id, .. }) => {
                    Some(run_id.to_string())
                }
                _ => None,
            })
            .collect()
    }
}

/// The runs `from..=to`. The first sync is a snapshot batch (the deck
/// knows no incarnation yet): the timeline starts after its watermark.
fn runs(from: u32, to: u32) -> Vec<String> {
    (from..=to).map(|i| format!("run-{i}")).collect()
}

#[tokio::test]
async fn two_stations_one_dialed_one_dialing_sync_and_a_deck_restart_loses_and_repeats_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let d = deck(dir.path(), &identity, &["127.0.0.1:0".parse().unwrap()]).await;
    let deck_addr = d.node.listen_addrs()[0];

    // Alpha listens; the deck dials it.
    let alpha = station("alpha").await;
    alpha.session("a1").await;
    let a_first = alpha.run("a1", 1).await;
    let inv = alpha
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let st = d.node.add_station(&inv, None).await.unwrap();
    assert_eq!(st.name, "alpha", "the name of the invitation's station");
    assert_eq!(st.direction, Direction::DeckDials);
    assert_eq!(st.scopes, STATION_SCOPES.into_iter().collect());

    // Bravo has no listener; it dials the deck (NAT).
    let bravo = station("bravo").await;
    bravo.session("b1").await;
    let b_first = bravo.run("b1", 1).await;
    bravo
        .node
        .join(
            &d.node
                .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite)
                .unwrap(),
        )
        .await
        .unwrap();
    // The first sync of each station is a snapshot batch.
    d.synced("alpha", a_first).await;
    let bravo_fp = bravo.node.link().node.identity.fingerprint().short();
    d.synced("bravo", b_first).await;
    assert_ne!(bravo_fp, "bravo");

    let a_head = alpha.run("a1", 2).await;
    let b_head = bravo.run("b1", 2).await;
    d.synced("alpha", a_head).await;
    // The dialing station is pinned under its short fingerprint and renamed
    // to its station.toml name at the first link.
    d.synced("bravo", b_head).await;
    let bravo_rec = d
        .store
        .find_station("bravo".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bravo_rec.direction, Direction::StationDials);
    assert_eq!(d.runs("alpha", "a1").await, runs(2, 2));
    assert_eq!(d.runs("bravo", "b1").await, runs(2, 2));
    let cards = d.store.sessions().await.unwrap();
    assert_eq!(cards.len(), 2);
    assert!(
        cards.iter().all(|c| c.status == CardStatus::Working),
        "{cards:?}"
    );

    // The deck stops; both stations keep working; the deck restarts.
    d.node.shutdown();
    drop(d);
    alpha.run("a1", 3).await;
    bravo.run("b1", 3).await;
    let d = deck(dir.path(), &identity, &[deck_addr]).await;
    assert_eq!(d.node.listen_addrs(), [deck_addr]);
    let a_head = alpha.run("a1", 4).await;
    let b_head = bravo.run("b1", 4).await;
    d.synced("alpha", a_head).await;
    d.synced("bravo", b_head).await;
    assert_eq!(
        d.runs("alpha", "a1").await,
        runs(2, 4),
        "no loss, no repeat"
    );
    assert_eq!(
        d.runs("bravo", "b1").await,
        runs(2, 4),
        "no loss, no repeat"
    );
    assert_eq!(d.node.link().open_links(), 2);
    assert_eq!(d.identity.fingerprint(), identity.fingerprint());

    // `remove-station` closes the link and drops the station's cards.
    d.node.remove_station("bravo").await.unwrap();
    eventually("bravo's link closes", || async {
        d.node.link().open_links() == 1
    })
    .await;
    assert!(
        d.store
            .find_station("bravo".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    let cards = d.store.sessions().await.unwrap();
    assert_eq!(cards.len(), 1, "the removed station's cards are gone");
}

#[tokio::test]
async fn a_deck_without_a_link_listener_cannot_invite_the_first_sync_is_a_snapshot_and_offline_cards_say_unreachable()
 {
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let d = deck(dir.path(), &identity, &[]).await;
    assert!(matches!(
        d.node
            .invite(fleetdeck_deck::knobs::DeckKnobs::default().invite),
        Err(NodeError::NoListener)
    ));

    let first = station("alpha").await;
    first.session("old").await;
    let head = first.run("old", 1).await;
    let inv = first
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    d.node.add_station(&inv, None).await.unwrap();
    d.synced("alpha", head).await;
    // The deck knew no incarnation: the station answered with a snapshot
    // batch, which stored the incarnation and the watermark.
    let before = d
        .store
        .find_station("alpha".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.cursor, head);
    assert_eq!(before.incarnation.as_ref(), Some(first.store.incarnation()));

    let again = first.node.invite("127.0.0.1:0".parse().unwrap()).await;
    assert!(again.is_err(), "the station has a deck already");

    // The station forgets the deck and closes the link: the cards say the
    // station is unreachable.
    assert_eq!(first.node.forget_deck().await.unwrap(), 1);
    eventually("alpha is unreachable", || async {
        d.store
            .sessions()
            .await
            .unwrap()
            .iter()
            .all(|c| c.status == CardStatus::Unreachable)
    })
    .await;
}

/// ADR-006 §5 and §6.1 end to end (DL-211): the station node serves the
/// loopback coordinator endpoint; an authenticated tool call goes over the
/// paired link to the deck, which decides it against the owner's grant.
#[tokio::test]
async fn a_bound_coordinator_reaches_the_deck_through_the_station_endpoint() {
    use fleetdeck_deck::store::NewGrant;
    use fleetdeck_proto::mutation::CoordinatorBinding;
    use fleetdeck_station::coordinator::{Binding, BindingState, ToolClass, mint};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn post(url: &str, token: Option<&str>, body: &str) -> (u16, String) {
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
            .parse()
            .unwrap();
        let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_owned())
    }

    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(Identity::generate().unwrap());
    let d = deck(dir.path(), &identity, &[]).await;
    let st = station("alpha").await;
    st.session("coord-1").await;
    st.session("worker-1").await;
    let head = st.run("worker-1", 1).await;
    let inv = st
        .node
        .invite("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    d.node.add_station(&inv, None).await.unwrap();
    d.synced("alpha", head).await;
    let station_id = d
        .store
        .find_station("alpha".to_owned())
        .await
        .unwrap()
        .unwrap()
        .station_id;

    let url = st.node.coordinator_url();
    // No credential: the uniform 401.
    let (status, _) = post(
        &url,
        None,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    )
    .await;
    assert_eq!(status, 401);

    // The owner's grant, bound to the caller's native session.
    let grant = d
        .node
        .create_grant(NewGrant::new(station_id.clone(), "coord-1".into()))
        .await
        .unwrap();
    assert!(
        d.store
            .bind_grant(CoordinatorBinding {
                binding_id: "b-1".to_owned(),
                grant_id: grant.grant_id.clone(),
                generation: 1,
                provider_instance: "claude".into(),
                native_id: "native-1".to_owned(),
            })
            .await
            .unwrap()
    );
    let (token, digest) = mint().unwrap();
    st.node.coordinator_bindings().insert(
        digest,
        Binding {
            binding_id: "b-1".to_owned(),
            grant_id: grant.grant_id.clone(),
            session_id: "coord-1".into(),
            provider_instance: "claude".to_owned(),
            native_id: Some("native-1".to_owned()),
            generation: 1,
            classes: [ToolClass::Read, ToolClass::Mail].into_iter().collect(),
            state: BindingState::Active,
            control_skill: "skill".to_owned(),
            scope: fleetdeck_proto::command::GrantScope::Lineage,
        },
    );
    let call = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "send_mail", "arguments": {
            "to": {"station": station_id.as_str(), "session_id": "worker-1"},
            "text": "hello from the coordinator"}}
    });
    let (status, body) = post(&url, Some(token.expose()), &call.to_string()).await;
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["result"]["isError"], false, "{body}");
    // The deck decided the call once and stored the mail for the worker.
    let mails = d.store.mails(10).await.unwrap();
    assert_eq!(mails.len(), 1, "{body}");
    assert_eq!(mails[0].session_id.as_str(), "worker-1");
    assert_eq!(d.store.coordinator_receipts().await.unwrap(), 1);
}
