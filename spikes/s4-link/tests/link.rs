//! Link v1 over loopback: handshake, feature negotiation, both dial
//! directions, requests in both directions, scopes, link epochs, and the data
//! channel with credit flow control (PLAN §8.1-§8.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use s4_link::frames::{ControlFrame, Outcome, Scope};
use s4_link::hello::{Channel, feature};
use s4_link::identity::Identity;
use s4_link::link::{self, DataEvent, Inbound, Link, NodeConfig};
use s4_link::tls::Role;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

fn lo() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A paired deck and station: each pins the other with its role. The deck
/// grants the station only `coordinate`; the station grants the deck
/// `read`, `drive` and `approve`.
fn paired() -> (NodeConfig, NodeConfig) {
    let deck = NodeConfig::new(Arc::new(Identity::generate().unwrap()), Role::Deck);
    let station = NodeConfig::new(Arc::new(Identity::generate().unwrap()), Role::Station);
    let (dk, sk) = (deck.identity.public_key(), station.identity.public_key());
    deck.pins.pin(sk, Role::Station);
    station.pins.pin(dk, Role::Deck);
    deck.grants.grant(sk, [Scope::Coordinate]);
    station
        .grants
        .grant(dk, [Scope::Read, Scope::Drive, Scope::Approve]);
    (deck, station)
}

async fn next_inbound(link: &mut Link) -> Inbound {
    tokio::time::timeout(T, link.inbound.recv())
        .await
        .expect("inbound timeout")
        .expect("inbound ended")
}

