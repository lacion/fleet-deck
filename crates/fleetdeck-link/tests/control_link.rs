//! The control channel on real links over loopback (ADR-004 §9.6, G6–G8):
//! roles come from the pairing, not from the dial direction; the link table
//! ends superseded control channels; a closed channel frees its table entry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::{self, Inbound, InboundRequest};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, GrantSource, Grants, LinkListener, LiveLink};
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope};
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::json;

const T: Duration = Duration::from_secs(10);

struct Node {
    config: NodeConfig,
    grants: Arc<Grants>,
}

fn node(role: Role) -> Node {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    let mut config = NodeConfig::new(
        identity,
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    );
    let grants = Grants::new();
    config.grants = Arc::clone(&grants) as Arc<dyn GrantSource>;
    Node { config, grants }
}

/// A deck and a station that paired: the deck may read, drive and approve;
/// the station may coordinate.
fn paired() -> (Node, Node) {
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

fn lo() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn listen(n: &Node) -> LinkListener {
    node::listen(n.config.clone(), lo()).await.unwrap()
}

/// `from` dials `to`; returns (live link at `from`, live link at `to`).
async fn link(from: &Node, to: &Node, l: &mut LinkListener) -> (LiveLink, LiveLink) {
    let peer = to.config.identity.public_key();
    let (d, a) = tokio::time::timeout(T, async {
        tokio::join!(node::dial(&from.config, l.local_addr, peer), l.next())
    })
    .await
    .expect("link setup timeout");
    let d = control::start(&from.config, d.unwrap());
    let a = control::start(&to.config, a.expect("listener ended").unwrap());
    (d, a)
}

async fn next_request(l: &mut LiveLink) -> InboundRequest {
    match tokio::time::timeout(T, l.inbound.recv())
        .await
        .expect("no request")
    {
        Some(Inbound::Request(r)) => r,
        other => panic!("expected a request, got {other:?}"),
    }
}

async fn serve_one(l: &mut LiveLink, answer: serde_json::Value) -> String {
    let r = next_request(l).await;
    l.control.respond(r.id, Outcome::Ok(answer)).await.unwrap();
    r.method
}

fn code(o: &Outcome) -> &str {
    match o {
        Outcome::Err(e) => &e.code,
        Outcome::Ok(_) => "ok",
    }
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn roles_come_from_the_pairing_not_from_the_dial_direction() {
    for station_dials in [true, false] {
        let (deck, station) = paired();
        let (mut at_deck, mut at_station) = if station_dials {
            let mut l = listen(&deck).await;
            let (s, d) = link(&station, &deck, &mut l).await;
            (d, s)
        } else {
            let mut l = listen(&station).await;
            link(&deck, &station, &mut l).await
        };
        let inc = IncarnationId::from("inc-s".to_owned());

        // deck → station requests with the scopes the station granted.
        let c = at_deck.control.clone();
        let call = tokio::spawn(async move {
            let id = CommandId::from("cmd-1".to_owned());
            c.request("session.interrupt", Some(&id), json!({"session_id": "s1"}))
                .await
        });
        assert_eq!(
            serve_one(&mut at_station, json!("ok")).await,
            "session.interrupt"
        );
        assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!("ok")));
        let id = CommandId::from("cmd-2".to_owned());
        let o = at_deck
            .control
            .request("station.configure", Some(&id), json!({}))
            .await
            .unwrap();
        assert_eq!(code(&o), "forbidden", "admin was not granted");

        // station → deck: a coordinator query; a drive method is the wrong role.
        let c = at_station.control.clone();
        let call =
            tokio::spawn(async move { c.request("coordinator.query", None, json!({})).await });
        assert_eq!(
            serve_one(&mut at_deck, json!([])).await,
            "coordinator.query"
        );
        assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!([])));
        let id = CommandId::from("cmd-3".to_owned());
        let o = at_station
            .control
            .request("session.prompt", Some(&id), json!({}))
            .await
            .unwrap();
        assert_eq!(code(&o), "wrong_role");

        // Stream frames: snapshots station → deck, acks deck → station.
        let end = ControlFrame::SnapshotEnd {
            incarnation: inc.clone(),
            watermark: 3,
        };
        at_station.control.send(end.clone()).await.unwrap();
        assert_eq!(
            at_deck.inbound.recv().await,
            Some(Inbound::Frame(Box::new(end)))
        );
        let ack = ControlFrame::Ack {
            incarnation: inc,
            station_seq: 3,
        };
        at_deck.control.send(ack.clone()).await.unwrap();
        assert_eq!(
            at_station.inbound.recv().await,
            Some(Inbound::Frame(Box::new(ack.clone())))
        );
        assert!(at_station.control.send(ack).await.is_err());
        drop((at_deck, at_station));
        eventually("tables empty", || {
            deck.config.table.is_empty() && station.config.table.is_empty()
        })
        .await;
    }
}

#[tokio::test]
async fn a_superseded_link_ends_its_control_channel_and_the_new_one_serves() {
    let (deck, station) = paired();
    let mut l = listen(&deck).await;
    let (old_s, old_d) = link(&station, &deck, &mut l).await;
    let (new_s, mut new_d) = link(&station, &deck, &mut l).await;
    for old in [&old_s, &old_d] {
        let end = tokio::time::timeout(T, old.control.closed()).await.unwrap();
        assert_eq!(end.reason, "superseded");
    }
    let c = new_s.control.clone();
    let call = tokio::spawn(async move { c.request("coordinator.query", None, json!({})).await });
    serve_one(&mut new_d, json!(1)).await;
    assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!(1)));
    drop((old_s, old_d));
    let fp = station.config.identity.fingerprint();
    assert_eq!(deck.config.table.current(&fp).unwrap().epoch, 2);
}

#[tokio::test]
async fn a_close_ends_both_sides_and_frees_both_table_entries() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let (at_deck, at_station) = link(&deck, &station, &mut l).await;
    let start = tokio::time::Instant::now();
    at_station.control.close("shutdown", "the station stops");
    let end = tokio::time::timeout(T, at_deck.control.closed())
        .await
        .unwrap();
    assert_eq!(
        (end.reason.as_str(), end.detail.as_str()),
        ("shutdown", "the station stops")
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    eventually("tables empty", || {
        deck.config.table.is_empty() && station.config.table.is_empty()
    })
    .await;
    // The live links are still held: the table entry went with the channel.
    assert!(at_station.control.ended().is_some());
}
