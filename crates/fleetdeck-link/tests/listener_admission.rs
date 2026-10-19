//! The pending-handshake admission of a link listener (FDV2-001): 16 per
//! source and 64 in all, released when the handshake ends, and no permit for
//! an established link.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(
    not(target_os = "linux"),
    allow(unused_imports, dead_code, reason = "most tests here are Linux-only")
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, LinkListener, OpenLink};
use fleetdeck_proto::IncarnationId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};

const T: Duration = Duration::from_secs(10);

fn node(role: Role) -> NodeConfig {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    NodeConfig::new(
        identity,
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    )
}

async fn listen(n: &NodeConfig) -> LinkListener {
    node::listen(n.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap()
}

/// A TCP connection to `to` from the loopback address `127.0.0.<last>`.
async fn connect(to: SocketAddr, last: u8) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket
        .bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)),
            0,
        ))
        .unwrap();
    socket.connect(to).await.unwrap()
}

async fn eventually(what: &str, ok: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// True when the listener closes `s` at once (a refused connection).
async fn refused(s: &mut TcpStream) -> bool {
    let mut buf = [0u8; 16];
    matches!(
        tokio::time::timeout(Duration::from_secs(3), s.read(&mut buf)).await,
        Ok(Ok(0) | Err(_))
    )
}

#[tokio::test]
async fn the_17th_pending_connection_of_one_ip_is_refused() {
    let deck = node(Role::Deck);
    let l = listen(&deck).await;
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(connect(l.local_addr, 1).await);
    }
    eventually("16 pending handshakes", || l.admitted() == 16).await;
    let mut seventeenth = connect(l.local_addr, 1).await;
    assert!(refused(&mut seventeenth).await);
    assert_eq!(l.admitted(), 16);
    // Another source still gets a place (Linux has the whole 127.0.0.0/8
    // range on loopback).
    #[cfg(target_os = "linux")]
    let _other = {
        let c = connect(l.local_addr, 2).await;
        eventually("a second source", || l.admitted() == 17).await;
        c
    };
    // The permits go with the connections.
    drop(held);
    eventually("the permits are released", || l.admitted() <= 1).await;
}

#[tokio::test]
async fn a_failed_handshake_releases_its_permit() {
    let deck = node(Role::Deck);
    let l = listen(&deck).await;
    for _ in 0..3 {
        let mut s = connect(l.local_addr, 1).await;
        s.write_all(b"this is not a TLS client hello\r\n\r\n")
            .await
            .unwrap();
        // The handshake fails and the listener closes the stream.
        let mut buf = [0u8; 256];
        let _ = tokio::time::timeout(T, s.read(&mut buf)).await.unwrap();
        eventually("the permit of the failed handshake", || l.admitted() == 0).await;
    }
}

// Needs several source addresses on loopback.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn established_links_hold_no_permit() {
    let deck = node(Role::Deck);
    let mut l = listen(&deck).await;
    let mut links: Vec<(OpenLink, OpenLink)> = Vec::new();
    for _ in 0..10 {
        let station = node(Role::Station);
        deck.pins.pin(station.identity.public_key(), Role::Station);
        station.pins.pin(deck.identity.public_key(), Role::Deck);
        let (dialed, accepted) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&station, l.local_addr, deck.identity.public_key()),
                l.next_link()
            )
        })
        .await
        .expect("link setup timeout");
        links.push((dialed.unwrap(), accepted.expect("listener ended")));
    }
    assert_eq!(links.len(), 10);
    eventually("no permit for the 10 links", || l.admitted() == 0).await;

    // 64 pending connections fill the listener: 4 sources of 16.
    let mut pending = Vec::new();
    for source in 11..=14 {
        for _ in 0..16 {
            pending.push(connect(l.local_addr, source).await);
        }
    }
    eventually("64 pending handshakes", || l.admitted() == 64).await;
    // The 65th, from a fifth source, is refused.
    let mut last = connect(l.local_addr, 15).await;
    assert!(refused(&mut last).await);
    assert_eq!(l.admitted(), 64);
    drop(links);
}
