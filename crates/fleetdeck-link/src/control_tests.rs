//! Control channel tests over in-memory WebSocket pairs. The paused tokio
//! clock is the injected clock of the heartbeat and limit tests (G8).

use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope, WireError};
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::DuplexStream;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role as WsRole;

use super::*;
use crate::hello::Channel;
use crate::ws::ws_config;

type Raw = WebSocketStream<DuplexStream>;

async fn ws_pair(buffer: usize) -> (Raw, Raw) {
    let (a, b) = tokio::io::duplex(buffer);
    let cfg = Some(ws_config(Channel::Control));
    let a = WebSocketStream::from_raw_socket(a, WsRole::Client, cfg).await;
    let b = WebSocketStream::from_raw_socket(b, WsRole::Server, cfg).await;
    (a, b)
}

fn negotiated(features: &[&str]) -> Negotiated {
    Negotiated {
        version: PROTO_MAX,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn other(role: Role) -> Role {
    match role {
        Role::Deck => Role::Station,
        Role::Station => Role::Deck,
    }
}

struct Side {
    control: Control,
    inbound: mpsc::Receiver<Inbound>,
    grants: Arc<Grants>,
    peer: Fingerprint,
    /// The link table's signal for this side.
    table: watch::Sender<Option<Closing>>,
}

fn side(ws: Raw, local: Role, features: &[&str], config: ControlConfig) -> Side {
    let peer = Identity::generate().unwrap().fingerprint();
    let grants = Grants::new();
    let params = ChannelParams {
        local,
        peer_role: other(local),
        peer,
        negotiated: negotiated(features),
        grants: Arc::clone(&grants) as Arc<dyn GrantSource>,
        config,
    };
    let (table, closing) = watch::channel(None);
    let (control, inbound) = spawn(ws, params, closing, None);
    Side {
        control,
        inbound,
        grants,
        peer,
        table,
    }
}

/// A deck and a station control channel over one in-memory socket.
async fn pair(config: ControlConfig) -> (Side, Side) {
    let (a, b) = ws_pair(1 << 20).await;
    let deck = side(a, Role::Deck, feature::ALL, config);
    let station = side(b, Role::Station, feature::ALL, config);
    (deck, station)
}

/// One control channel with role `local`; the test drives the raw peer.
async fn with_raw(
    local: Role,
    features: &[&str],
    buffer: usize,
    config: ControlConfig,
) -> (Side, Raw) {
    let (a, b) = ws_pair(buffer).await;
    (side(a, local, features, config), b)
}

async fn send_raw(raw: &mut Raw, frame: &ControlFrame) {
    let text = serde_json::to_string(frame).unwrap();
    raw.send(Message::text(text)).await.unwrap();
}

/// The next control frame from the channel, skipping heartbeat pings.
async fn recv_raw(raw: &mut Raw) -> Option<ControlFrame> {
    loop {
        match raw.next().await? {
            Ok(Message::Text(t)) => {
                let frame: ControlFrame = serde_json::from_str(t.as_str()).unwrap();
                if !matches!(frame, ControlFrame::Ping { .. }) {
                    return Some(frame);
                }
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn inc() -> IncarnationId {
    IncarnationId::from("inc-1".to_owned())
}

fn cid(n: u32) -> CommandId {
    CommandId::from(format!("cmd-{n}"))
}

fn ack(seq: u64) -> ControlFrame {
    ControlFrame::Ack {
        incarnation: inc(),
        station_seq: seq,
    }
}

fn snapshot_end(seq: u64) -> ControlFrame {
    ControlFrame::SnapshotEnd {
        incarnation: inc(),
        watermark: seq,
    }
}

fn request(id: u64, method: &str, command_id: Option<CommandId>) -> ControlFrame {
    ControlFrame::Request {
        id,
        command_id,
        method: method.to_owned(),
        params: json!({}),
    }
}

fn err_code(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Err(WireError { code, .. }) => code,
        Outcome::Ok(_) => "ok",
    }
}

async fn next_request(side: &mut Side) -> InboundRequest {
    match side.inbound.recv().await {
        Some(Inbound::Request(r)) => r,
        other => panic!("expected a request, got {other:?}"),
    }
}

#[tokio::test]
async fn requests_go_both_ways_and_the_receiver_checks_the_scopes_it_granted_now() {
    let (mut deck, mut station) = pair(ControlConfig::default()).await;
    station
        .grants
        .grant(station.peer, [Scope::Read, Scope::Drive]);
    deck.grants.grant(deck.peer, [Scope::Coordinate]);

    // deck → station, drive scope granted.
    let call = {
        let c = deck.control.clone();
        tokio::spawn(async move {
            c.request("session.prompt", Some(&cid(1)), json!({"x": 1}))
                .await
        })
    };
    let r = next_request(&mut station).await;
    assert_eq!(
        (r.method.as_str(), r.spec.scope),
        ("session.prompt", Scope::Drive)
    );
    assert_eq!(r.command_id, Some(cid(1)));
    station
        .control
        .respond(r.id, Outcome::Ok(json!("done")))
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!("done")));
    assert!(
        station
            .control
            .respond(r.id, Outcome::Ok(json!(1)))
            .await
            .is_err(),
        "answered twice"
    );

    // The station refuses a scope it did not grant; the application sees nothing.
    let o = deck
        .control
        .request("approval.answer", Some(&cid(2)), json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "forbidden");

    // station → deck.
    let call = {
        let c = station.control.clone();
        tokio::spawn(async move { c.request("coordinator.query", None, json!({})).await })
    };
    let r = next_request(&mut deck).await;
    assert_eq!(r.method, "coordinator.query");
    deck.control
        .respond(r.id, Outcome::Ok(json!([])))
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!([])));

    // Methods of the other role are refused before they are sent.
    let o = deck
        .control
        .request("coordinator.call", Some(&cid(3)), json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "wrong_role");
    let o = station
        .control
        .request("session.prompt", Some(&cid(4)), json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "wrong_role");
    let o = deck
        .control
        .request("session.stop", None, json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "missing_command_id");

    // Stream frames in their fixed directions.
    station.control.send(snapshot_end(5)).await.unwrap();
    assert_eq!(
        deck.inbound.recv().await,
        Some(Inbound::Frame(Box::new(snapshot_end(5))))
    );
    deck.control.send(ack(5)).await.unwrap();
    assert_eq!(
        station.inbound.recv().await,
        Some(Inbound::Frame(Box::new(ack(5))))
    );
    assert!(station.control.send(ack(5)).await.is_err());
    assert!(deck.control.send(snapshot_end(5)).await.is_err());
    assert!(
        deck.control
            .send(ControlFrame::Ping { nonce: 1 })
            .await
            .is_err()
    );

    // A revocation applies to the next request.
    station.grants.revoke(&station.peer);
    let o = deck
        .control
        .request("station.info", None, json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "forbidden");
    assert!(deck.control.ended().is_none() && station.control.ended().is_none());
}

#[tokio::test]
async fn a_frame_in_the_wrong_direction_or_without_its_feature_closes_the_link() {
    let all = feature::ALL;
    let control_only = &[feature::CONTROL][..];
    let cases: Vec<(Role, &[&str], Message, &str)> = vec![
        // A station must not send an ack.
        (
            Role::Deck,
            all,
            Message::text(serde_json::to_string(&ack(1)).unwrap()),
            "wrong_role",
        ),
        // A deck must not send events or snapshots.
        (
            Role::Station,
            all,
            Message::text(serde_json::to_string(&snapshot_end(1)).unwrap()),
            "wrong_role",
        ),
        // A snapshot needs snapshots.v1.
        (
            Role::Deck,
            control_only,
            Message::text(serde_json::to_string(&snapshot_end(1)).unwrap()),
            "protocol_error",
        ),
        // A response to a request that nobody sent.
        (
            Role::Deck,
            all,
            Message::text(r#"{"type":"response","id":9,"ok":null}"#),
            "protocol_error",
        ),
        (
            Role::Deck,
            all,
            Message::binary(vec![1u8, 2, 3]),
            "protocol_error",
        ),
        (
            Role::Deck,
            all,
            Message::text("{not json"),
            "protocol_error",
        ),
    ];
    for (i, (local, features, msg, reason)) in cases.into_iter().enumerate() {
        let (s, mut raw) = with_raw(local, features, 1 << 16, ControlConfig::default()).await;
        raw.send(msg).await.unwrap();
        let end = s.control.closed().await;
        assert_eq!(end.reason, reason, "case {i}: {}", end.detail);
        match recv_raw(&mut raw).await {
            Some(ControlFrame::Close { reason: r, .. }) => assert_eq!(r, reason, "case {i}"),
            other => panic!("case {i}: expected close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_repeated_open_request_id_closes_the_link() {
    let (mut s, mut raw) = with_raw(
        Role::Station,
        feature::ALL,
        1 << 16,
        ControlConfig::default(),
    )
    .await;
    s.grants.grant(s.peer, [Scope::Read]);
    send_raw(&mut raw, &request(7, "station.info", None)).await;
    assert_eq!(next_request(&mut s).await.id, 7);
    send_raw(&mut raw, &request(7, "station.info", None)).await;
    assert_eq!(s.control.closed().await.reason, "protocol_error");
}

#[tokio::test]
async fn at_most_128_requests_are_open_in_each_direction() {
    // Own requests: the 129th is refused locally and never sent.
    let (deck, mut station) = pair(ControlConfig::default()).await;
    station.grants.grant(station.peer, [Scope::Read]);
    let mut calls = Vec::new();
    for _ in 0..MAX_OUTSTANDING {
        let c = deck.control.clone();
        calls.push(tokio::spawn(async move {
            c.request("station.info", None, json!({})).await
        }));
    }
    for _ in 0..MAX_OUTSTANDING {
        next_request(&mut station).await;
    }
    assert_eq!(deck.control.outstanding(), MAX_OUTSTANDING);
    let o = deck
        .control
        .request("station.info", None, json!({}))
        .await
        .unwrap();
    assert_eq!(err_code(&o), "too_many_requests");
    assert!(
        station.inbound.try_recv().is_err(),
        "the refused request was not sent"
    );

    // Inbound requests: the 129th open one gets too_many_requests.
    let (mut s, mut raw) = with_raw(
        Role::Station,
        feature::ALL,
        1 << 20,
        ControlConfig::default(),
    )
    .await;
    s.grants.grant(s.peer, [Scope::Read]);
    for id in 1..=129 {
        send_raw(&mut raw, &request(id, "station.info", None)).await;
    }
    match recv_raw(&mut raw).await {
        Some(ControlFrame::Response { id, outcome }) => {
            assert_eq!((id, err_code(&outcome)), (129, "too_many_requests"));
        }
        other => panic!("expected a response, got {other:?}"),
    }
    assert_eq!(s.control.open_inbound(), MAX_OUTSTANDING);
    // An answer frees a slot.
    let first = next_request(&mut s).await;
    s.control
        .respond(first.id, Outcome::Ok(json!(null)))
        .await
        .unwrap();
    assert!(matches!(
        recv_raw(&mut raw).await,
        Some(ControlFrame::Response { id: 1, .. })
    ));
    send_raw(&mut raw, &request(130, "station.info", None)).await;
    let mut seen = 1;
    while seen < 129 {
        let r = next_request(&mut s).await;
        seen += 1;
        if r.id == 130 {
            break;
        }
    }
    assert!(s.control.ended().is_none());
    drop(calls);
}

#[tokio::test(start_paused = true)]
async fn a_blackholed_peer_is_detected_in_less_than_50_seconds() {
    // The raw peer never reads and never writes.
    let (s, _raw) = with_raw(Role::Deck, feature::ALL, 1 << 16, ControlConfig::default()).await;
    let start = Instant::now();
    let end = s.control.closed().await;
    let took = start.elapsed();
    assert_eq!(end.reason, "timeout", "{}", end.detail);
    assert!(
        took >= IDLE_LIMIT && took < Duration::from_secs(50),
        "{took:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_heartbeat_keeps_an_idle_link_with_a_live_peer_open() {
    let (deck, station) = pair(ControlConfig::default()).await;
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert!(deck.control.ended().is_none(), "{:?}", deck.control.ended());
    assert!(
        station.control.ended().is_none(),
        "{:?}",
        station.control.ended()
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_application_queue_does_not_block_supersession() {
    let config = ControlConfig {
        queue: 2,
        ..ControlConfig::default()
    };
    let (s, mut raw) = with_raw(Role::Deck, feature::ALL, 1 << 20, config).await;
    // The application never reads; the station keeps sending.
    for n in 0..50 {
        send_raw(&mut raw, &snapshot_end(n)).await;
    }
    while s.inbound.len() < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let start = Instant::now();
    s.table.send_replace(Some(Closing {
        reason: "superseded".into(),
        detail: "a newer link".into(),
    }));
    let end = s.control.closed().await;
    assert_eq!(end.reason, "superseded");
    assert!(start.elapsed() <= CLEANUP_LIMIT, "{:?}", start.elapsed());
    assert!(
        matches!(recv_raw(&mut raw).await, Some(ControlFrame::Close { reason, .. }) if reason == "superseded")
    );
}

#[tokio::test(start_paused = true)]
async fn a_blocked_socket_and_a_full_send_queue_do_not_block_shutdown() {
    let config = ControlConfig {
        queue: 4,
        ..ControlConfig::default()
    };
    // A tiny socket buffer that the raw peer never drains.
    let (s, _raw) = with_raw(Role::Deck, feature::ALL, 256, config).await;
    let sender = {
        let c = s.control.clone();
        tokio::spawn(async move {
            let mut n = 0;
            while c.send(ack(n)).await.is_ok() {
                n += 1;
            }
            n
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!sender.is_finished(), "the sender waits for queue capacity");
    let start = Instant::now();
    s.control.close("shutdown", "the node stops");
    let end = s.control.closed().await;
    assert_eq!(end.reason, "shutdown");
    assert!(start.elapsed() <= CLEANUP_LIMIT, "{:?}", start.elapsed());
    let sent = sender.await.unwrap();
    assert!(sent < 100, "{sent}");
}

#[tokio::test]
async fn the_end_of_the_link_fails_open_requests_and_ends_the_inbound_queue() {
    let (deck, mut station) = pair(ControlConfig::default()).await;
    station.grants.grant(station.peer, [Scope::Read]);
    let call = {
        let c = deck.control.clone();
        tokio::spawn(async move { c.request("station.info", None, json!({})).await })
    };
    next_request(&mut station).await;
    station.control.close("bye", "the station stops");
    let err = call.await.unwrap().unwrap_err();
    assert_eq!(err.reason(), Some("bye"));
    assert_eq!(deck.control.closed().await.reason, "bye");
    let mut deck = deck;
    assert_eq!(deck.inbound.recv().await, None);
    assert!(
        deck.control
            .request("station.info", None, json!({}))
            .await
            .is_err()
    );
    // Dropping every handle closes the other side's link too.
    let (deck, station) = pair(ControlConfig::default()).await;
    drop(deck.control);
    assert_eq!(station.control.closed().await.reason, "closed");
}
