//! The station's link node (M2 P3-5c): `join` (the station dials the deck),
//! `invite` (the deck dials the station), restarts from the stored pins,
//! dial addresses and listen addresses, and `forget_deck`. The deck is a
//! bare `fleetdeck-link` node on loopback.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::{Secret, SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::control::Control;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::pairing::{Invitation, Invitations};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_link::revoke::REVOKED;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope, StationInfo};
use fleetdeck_station::link::runtime::DECK_SCOPES;
use fleetdeck_station::link::{NodeError, StationNode};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{OfflineApprovalPolicy, Store, StoreConfig};
use serde_json::json;

const T: Duration = Duration::from_secs(10);

fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

struct Deck {
    identity: Arc<Identity>,
    node: NodeConfig,
    invitations: Arc<Invitations>,
    listener: LinkListener,
}

async fn deck() -> Deck {
    let identity = Arc::new(Identity::generate().unwrap());
    let invitations = Invitations::new();
    let node = NodeConfig::new(
        Arc::clone(&identity),
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Grants::new())
    .with_invitations(Arc::clone(&invitations));
    let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    Deck {
        identity,
        node,
        invitations,
        listener,
    }
}

impl Deck {
    fn invitation(&self) -> Secret<String> {
        self.invitations
            .create(
                self.listener.local_addr,
                self.identity.fingerprint(),
                Duration::from_secs(60),
                [Scope::Read],
            )
            .unwrap()
            .reveal()
    }

    /// The next link that the station dials.
    async fn accept(&mut self) -> LiveLink {
        let open = tokio::time::timeout(T, self.listener.next_link())
            .await
            .expect("the station dials")
            .unwrap();
        fleetdeck_link::start(&self.node, open)
    }
}

struct Station {
    _dir: tempfile::TempDir,
    store: Store,
    manager: Arc<SessionManager>,
    identity: Arc<Identity>,
}

async fn station() -> Station {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = Store::open(
        &dir.path().join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(store.clone(), policy, Adapters::new(), ids)
        .await
        .unwrap();
    Station {
        _dir: dir,
        store,
        manager: Arc::new(manager),
        identity: Arc::new(Identity::generate().unwrap()),
    }
}

impl Station {
    async fn node(&self, listen: &[SocketAddr]) -> Arc<StationNode> {
        // A restarted node binds the stored listen address again; the old
        // listener's socket closes asynchronously after its abort.
        let mut tries = 0;
        loop {
            let started = StationNode::start(
                Arc::clone(&self.manager),
                Arc::clone(&self.identity),
                "box".to_owned(),
                listen,
                fast(),
            )
            .await;
            match started {
                Ok(node) => return node,
                Err(NodeError::Listen { .. }) if tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("station node: {e}"),
            }
        }
    }
}

async fn info(control: &Control) -> StationInfo {
    match tokio::time::timeout(T, control.request("station.info", None, json!({})))
        .await
        .expect("answer")
        .unwrap()
    {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("station.info: {e}"),
    }
}

#[tokio::test]
async fn join_pins_the_deck_with_its_address_dials_it_and_redials_after_a_restart() {
    let mut deck = deck().await;
    let station = station().await;
    let node = station.node(&[]).await;
    assert!(
        node.listen_addrs().await.is_empty(),
        "a joining station only dials"
    );

    let fp = node.join(&deck.invitation()).await.unwrap();
    assert_eq!(fp, deck.identity.fingerprint());
    let keys = station.store.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, deck.identity.public_key());
    assert_eq!(keys[0].role, Role::Deck);
    assert_eq!(keys[0].scopes, DECK_SCOPES.into_iter().collect());
    assert_eq!(
        keys[0].dial_url.as_deref(),
        Some(deck.listener.local_addr.to_string().as_str())
    );

    let link = deck.accept().await;
    assert_eq!(link.peer, station.identity.public_key());
    assert_eq!(info(&link.control).await.name, "box");

    // One deck per station (DL-048).
    let again = node.join(&deck.invitation()).await;
    assert!(
        matches!(again, Err(NodeError::AlreadyPaired(_))),
        "{again:?}"
    );
    let invite = node.invite("127.0.0.1:0".parse().unwrap()).await;
    assert!(
        matches!(invite, Err(NodeError::AlreadyPaired(_))),
        "{invite:?}"
    );

    // A restarted node loads the pin and the address and dials again.
    node.shutdown();
    link.control.close("test", "station restart");
    let node = station.node(&[]).await;
    let link = deck.accept().await;
    assert_eq!(info(&link.control).await.name, "box");

    // `forget-deck` closes the link within 1 s and the dialer stops.
    let t0 = tokio::time::Instant::now();
    assert_eq!(node.forget_deck().await.unwrap(), 1);
    let closing = tokio::time::timeout(Duration::from_secs(1), link.control.closed())
        .await
        .expect("closed within 1 s");
    assert_eq!(closing.reason, REVOKED);
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(station.store.paired_keys().await.unwrap().is_empty());
    let redial = tokio::time::timeout(Duration::from_millis(500), deck.listener.next_link()).await;
    assert!(redial.is_err(), "a forgotten deck is not dialed again");
    assert_eq!(node.forget_deck().await.unwrap(), 0);
}

