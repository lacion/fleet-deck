//! Helpers for link tests over loopback: paired nodes, link setup, waits.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::{Inbound, InboundRequest};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{Outcome, Scope};

pub const T: Duration = Duration::from_secs(10);

pub struct Node {
    pub config: NodeConfig,
    pub grants: Arc<Grants>,
}

pub fn node(role: Role) -> Node {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    let grants = Grants::new();
    let config = NodeConfig::new(
        identity,
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&grants));
    Node { config, grants }
}

/// A deck and a station that paired: the deck may read, drive and approve;
/// the station may coordinate.
pub fn paired() -> (Node, Node) {
    let deck = node(Role::Deck);
    let station = node(Role::Station);
    deck.config
        .pins
        .pin(station.config.identity.public_key(), Role::Station);
    station
        .config
        .pins
        .pin(deck.config.identity.public_key(), Role::Deck);
    station.grants.grant(
        deck.config.identity.fingerprint(),
        [Scope::Read, Scope::Drive, Scope::Approve],
    );
    deck.grants
        .grant(station.config.identity.fingerprint(), [Scope::Coordinate]);
    (deck, station)
}

pub fn lo() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

pub async fn listen(n: &Node) -> LinkListener {
    node::listen(n.config.clone(), lo()).await.unwrap()
}

/// `from` dials `to`; returns (live link at `from`, live link at `to`).
/// Errors of earlier attempts on `l` (pairing failures) are skipped.
pub async fn link(from: &Node, to: &Node, l: &mut LinkListener) -> (LiveLink, LiveLink) {
    let peer = to.config.identity.public_key();
    let (d, a) = tokio::time::timeout(T, async {
        tokio::join!(node::dial(&from.config, l.local_addr, peer), l.next_link())
    })
    .await
    .expect("link setup timeout");
    let d = fleetdeck_link::start(&from.config, d.unwrap());
    let a = fleetdeck_link::start(&to.config, a.expect("listener ended"));
    (d, a)
}

pub async fn next_request(l: &mut LiveLink) -> InboundRequest {
    match tokio::time::timeout(T, l.inbound.recv())
        .await
        .expect("no request")
    {
        Some(Inbound::Request(r)) => r,
        other => panic!("expected a request, got {other:?}"),
    }
}

pub async fn serve_one(l: &mut LiveLink, answer: serde_json::Value) -> String {
    let r = next_request(l).await;
    l.control.respond(r.id, Outcome::Ok(answer)).await.unwrap();
    r.method
}

pub fn code(o: &Outcome) -> &str {
    match o {
        Outcome::Err(e) => &e.code,
        Outcome::Ok(_) => "ok",
    }
}

pub async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
