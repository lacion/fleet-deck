//! The wire rules of a tailnet link listener (DL-178, tailnet design 5.4
//! and 6): the `Origin` and `Host` rule, the unauthenticated permits with
//! their stage deadlines, the message limit and the heartbeat. A raw client
//! speaks the protocol by hand, so a test can stall or misbehave.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::{Fingerprint, Identity, PeerKey};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::Scope;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpSocket, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{client_async_with_config, tungstenite};

use crate::epochs::Epochs;
use crate::handshake;
use crate::hello::{Channel, HandshakeFrame, LocalHello, Side, feature};
use crate::net;
use crate::node::{self, ListenerKind, NodeConfig, TailnetLimits};
use crate::pair_hub::{
    CommitError, Completion, HubConfig, PairApprover, PairRequests,
};
use crate::pairing::{Invitations, PairLimiter};
use crate::tls::{self, PeerPolicy, Role};
use crate::ws::{self, Ws};

const T: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Nothing;

#[async_trait::async_trait]
impl PairApprover for Nothing {
    async fn commit(&self, _: &Completion) -> Result<(), CommitError> {
        Ok(())
    }
    async fn completion(&self, _: &str) -> Result<Option<Completion>, String> {
        Ok(None)
    }
}

fn node(role: Role) -> NodeConfig {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    NodeConfig::new(
        identity,
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    )
}

struct Deck {
    config: NodeConfig,
    hub: Arc<PairRequests>,
    listener: node::LinkListener,
}

impl Deck {
    fn fp(&self) -> Fingerprint {
        self.config.identity.fingerprint()
    }
}

/// A tailnet-kind deck on loopback with an open window and short stage
/// deadlines.
async fn deck(limits: TailnetLimits, cfg: HubConfig) -> Deck {
    let mut config = node(Role::Deck);
    let hub = PairRequests::new(
        config.identity.public_key(),
        [Scope::Admin].into(),
        config.pins.clone(),
        Arc::new(Nothing),
        cfg,
    );
    config = config
        .with_invitations(Invitations::new())
        .with_pair_requests(Arc::clone(&hub));
    config.listener_kind = Some(ListenerKind::Tailnet);
    config.tailnet = limits;
    config.pair_limiter = PairLimiter::default();
    let listener = node::listen(config.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    hub.open_window().await.unwrap();
    Deck {
        config,
        hub,
        listener,
    }
}

fn quick_limits() -> TailnetLimits {
    TailnetLimits {
        tls: Duration::from_millis(300),
        upgrade: Duration::from_millis(300),
        hello: Duration::from_millis(300),
        max_message: 16 * 1024,
    }
}

/// A raw client: TLS, then a WebSocket upgrade with the headers of the
/// caller.
async fn upgrade(
    addr: SocketAddr,
    identity: &Identity,
    deck: Fingerprint,
    path: &str,
    host: &str,
    origin: Option<&str>,
) -> Result<(Ws, PeerKey, [u8; 32]), tungstenite::Error> {
    let config = tls::client_config(identity, PeerPolicy::Fingerprint(deck)).unwrap();
    let (tls, peer) = net::connect(addr, config).await.unwrap();
    let exporter = ws::exporter_of(tls.get_ref().1).unwrap();
    let mut request = format!("wss://{host}{path}").into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_str(origin).unwrap());
    }
    let (ws, _) = client_async_with_config(
        request,
        tokio_rustls::TlsStream::Client(tls),
        Some(ws::ws_config(Channel::Pair)),
    )
    .await?;
    Ok((ws, peer, exporter))
}

async fn pair_handshake(
    deck: &Deck,
    station: &NodeConfig,
    local: Option<LocalHello>,
) -> (Ws, crate::Established) {
    let (mut ws, peer, exporter) = upgrade(
        deck.listener.local_addr,
        &station.identity,
        deck.fp(),
        Channel::Pair.path(),
        tls::SERVER_NAME,
        None,
    )
    .await
    .unwrap();
    let mut config = station.clone();
    if let Some(local) = local {
        config.local = local;
    }
    let est = handshake::run(
        &mut ws,
        &config.own(None),
        Side::Dialer,
        Channel::Pair,
        exporter,
        peer,
    )
    .await
    .unwrap();
    (ws, est)
}