#[tokio::test]
async fn invite_listens_now_and_after_a_restart_and_the_deck_pairs_and_dials() {
    let deck = deck().await;
    let station = station().await;
    let node = station.node(&[]).await;

    let text = node.invite("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let invitation = Invitation::parse(&text).unwrap();
    assert_eq!(invitation.fingerprint, station.identity.fingerprint());
    assert_eq!(node.listen_addrs().await, vec![invitation.addr]);
    assert_eq!(
        station.store.listen_addrs().await.unwrap(),
        vec![invitation.addr]
    );

    let paired = fleetdeck_link::pair(&deck.node, &invitation, [Scope::Read])
        .await
        .unwrap();
    assert_eq!(paired.key, station.identity.public_key());
    let keys = station.store.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, deck.identity.public_key());
    assert_eq!(keys[0].dial_url, None, "the deck dials this station");
    assert_eq!(keys[0].scopes, DECK_SCOPES.into_iter().collect());

    let open = node::dial(&deck.node, invitation.addr, paired.key)
        .await
        .unwrap();
    let link = fleetdeck_link::start(&deck.node, open);
    assert_eq!(info(&link.control).await.name, "box");

    // The invitation was single-use.
    let reuse = fleetdeck_link::pair(&deck.node, &invitation, [Scope::Read]).await;
    assert!(reuse.is_err());

    // After a restart the station listens on the same address.
    node.shutdown();
    link.control.close("test", "station restart");
    let node = station.node(&[]).await;
    assert_eq!(node.listen_addrs().await, vec![invitation.addr]);
    let open = node::dial(&deck.node, invitation.addr, paired.key)
        .await
        .unwrap();
    let link = fleetdeck_link::start(&deck.node, open);
    assert_eq!(info(&link.control).await.name, "box");
}

#[tokio::test]
async fn bad_invitations_public_addresses_and_wrong_secrets_pair_nothing() {
    let deck = deck().await;
    let station = station().await;
    let node = station.node(&[]).await;

    // D13: a public listen address is refused.
    let public = node.invite("8.8.8.8:7443".parse().unwrap()).await;
    assert!(
        matches!(public, Err(NodeError::Listen { .. })),
        "{public:?}"
    );
    assert!(station.store.listen_addrs().await.unwrap().is_empty());

    let garbage = node.join(&Secret::new("fdinv1:nonsense".to_owned())).await;
    assert!(
        matches!(garbage, Err(NodeError::Invitation(_))),
        "{garbage:?}"
    );

    // A wrong secret: the deck refuses the proof; nothing is pinned.
    let text = deck.invitation();
    let good = text.expose().clone();
    let (head, expiry) = good.rsplit_once('.').unwrap();
    let (head, secret) = head.rsplit_once('.').unwrap();
    let flipped: String = secret
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    let wrong = Secret::new(format!("{head}.{flipped}.{expiry}"));
    let refused = node.join(&wrong).await;
    assert!(matches!(refused, Err(NodeError::Pairing(_))), "{refused:?}");
    assert!(station.store.paired_keys().await.unwrap().is_empty());

    // A station's own invitation cannot pair it with itself.
    let own = node.invite("127.0.0.1:0".parse().unwrap()).await.unwrap();
    assert!(matches!(node.join(&own).await, Err(NodeError::NotADeck)));

    // The good invitation still works (one wrong proof does not cancel it).
    node.join(&text).await.unwrap();
    assert_eq!(station.store.paired_keys().await.unwrap().len(), 1);
}
