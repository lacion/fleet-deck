//! `fleetdeck station join | invite | forget-deck` against a running
//! `fleetdeck station serve` process (M2 P3-5c). The deck is an in-process
//! `fleetdeck-link` node on loopback; the station is the real binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use common::Station;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::Control;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::pairing::{Invitation, Invitations};
use fleetdeck_link::revoke::REVOKED;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope};
use serde_json::json;

const T: Duration = Duration::from_secs(20);

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
    async fn accept(&mut self) -> LiveLink {
        let open = tokio::time::timeout(T, self.listener.next_link())
            .await
            .expect("the station dials the deck")
            .unwrap();
        fleetdeck_link::start(&self.node, open)
    }
}

async fn station_name(control: &Control) -> String {
    let outcome = tokio::time::timeout(T, control.request("station.info", None, json!({})))
        .await
        .expect("answer")
        .unwrap();
    let Outcome::Ok(v) = outcome else {
        panic!("station.info failed: {outcome:?}");
    };
    v["name"].as_str().unwrap().to_owned()
}

/// Runs a blocking CLI call without stalling the deck's tasks.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_dials_the_deck_redials_after_a_restart_and_forget_deck_closes_the_link() {
    let mut deck = deck().await;
    let station = Arc::new(std::sync::Mutex::new(Station::start()));
    let invitation = deck
        .invitations
        .create(
            deck.listener.local_addr,
            deck.identity.fingerprint(),
            Duration::from_secs(60),
            [Scope::Read],
        )
        .unwrap()
        .reveal();
    // `fdinv1:wss://ADDR#FP.ID.SECRET.EXPIRY`: the secret is the third part.
    let secret = invitation.expose().rsplit('.').nth(1).unwrap().to_owned();
    assert_eq!(secret.len(), 32);
    let file = station.lock().unwrap().node.root().join("deck.inv");
    std::fs::write(&file, invitation.expose()).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    let s = Arc::clone(&station);
    let path = file.clone();
    let out = blocking(move || {
        s.lock()
            .unwrap()
            .ok(&["station", "join", "--invite-file", path.to_str().unwrap()])
    })
    .await;
    assert!(
        out.contains(&deck.identity.fingerprint().to_string()),
        "{out}"
    );
    assert!(!out.contains(&secret), "the invitation is never printed");

    let link = deck.accept().await;
    let name = station_name(&link.control).await;
    assert!(!name.is_empty());

    // A second join is refused: one deck per station (DL-048).
    let s = Arc::clone(&station);
    let err = blocking(move || {
        s.lock()
            .unwrap()
            .fails(&["station", "join", "--invite-file", file.to_str().unwrap()])
    })
    .await;
    assert!(err.contains("already paired"), "{err}");
    assert!(!err.contains(&secret), "{err}");

    // A restarted station dials the deck from its stored pin and address.
    let s = Arc::clone(&station);
    blocking(move || {
        let mut s = s.lock().unwrap();
        s.stop();
        s.spawn(None);
    })
    .await;
    let link = deck.accept().await;
    assert_eq!(station_name(&link.control).await, name);

    // `forget-deck` closes the link within 1 s.
    let s = Arc::clone(&station);
    let out = blocking(move || s.lock().unwrap().ok(&["station", "forget-deck"])).await;
    assert!(out.contains("forgot 1 deck key"), "{out}");
    let closing = tokio::time::timeout(Duration::from_secs(1), link.control.closed())
        .await
        .expect("closed within 1 s");
    assert_eq!(closing.reason, REVOKED);
    let s = Arc::clone(&station);
    let out = blocking(move || s.lock().unwrap().ok(&["station", "forget-deck"])).await;
    assert!(out.contains("no deck was paired"), "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invite_prints_an_invitation_that_pairs_the_deck_and_the_listener_survives_a_restart() {
    let deck = deck().await;
    let station = Arc::new(std::sync::Mutex::new(Station::start()));
    let s = Arc::clone(&station);
    let out = blocking(move || {
        s.lock()
            .unwrap()
            .ok(&["station", "invite", "--listen", "127.0.0.1:0"])
    })
    .await;
    let text = Secret::new(out.trim().to_owned());
    let invitation = Invitation::parse(&text).unwrap();
    assert!(invitation.addr.ip().is_loopback());
    assert_ne!(invitation.addr.port(), 0, "the bound port is announced");

    let paired = fleetdeck_link::pair(&deck.node, &invitation, [Scope::Read])
        .await
        .unwrap();
    let open = node::dial(&deck.node, invitation.addr, paired.key)
        .await
        .unwrap();
    let link = fleetdeck_link::start(&deck.node, open);
    let name = station_name(&link.control).await;

    // Paired: a new invitation is refused.
    let s = Arc::clone(&station);
    let err = blocking(move || {
        s.lock()
            .unwrap()
            .fails(&["station", "invite", "--listen", "127.0.0.1:0"])
    })
    .await;
    assert!(err.contains("already paired"), "{err}");

    // After a restart the station listens on the same address again.
    let s = Arc::clone(&station);
    blocking(move || {
        let mut s = s.lock().unwrap();
        s.stop();
        s.spawn(None);
    })
    .await;
    let mut tries = 0;
    let open = loop {
        match node::dial(&deck.node, invitation.addr, paired.key).await {
            Ok(open) => break open,
            Err(_) if tries < 100 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("the restarted station does not listen: {e}"),
        }
    };
    let link = fleetdeck_link::start(&deck.node, open);
    assert_eq!(station_name(&link.control).await, name);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_listen_address_is_refused() {
    let station = Arc::new(std::sync::Mutex::new(Station::start()));
    let s = Arc::clone(&station);
    let err = blocking(move || {
        s.lock()
            .unwrap()
            .fails(&["station", "invite", "--listen", "203.0.113.7:7443"])
    })
    .await;
    assert!(err.contains("203.0.113.7:7443"), "{err}");
}
