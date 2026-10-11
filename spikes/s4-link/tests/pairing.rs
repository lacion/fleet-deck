//! Invitation pairing in both directions (PLAN §8.5) and reconnect with
//! backoff and growing link epochs (§8.1), over loopback in one process.
//! `tests/multiprocess.rs` repeats the main paths with separate processes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use s4_link::frames::{Outcome, Scope};
use s4_link::identity::Identity;
use s4_link::link::{self, Inbound, Link, LinkListener, NodeConfig};
use s4_link::pairing::{DEFAULT_TTL, Invitation};
use s4_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use s4_link::secret::Secret;
use s4_link::tls::Role;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

fn lo() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn node(role: Role) -> NodeConfig {
    NodeConfig::new(Arc::new(Identity::generate().unwrap()), role)
}

/// Listens and opens one invitation that grants the joiner `scopes`.
async fn inviter(node: &NodeConfig, scopes: &[Scope]) -> (LinkListener, Invitation) {
    let l = link::listen(node.clone(), lo()).await.unwrap();
    let inv = node
        .invites
        .create(
            l.local_addr,
            node.identity.fingerprint(),
            DEFAULT_TTL,
            scopes.iter().copied(),
        )
        .unwrap();
    (l, inv)
}

async fn accept(l: &mut LinkListener) -> Link {
    tokio::time::timeout(T, l.next())
        .await
        .expect("accept timeout")
        .expect("listener ended")
        .expect("link refused")
}