fn request_frame(name: &str) -> HandshakeFrame {
    let req = crate::pair_request::CanonicalRequest::generate(
        name,
        BTreeSet::from([Scope::Read]),
    )
    .unwrap();
    HandshakeFrame::PairRequest {
        request_id: req.id_hex(),
        name: req.name.clone(),
        nonce: req.nonce_hex(),
        offered_scopes: req.scopes_vec(),
    }
}

/// The reason of the `Close` frame the deck sends next.
async fn next_close(ws: &mut Ws) -> String {
    match tokio::time::timeout(T, ws::recv_frame(ws)).await {
        Ok(Err(e)) => e.reason().unwrap_or("none").to_owned(),
        other => panic!("expected a close, got {other:?}"),
    }
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A TCP connection to `to` from the loopback address `127.0.0.<last>`.
async fn tcp(to: SocketAddr, last: u8) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket
        .bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)),
            0,
        ))
        .unwrap();
    socket.connect(to).await.unwrap()
}

/// True when the listener closes `s` within `within`.
async fn closed_within(s: &mut TcpStream, within: Duration) -> bool {
    let mut buf = [0u8; 16];
    matches!(
        tokio::time::timeout(within, s.read(&mut buf)).await,
        Ok(Ok(0) | Err(_))
    )
}

