//! A peer that finishes TLS and then stalls must never block a dial, a
//! pairing, the reconnect supervisor, or the listener. Every setup step after
//! TLS (WebSocket upgrade on both sides, hello/auth, pairing proofs) is bounded
//! by `NodeConfig::handshake_timeout`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use s4_link::identity::Identity;
use s4_link::link::{self, LinkError, NodeConfig};
use s4_link::net;
use s4_link::pairing::Invitation;
use s4_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use s4_link::secret::Secret;
use s4_link::tls::{self, PeerPolicy, Role};

const LIMIT: Duration = Duration::from_millis(300);
/// Upper bound of a bounded failure (limit + scheduling slack); far below
/// the 10 s default, so a missing timeout fails the test.
const FAST: Duration = Duration::from_secs(3);

fn node(role: Role) -> NodeConfig {
    let mut n = NodeConfig::new(Arc::new(Identity::generate().unwrap()), role);
    n.handshake_timeout = LIMIT;
    n
}

/// A TLS listener that accepts any client key, completes TLS, and then never
/// reads or writes a byte (it never answers the WebSocket upgrade).
async fn stalled_server(identity: Arc<Identity>) -> SocketAddr {
    let tcp = s4_link::net::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = tcp.local_addr().unwrap();
    let config = tls::server_config(&identity, PeerPolicy::AnyKey).unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, from)) = tcp.accept().await {
            if let Ok(conn) = net::handshake_accepted(sock, from, config.clone()).await {
                held.push(conn); // keep the connection open, silent
            }
        }
    });
    addr
}

fn assert_timeout(e: &LinkError, started: Instant) {
    assert_eq!(e.reason(), Some("timeout"), "{e}");
    assert!(started.elapsed() < FAST, "took {:?}", started.elapsed());
}

#[tokio::test]
async fn a_dial_to_a_server_that_never_upgrades_times_out() {
    let stall = Arc::new(Identity::generate().unwrap());
    let addr = stalled_server(stall.clone()).await;
    let station = node(Role::Station);
    station.pins.pin(stall.public_key(), Role::Deck);
    let t = Instant::now();
    let e = link::dial(&station, addr, 1).await.unwrap_err();
    assert_timeout(&e, t);
    assert!(e.to_string().contains("WebSocket upgrade"), "{e}");
}

#[tokio::test]
async fn pairing_with_a_server_that_never_upgrades_times_out() {
    let stall = Arc::new(Identity::generate().unwrap());
    let addr = stalled_server(stall.clone()).await;
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    let invitation = Invitation {
        addr,
        fingerprint: stall.fingerprint(),
        secret: Secret::new([7u8; 16]),
        expiry,
    };
    let station = node(Role::Station);
    let t = Instant::now();
    let e = link::pair(&station, &invitation, []).await.unwrap_err();
    assert_timeout(&e, t);
    assert!(station.pins.is_empty(), "nothing is pinned");
}

#[tokio::test]
async fn the_reconnect_supervisor_keeps_retrying_a_stalled_server() {
    let stall = Arc::new(Identity::generate().unwrap());
    let addr = stalled_server(stall.clone()).await;
    let station = node(Role::Station);
    station.pins.pin(stall.public_key(), Role::Deck);
    let policy = BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(100),
        stable: Duration::from_secs(60),
    };
    let (mut events, task) = spawn_dialer(station, addr, policy);
    let t = Instant::now();
    for want in 0..3u32 {
        match tokio::time::timeout(FAST, events.recv()).await.unwrap() {
            Some(DialerEvent::Failed {
                attempt, reason, ..
            }) => {
                assert_eq!(attempt, want);
                assert_eq!(reason, "timeout");
            }
            other => panic!("expected a failed attempt, got {other:?}"),
        }
    }
    assert!(t.elapsed() < FAST * 2, "took {:?}", t.elapsed());
    task.abort();
}

#[tokio::test]
async fn a_client_that_never_sends_the_upgrade_does_not_block_the_listener() {
    let deck = node(Role::Deck);
    let stall = Arc::new(Identity::generate().unwrap());
    deck.pins.pin(stall.public_key(), Role::Station);
    let mut listener = link::listen(deck.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let addr = listener.local_addr;

    // The stalled client completes TLS and then sends nothing.
    let config =
        tls::client_config(&stall, PeerPolicy::Fingerprint(deck.identity.fingerprint())).unwrap();
    let (_held, _) = net::connect(addr, config).await.unwrap();

    // A well-behaved station dials at the same time and links.
    let station = node(Role::Station);
    station.pins.pin(deck.identity.public_key(), Role::Deck);
    deck.pins.pin(station.identity.public_key(), Role::Station);
    let dialing = tokio::spawn(async move { link::dial(&station, addr, 1).await });

    let t = Instant::now();
    let mut linked = false;
    let mut timed_out = false;
    while !(linked && timed_out) {
        match tokio::time::timeout(FAST, listener.next()).await.unwrap() {
            Some(Ok(_link)) => linked = true,
            Some(Err(e)) => {
                assert_eq!(e.reason(), Some("timeout"), "{e}");
                timed_out = true;
            }
            None => panic!("listener ended"),
        }
    }
    assert!(t.elapsed() < FAST, "took {:?}", t.elapsed());
    assert!(dialing.await.unwrap().is_ok());
}