/// `from` asks `to` for `station.info`; `to` answers.
async fn round_trip(from: &Link, to: &mut Link) {
    let ask = from.control.clone();
    let call =
        tokio::spawn(async move { ask.request("station.info", None, json!({})).await.unwrap() });
    match tokio::time::timeout(T, to.inbound.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Inbound::Request { id, method, .. } => {
            assert_eq!(method, "station.info");
            to.control
                .respond(id, Outcome::Ok(json!({"ok": true})))
                .await
                .unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(call.await.unwrap(), Outcome::Ok(_)));
}

#[tokio::test]
async fn a_station_without_a_listener_joins_the_deck_and_dials_it() {
    let deck = node(Role::Deck);
    let station = node(Role::Station);
    let (mut l, inv) = inviter(&deck, &[Scope::Coordinate]).await;
    // The invitation travels as text (copy and paste by the owner).
    let text = inv.to_uri();
    let parsed: Invitation = text.parse().unwrap();
    let deck_key = link::pair(&station, &parsed, [Scope::Read, Scope::Drive])
        .await
        .unwrap();
    assert_eq!(deck_key, deck.identity.public_key());
    assert_eq!(station.pins.role_of(&deck_key), Some(Role::Deck));
    let joined = tokio::time::timeout(T, deck.invites.next_paired())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(joined.key, station.identity.public_key());
    assert_eq!(joined.role, Role::Station);
    assert_eq!(deck.grants.of(&joined.key), [Scope::Coordinate].into());
    assert_eq!(deck.invites.open_count(), 0, "the invitation is used up");

    // The station has no listener (NAT): it dials, and the deck drives it
    // over that link.
    let addr = l.local_addr;
    let st = station.clone();
    let dialed = tokio::spawn(async move { link::dial(&st, addr, 1).await });
    let mut at_deck = accept(&mut l).await;
    let mut at_station = dialed.await.unwrap().unwrap();
    round_trip(&at_deck, &mut at_station).await;
    assert_eq!(at_deck.peer_role, Role::Station);
    at_deck.inbound.close();
    at_station.inbound.close();
}

#[tokio::test]
async fn the_deck_joins_a_station_invitation_and_dials_the_station() {
    let deck = node(Role::Deck);
    let station = node(Role::Station);
    let (mut l, inv) = inviter(&station, &[Scope::Read, Scope::Drive]).await;
    link::pair(&deck, &inv, [Scope::Coordinate]).await.unwrap();
    assert_eq!(
        deck.pins.role_of(&station.identity.public_key()),
        Some(Role::Station)
    );
    let addr = l.local_addr;
    let dk = deck.clone();
    let dialed = tokio::spawn(async move { link::dial(&dk, addr, 1).await });
    let mut at_station = accept(&mut l).await;
    let at_deck = dialed.await.unwrap().unwrap();
    round_trip(&at_deck, &mut at_station).await;
}

#[tokio::test]
async fn an_invitation_works_once() {
    let deck = node(Role::Deck);
    let (_l, inv) = inviter(&deck, &[]).await;
    // A second invitation stays open, so TLS still lets strangers in and the
    // refusal comes from the proof.
    let _other = deck
        .invites
        .create(
            inv.addr,
            deck.identity.fingerprint(),
            DEFAULT_TTL,
            [Scope::Read],
        )
        .unwrap();
    let first = node(Role::Station);
    link::pair(&first, &inv, []).await.unwrap();
    let second = node(Role::Station);
    let err = link::pair(&second, &inv, []).await.unwrap_err();
    assert_eq!(err.reason(), Some("invalid_invitation"), "{err}");
    assert!(second.pins.is_empty());
    assert_eq!(deck.pins.len(), 1);
}

#[tokio::test]
async fn expired_invitations_fail_on_both_sides() {
    let deck = node(Role::Deck);
    let l = link::listen(deck.clone(), lo()).await.unwrap();
    let fp = deck.identity.fingerprint();
    let old = deck
        .invites
        .create(l.local_addr, fp, Duration::ZERO, [])
        .unwrap();
    let station = node(Role::Station);
    let err = link::pair(&station, &old, []).await.unwrap_err();
    assert_eq!(err.reason(), Some("invitation_expired"));
    // A joiner with a wrong clock sends the old secret anyway: the deck's own
    // expiry decides.
    let _open = deck
        .invites
        .create(l.local_addr, fp, DEFAULT_TTL, [])
        .unwrap();
    let forged = Invitation {
        addr: old.addr,
        fingerprint: fp,
        secret: Secret::new(*old.secret.expose()),
        expiry: u64::MAX,
    };
    let err = link::pair(&station, &forged, []).await.unwrap_err();
    assert_eq!(err.reason(), Some("invalid_invitation"));
    assert!(deck.pins.is_empty());
}

#[tokio::test]
async fn the_joiner_accepts_only_the_invited_key() {
    let deck = node(Role::Deck);
    let (_l, inv) = inviter(&deck, &[]).await;
    let other = Identity::generate().unwrap();
    let wrong = Invitation {
        addr: inv.addr,
        fingerprint: other.fingerprint(),
        secret: Secret::new(*inv.secret.expose()),
        expiry: inv.expiry,
    };
    let station = node(Role::Station);
    assert!(link::pair(&station, &wrong, []).await.is_err());
    assert!(station.pins.is_empty());
    assert!(deck.pins.is_empty());
    assert_eq!(
        deck.invites.open_count(),
        1,
        "the invitation is still unused"
    );
}

#[tokio::test]
async fn a_wrong_secret_pairs_nothing() {
    let deck = node(Role::Deck);
    let (_l, inv) = inviter(&deck, &[]).await;
    let mut bad = *inv.secret.expose();
    bad[0] ^= 1;
    let guess = Invitation {
        addr: inv.addr,
        fingerprint: inv.fingerprint,
        secret: Secret::new(bad),
        expiry: inv.expiry,
    };
    let station = node(Role::Station);
    let err = link::pair(&station, &guess, []).await.unwrap_err();
    assert_eq!(err.reason(), Some("invalid_invitation"));
    assert!(station.pins.is_empty() && deck.pins.is_empty());
}

#[tokio::test]
async fn an_unpaired_key_may_only_use_the_pair_channel() {
    let deck = node(Role::Deck);
    let (l, inv) = inviter(&deck, &[]).await;
    let stranger = node(Role::Station);
    // The stranger knows the deck's key (it read the invitation) but did not
    // pair: the open invitation lets it through TLS, the link layer refuses.
    stranger.pins.pin(deck.identity.public_key(), Role::Deck);
    let err = link::dial(&stranger, l.local_addr, 1).await.unwrap_err();
    assert_eq!(err.reason(), Some("not_paired"), "{err}");
    // Without an open invitation TLS itself refuses the unknown key.
    deck.invites.close_all();
    assert!(link::dial(&stranger, l.local_addr, 2).await.is_err());
    assert!(link::pair(&stranger, &inv, []).await.is_err());
    assert!(deck.pins.is_empty());
}

async fn next(events: &mut tokio::sync::mpsc::Receiver<DialerEvent>) -> DialerEvent {
    tokio::time::timeout(T, events.recv())
        .await
        .expect("dialer event timeout")
        .expect("dialer ended")
}

#[tokio::test]
async fn the_dialer_reconnects_with_backoff_and_a_higher_epoch() {
    let deck = node(Role::Deck);
    let station = node(Role::Station);
    let (mut l, inv) = inviter(&deck, &[]).await;
    link::pair(&station, &inv, [Scope::Read]).await.unwrap();
    let addr = l.local_addr;
    let policy = BackoffPolicy {
        base: Duration::from_millis(40),
        cap: Duration::from_millis(400),
        stable: Duration::from_secs(60),
    };
    let (mut events, _task) = spawn_dialer(station.clone(), addr, policy);

    let DialerEvent::Linked(first) = next(&mut events).await else {
        panic!("expected a link")
    };
    let at_deck = accept(&mut l).await;
    assert_eq!(at_deck.epoch, first.epoch);

    // The deck goes away (process stop): the listener and its links end.
    drop(at_deck);
    drop(l);
    let DialerEvent::Lost { retry_in, .. } = next(&mut events).await else {
        panic!("expected the link to end")
    };
    assert!(retry_in >= Duration::from_millis(20) && retry_in <= Duration::from_millis(40));
    let mut failures = Vec::new();
    while failures.len() < 3 {
        match next(&mut events).await {
            DialerEvent::Failed {
                attempt, retry_in, ..
            } => failures.push((attempt, retry_in)),
            other => panic!("{other:?}"),
        }
    }
    // Attempts count up; delays stay in [ceiling/2, ceiling] and grow.
    assert_eq!(failures.iter().map(|f| f.0).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(failures[2].1 >= Duration::from_millis(160));

    // The deck comes back on the same address with the same identity.
    let mut l = link::listen(deck.clone(), addr).await.unwrap();
    let mut second = loop {
        match next(&mut events).await {
            DialerEvent::Linked(link) => break link,
            DialerEvent::Failed { .. } => {}
            other => panic!("{other:?}"),
        }
    };
    let at_deck = accept(&mut l).await;
    assert!(second.epoch > first.epoch);
    assert_eq!(at_deck.epoch, second.epoch);
    // The deck drives the station over the new link (the station granted the
    // deck `read` when it paired; the deck granted the station nothing).
    round_trip(&at_deck, &mut second).await;
}