#[tokio::test]
async fn a_request_with_an_origin_header_is_refused_with_403() {
    for kind in [ListenerKind::Tailnet, ListenerKind::Other] {
        let mut config = node(Role::Deck);
        config.listener_kind = Some(kind);
        let l = node::listen(config.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let station = Identity::generate().unwrap();
        config.pins.pin(station.public_key(), Role::Station);
        for channel in [Channel::Control, Channel::Data, Channel::Pair] {
            let r = upgrade(
                l.local_addr,
                &station,
                config.identity.fingerprint(),
                channel.path(),
                tls::SERVER_NAME,
                Some("https://evil.example"),
            )
            .await;
            match r {
                Err(tungstenite::Error::Http(resp)) => {
                    assert_eq!(resp.status(), 403, "{kind:?} {channel:?}");
                }
                other => panic!("{kind:?} {channel:?}: expected 403, got {:?}", other.err()),
            }
        }
    }
}

#[tokio::test]
async fn a_request_with_another_host_is_refused_with_403() {
    let deck = deck(TailnetLimits::default(), HubConfig::default()).await;
    let station = Identity::generate().unwrap();
    deck.config.pins.pin(station.public_key(), Role::Station);
    for host in ["evil.example", "127.0.0.1", "node.fleet-deck.invalid.evil.example"] {
        let r = upgrade(
            deck.listener.local_addr,
            &station,
            deck.fp(),
            Channel::Control.path(),
            host,
            None,
        )
        .await;
        match r {
            Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 403, "{host}"),
            other => panic!("{host}: expected 403, got {:?}", other.err()),
        }
    }
    // The host of the protocol's own dialer is accepted.
    assert!(
        upgrade(
            deck.listener.local_addr,
            &station,
            deck.fp(),
            Channel::Control.path(),
            tls::SERVER_NAME,
            None,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn a_deck_that_was_not_negotiated_with_the_feature_closes_a_request_with_unsupported() {
    let deck = deck(TailnetLimits::default(), HubConfig::default()).await;
    let station = node(Role::Station);
    let mut local = station.local.clone();
    local.features.retain(|f| f != feature::PAIR_REQUEST);
    let (mut ws, est) = pair_handshake(&deck, &station, Some(local)).await;
    assert!(!est.negotiated.has(feature::PAIR_REQUEST));
    ws::send_frame(&mut ws, &request_frame("a")).await.unwrap();
    assert_eq!(next_close(&mut ws).await, "unsupported");
    assert!(deck.hub.list().await.is_empty());
}

#[tokio::test]
async fn a_request_with_bad_fields_is_a_protocol_error() {
    let deck = deck(TailnetLimits::default(), HubConfig::default()).await;
    let station = node(Role::Station);
    let good = request_frame("a");
    let HandshakeFrame::PairRequest {
        request_id,
        nonce,
        offered_scopes,
        ..
    } = good
    else {
        unreachable!()
    };
    for (id, name, nonce, scopes) in [
        ("zz".to_owned(), "a".to_owned(), nonce.clone(), offered_scopes.clone()),
        (request_id.clone(), "a".to_owned(), "00".into(), offered_scopes.clone()),
        (request_id.clone(), String::new(), nonce.clone(), offered_scopes.clone()),
        (request_id.clone(), "x".repeat(65), nonce.clone(), offered_scopes.clone()),
        (request_id.clone(), "a\u{202E}b".to_owned(), nonce.clone(), offered_scopes.clone()),
        (request_id.clone(), "a".to_owned(), nonce.clone(), vec![Scope::Coordinate]),
    ] {
        let (mut ws, _) = pair_handshake(&deck, &station, None).await;
        let frame = HandshakeFrame::PairRequest {
            request_id: id,
            name,
            nonce,
            offered_scopes: scopes,
        };
        ws::send_frame(&mut ws, &frame).await.unwrap();
        assert_eq!(next_close(&mut ws).await, "protocol_error");
    }
    assert!(deck.hub.list().await.is_empty());
}

#[tokio::test]
async fn an_unpinned_key_cannot_send_a_message_above_16_kib() {
    let deck = deck(TailnetLimits::default(), HubConfig::default()).await;
    let station = node(Role::Station);
    let (mut ws, _) = pair_handshake(&deck, &station, None).await;
    let big = "x".repeat(20 * 1024);
    // The send may succeed; the deck closes the connection at the limit.
    let _ = ws.send(Message::text(big)).await;
    let ended = tokio::time::timeout(T, async {
        loop {
            match ws.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true));
    assert!(deck.hub.list().await.is_empty());
    eventually("the permit is released", || deck.listener.admitted() == 0).await;
}

#[tokio::test]
async fn a_waiting_connection_that_sends_no_pong_is_closed_after_the_deadline() {
    let cfg = HubConfig {
        ping_every: Duration::from_millis(50),
        dead_after: Duration::from_millis(200),
        ..HubConfig::default()
    };
    let deck = deck(TailnetLimits::default(), cfg).await;
    let station = node(Role::Station);
    let (mut ws, _) = pair_handshake(&deck, &station, None).await;
    ws::send_frame(&mut ws, &request_frame("a")).await.unwrap();
    // The client reads the PairPending and then stops reading: no pong.
    let (_, frame) = ws::recv_frame(&mut ws).await.unwrap();
    assert!(matches!(frame, HandshakeFrame::PairPending { .. }));
    eventually("the request waits", || deck.listener.admitted() == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(deck.hub.list().await.len(), 1);
    eventually("the silent connection is closed", || {
        deck.listener.admitted() == 0
    })
    .await;
    assert!(deck.hub.list().await.is_empty());
}

#[tokio::test]
async fn a_connection_that_answers_the_pings_keeps_its_request() {
    let cfg = HubConfig {
        ping_every: Duration::from_millis(50),
        dead_after: Duration::from_millis(200),
        ..HubConfig::default()
    };
    let deck = deck(TailnetLimits::default(), cfg).await;
    let station = node(Role::Station);
    let (mut ws, _) = pair_handshake(&deck, &station, None).await;
    ws::send_frame(&mut ws, &request_frame("a")).await.unwrap();
    let (_, frame) = ws::recv_frame(&mut ws).await.unwrap();
    assert!(matches!(frame, HandshakeFrame::PairPending { .. }));
    // Reading answers each ping with a pong for 600 ms (3 dead times).
    let _ = tokio::time::timeout(Duration::from_millis(600), ws::recv_frame(&mut ws)).await;
    assert_eq!(deck.hub.list().await.len(), 1);
}

#[tokio::test]
async fn a_connection_that_stalls_in_tls_is_closed_at_the_stage_deadline() {
    let deck = deck(quick_limits(), HubConfig::default()).await;
    let mut s = tcp(deck.listener.local_addr, 1).await;
    eventually("the permit", || deck.listener.admitted() == 1).await;
    assert_eq!(deck.listener.in_setup(), 1);
    assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    eventually("the permit is released", || deck.listener.admitted() == 0).await;
    assert_eq!(deck.listener.in_setup(), 0);
}

#[tokio::test]
async fn the_65th_connection_and_the_33rd_in_setup_are_refused_at_accept() {
    let limits = TailnetLimits {
        tls: Duration::from_secs(30),
        upgrade: Duration::from_secs(30),
        hello: Duration::from_secs(30),
        max_message: 16 * 1024,
    };
    let deck = deck(limits, HubConfig::default()).await;
    let addr = deck.listener.local_addr;
    // 32 stalled connections fill the TLS and upgrade stage of the listener.
    let mut held = Vec::new();
    for source in 1..=2u8 {
        for _ in 0..16 {
            held.push(tcp(addr, source).await);
        }
    }
    eventually("32 in setup", || deck.listener.in_setup() == 32).await;
    let mut more = tcp(addr, 3).await;
    assert!(closed_within(&mut more, Duration::from_secs(3)).await);
    assert_eq!(deck.listener.in_setup(), 32);
    drop(held);
    eventually("released", || deck.listener.admitted() == 0).await;
}

#[tokio::test]
async fn an_unknown_key_on_the_control_or_data_path_is_refused_and_its_permit_released() {
    let deck = deck(quick_limits(), HubConfig::default()).await;
    let station = Identity::generate().unwrap();
    for channel in [Channel::Control, Channel::Data] {
        // The window is open, so TLS lets the key in; the upgrade works, and
        // the client then stalls without a Hello.
        let (mut ws, _, _) = upgrade(
            deck.listener.local_addr,
            &station,
            deck.fp(),
            channel.path(),
            tls::SERVER_NAME,
            None,
        )
        .await
        .unwrap();
        eventually("the permit", || deck.listener.admitted() == 1).await;
        let ended = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_) | Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "{channel:?}: the stalled connection was closed");
        eventually("the permit is released", || deck.listener.admitted() == 0).await;
    }
}

#[tokio::test]
async fn a_stalled_hello_after_a_busy_refusal_still_ends_at_the_deadline() {
    let deck = deck(quick_limits(), HubConfig::default()).await;
    // Four connections hold the 4 handshake slots (no Hello from them).
    let mut holders = Vec::new();
    for _ in 0..4 {
        let station = Identity::generate().unwrap();
        let (ws, _, _) = upgrade(
            deck.listener.local_addr,
            &station,
            deck.fp(),
            Channel::Pair.path(),
            tls::SERVER_NAME,
            None,
        )
        .await
        .unwrap();
        holders.push(ws);
    }
    // The 5th is busy; it stalls too, and the deck must not wait for ever.
    let station = Identity::generate().unwrap();
    let (mut fifth, _, _) = upgrade(
        deck.listener.local_addr,
        &station,
        deck.fp(),
        Channel::Pair.path(),
        tls::SERVER_NAME,
        None,
    )
    .await
    .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match fifth.next().await {
                None | Some(Err(_)) => return,
                Some(Ok(Message::Close(_))) => return,
                Some(Ok(Message::Text(_))) => return, // the Close frame
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok());
    drop(holders);
    eventually("all permits released", || deck.listener.admitted() == 0).await;
}

/// A WebSocket upgrade from the loopback address `127.0.0.<source>`; the
/// client then sends nothing.
async fn stalled_upgrade(
    deck: &Deck,
    source: u8,
    path: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_rustls::TlsStream<TcpStream>> {
    let station = Identity::generate().unwrap();
    let config = tls::client_config(&station, PeerPolicy::Fingerprint(deck.fp())).unwrap();
    let name = rustls::pki_types::ServerName::try_from(tls::SERVER_NAME).unwrap();
    let stream = tcp(deck.listener.local_addr, source).await;
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, stream)
        .await
        .unwrap();
    let request = format!("wss://{}{path}", tls::SERVER_NAME)
        .into_client_request()
        .unwrap();
    let (ws, _) = client_async_with_config(
        request,
        tokio_rustls::TlsStream::Client(tls),
        Some(ws::ws_config(Channel::Pair)),
    )
    .await
    .unwrap();
    ws
}

#[tokio::test]
async fn sixty_four_permits_stay_taken_until_the_sockets_close_and_the_65th_is_refused() {
    let limits = TailnetLimits {
        tls: Duration::from_secs(30),
        upgrade: Duration::from_secs(30),
        hello: Duration::from_secs(30),
        max_message: 16 * 1024,
    };
    let deck = deck(limits, HubConfig::default()).await;
    let mut held = Vec::new();
    for source in 1..=4u8 {
        for _ in 0..16 {
            held.push(stalled_upgrade(&deck, source, Channel::Control.path()).await);
        }
    }
    eventually("64 permits", || deck.listener.admitted() == 64).await;
    // The upgrade is done: the setup places are free, the permits are not.
    assert_eq!(deck.listener.in_setup(), 0);
    let mut last = tcp(deck.listener.local_addr, 5).await;
    assert!(closed_within(&mut last, Duration::from_secs(3)).await);
    assert_eq!(deck.listener.admitted(), 64);
    drop(held);
    eventually("the permits are released", || deck.listener.admitted() == 0).await;
}