/// Answers the next inbound request with `{"served": method}`.
async fn serve_one(link: &mut Link) -> String {
    match next_inbound(link).await {
        Inbound::Request { id, method, .. } => {
            link.control
                .respond(id, Outcome::Ok(json!({"served": method})))
                .await
                .unwrap();
            method
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

async fn link_both_ways(listener_node: NodeConfig, dialer_node: NodeConfig) -> (Link, Link) {
    let mut l = link::listen(listener_node, lo()).await.unwrap();
    let addr = l.local_addr;
    let dialer = tokio::spawn(async move { link::dial(&dialer_node, addr, 1).await });
    let accepted = tokio::time::timeout(T, l.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let dialed = dialer.await.unwrap().unwrap();
    (dialed, accepted)
}

/// Station dials (the NAT case) or deck dials: requests flow both ways,
/// events and acks pass, and ping works on both sides.
async fn exercise(mut deck: Link, mut station: Link) {
    assert_eq!(deck.peer_role, Role::Station);
    assert_eq!(station.peer_role, Role::Deck);
    assert_eq!(deck.negotiated, station.negotiated);
    assert!(deck.negotiated.has(feature::DATA_STREAMS));
    assert_eq!(deck.epoch, station.epoch);

    let req = deck.control.clone();
    let call = tokio::spawn(async move { req.request("session.list", None, json!({})).await });
    assert_eq!(serve_one(&mut station).await, "session.list");
    assert_eq!(
        call.await.unwrap().unwrap(),
        Outcome::Ok(json!({"served": "session.list"}))
    );

    let req = station.control.clone();
    let call = tokio::spawn(async move {
        req.request("coordinator.call", Some("01JCMD"), json!({"op": "x"}))
            .await
    });
    assert_eq!(serve_one(&mut deck).await, "coordinator.call");
    assert!(matches!(call.await.unwrap().unwrap(), Outcome::Ok(_)));

    let event = ControlFrame::Event {
        station_seq: 1,
        session_id: "s1".into(),
        session_seq: 1,
        event: json!({"kind": "turn.started"}),
    };
    station.control.send(event.clone()).await.unwrap();
    assert_eq!(next_inbound(&mut deck).await, Inbound::Frame(event));
    deck.control
        .send(ControlFrame::Ack { station_seq: 1 })
        .await
        .unwrap();
    assert_eq!(
        next_inbound(&mut station).await,
        Inbound::Frame(ControlFrame::Ack { station_seq: 1 })
    );

    deck.control.ping().await.unwrap();
    station.control.ping().await.unwrap();
}

#[tokio::test]
async fn the_station_dials_the_deck() {
    let (deck, station) = paired();
    let (station_link, deck_link) = link_both_ways(deck, station).await;
    assert_eq!(station_link.side, s4_link::hello::Side::Dialer);
    exercise(deck_link, station_link).await;
}

#[tokio::test]
async fn the_deck_dials_the_station() {
    let (deck, station) = paired();
    let (deck_link, station_link) = link_both_ways(station, deck).await;
    assert_eq!(deck_link.side, s4_link::hello::Side::Dialer);
    exercise(deck_link, station_link).await;
}

#[tokio::test]
async fn no_common_version_fails_on_both_sides_with_the_same_reason() {
    let (deck, mut station) = paired();
    (station.local.proto_min, station.local.proto_max) = (2, 3);
    let mut l = link::listen(deck, lo()).await.unwrap();
    let dial = link::dial(&station, l.local_addr, 1).await;
    let dial_err = dial.expect_err("the dial must fail");
    assert_eq!(dial_err.reason(), Some("version_mismatch"), "{dial_err}");
    let listen_err = tokio::time::timeout(T, l.next())
        .await
        .unwrap()
        .unwrap()
        .expect_err("the accept must fail");
    assert_eq!(
        listen_err.reason(),
        Some("version_mismatch"),
        "{listen_err}"
    );
    assert!(
        listen_err.to_string().contains("1..=1") && listen_err.to_string().contains("2..=3"),
        "{listen_err}"
    );
}

#[tokio::test]
async fn features_are_negotiated_and_gate_methods_and_channels() {
    let (deck, mut station) = paired();
    station.local.features = vec![feature::CONTROL.to_owned()];
    let (mut station_link, deck_link) = link_both_ways(deck, station).await;
    assert!(
        deck_link.data.is_none() && station_link.data.is_none(),
        "no data channel without data_streams.v1"
    );
    assert!(!deck_link.negotiated.has(feature::SNAPSHOTS));
    let Outcome::Err(e) = deck_link
        .control
        .request("session.snapshot", None, json!({}))
        .await
        .unwrap()
    else {
        panic!("snapshot must be refused")
    };
    assert_eq!(e.code, "feature_not_negotiated");
    // the refused request never reached the station application
    assert!(station_link.inbound.try_recv().is_err());
}

#[tokio::test]
async fn requests_are_checked_against_the_method_table_and_the_granted_scopes() {
    let (deck, station) = paired();
    let station_grants = station.grants.clone();
    let deck_key = deck.identity.public_key();
    let (deck_link, mut station_link) = link_both_ways(station, deck).await;
    let code = |o: Outcome| match o {
        Outcome::Err(e) => e.code,
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    };
    let c = &deck_link.control;
    assert_eq!(
        code(
            c.request("station.configure", Some("01J1"), json!({}))
                .await
                .unwrap()
        ),
        "forbidden"
    );
    assert_eq!(
        code(c.request("shell.exec", None, json!({})).await.unwrap()),
        "unknown_method"
    );
    assert_eq!(
        code(c.request("session.prompt", None, json!({})).await.unwrap()),
        "missing_command_id"
    );
    // narrowing takes effect on the next request
    station_grants.grant(deck_key, [Scope::Approve]);
    assert_eq!(
        code(c.request("session.list", None, json!({})).await.unwrap()),
        "forbidden"
    );
    station_grants.revoke(&deck_key);
    assert_eq!(
        code(
            c.request("approval.answer", Some("01J2"), json!({}))
                .await
                .unwrap()
        ),
        "forbidden"
    );
    assert!(
        station_link.inbound.try_recv().is_err(),
        "no refused request reached the application"
    );
}

#[tokio::test]
async fn a_higher_link_epoch_supersedes_and_a_lower_one_is_refused() {
    let (deck, station) = paired();
    let mut l = link::listen(deck, lo()).await.unwrap();
    let addr = l.local_addr;
    let mut a = link::dial(&station, addr, 5).await.unwrap();
    let mut a_deck = l.next_link().await.unwrap();
    assert_eq!(a_deck.epoch, 5);

    let stale = link::dial(&station, addr, 4)
        .await
        .expect_err("a lower epoch must be refused");
    assert_eq!(stale.reason(), Some("stale_link_epoch"), "{stale}");
    let equal = link::dial(&station, addr, 5)
        .await
        .expect_err("an equal epoch must be refused");
    assert_eq!(equal.reason(), Some("stale_link_epoch"));
    assert!(matches!(l.next().await, Some(Err(_))));
    assert!(matches!(l.next().await, Some(Err(_))));
    a.control
        .ping()
        .await
        .expect("the current link still works");

    let b = link::dial(&station, addr, 6).await.unwrap();
    let b_deck = l.next_link().await.unwrap();
    assert_eq!(b_deck.epoch, 6);
    for old in [&mut a_deck, &mut a] {
        match next_inbound(old).await {
            Inbound::Closed { reason, .. } => assert_eq!(reason, "superseded"),
            other => panic!("expected Closed, got {other:?}"),
        }
    }
    b.control.ping().await.unwrap();
    b_deck.control.ping().await.unwrap();
}

#[tokio::test]
async fn a_data_channel_needs_its_control_channel() {
    let (deck, station) = paired();
    let mut l = link::listen(deck, lo()).await.unwrap();
    let addr = l.local_addr;
    let e = link::dial_lone_channel(&station, addr, Channel::Data, 1)
        .await
        .expect_err("must be refused");
    assert!(e.to_string().contains("no control channel"), "{e}");
    let le = l.next().await.unwrap().err().unwrap();
    assert_eq!(le.reason(), Some("no_control_channel"));
    // a live link of epoch 2 does not adopt a stray data channel of epoch 1
    let _link = link::dial(&station, addr, 2).await.unwrap();
    let _deck = l.next_link().await.unwrap();
    assert!(
        link::dial_lone_channel(&station, addr, Channel::Data, 1)
            .await
            .is_err()
    );
}

async fn read_stream(
    data: &link::Data,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<DataEvent>,
) -> (String, Vec<u8>) {
    let mut purpose = String::new();
    let mut buf = Vec::new();
    loop {
        match tokio::time::timeout(T, events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            DataEvent::Opened { purpose: p, .. } => purpose = p,
            DataEvent::Data { stream, bytes } => {
                buf.extend_from_slice(&bytes);
                data.consumed(stream, bytes.len()).await.unwrap();
            }
            DataEvent::Closed { .. } => return (purpose, buf),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_data_channel_streams_a_mebibyte_each_way_with_flow_control() {
    let (deck, station) = paired();
    let (station_link, deck_link) = link_both_ways(deck, station).await;
    let (s_data, mut s_events) = station_link.data.unwrap();
    let (d_data, mut d_events) = deck_link.data.unwrap();
    let payload = |seed: u8| {
        (0..1024 * 1024)
            .map(|i| (i as u8).wrapping_mul(seed))
            .collect::<Vec<u8>>()
    };
    let send = |data: link::Data, purpose: &'static str, bytes: Vec<u8>| {
        tokio::spawn(async move {
            let s = data.open(purpose).await.unwrap();
            data.write(s, bytes).await.unwrap();
            data.close(s).await.unwrap();
        })
    };
    // 1 MiB is four times the credit window, so both writers must wait for credit.
    let up = send(s_data.clone(), "artifact:up", payload(3));
    let down = send(d_data.clone(), "artifact:down", payload(7));
    let (p1, got_up) = read_stream(&d_data, &mut d_events).await;
    let (p2, got_down) = read_stream(&s_data, &mut s_events).await;
    up.await.unwrap();
    down.await.unwrap();
    assert_eq!((p1.as_str(), p2.as_str()), ("artifact:up", "artifact:down"));
    assert!(got_up == payload(3), "upload bytes differ");
    assert!(got_down == payload(7), "download bytes differ");
}

#[tokio::test]
async fn a_reader_that_gives_no_credit_stops_the_writer() {
    let (deck, station) = paired();
    let (station_link, deck_link) = link_both_ways(deck, station).await;
    let (s_data, _s_events) = station_link.data.unwrap();
    let (_d_data, mut d_events) = deck_link.data.unwrap();
    let s = s_data.open("artifact:slow").await.unwrap();
    let writer = tokio::spawn(async move { s_data.write(s, vec![1; 1024 * 1024]).await });
    let mut received = 0usize;
    // The deck reads but never calls consumed(): the station stops at 256 KiB.
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(500), d_events.recv()).await
    {
        if let DataEvent::Data { bytes, .. } = ev {
            received += bytes.len();
        }
    }
    assert_eq!(received, s4_link::mux::INITIAL_CREDIT as usize);
    assert!(!writer.is_finished(), "the writer waits for credit");
    writer.abort();
}
