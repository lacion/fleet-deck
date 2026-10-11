//! A peer that completes the WebSocket upgrade and then stalls at a later
//! setup step (Hello, Auth, Ready, pairing proofs) must fail with the reason
//! `timeout` within `NodeConfig::handshake_timeout` on the real side. The real
//! side also tells the stalled peer why: it sends `close` with reason
//! `timeout` (bounded, so a peer that reads nothing cannot block it either).
//!
//! The stalled peers here are scripted: they speak the real handshake with the
//! public `hello` functions and stop at a chosen step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use s4_link::hello::{self, Channel, EXPORTER_LABEL, HandshakeFrame, LocalHello, Side};
use s4_link::identity::{Identity, PeerKey};
use s4_link::link::{self, LinkError, NodeConfig};
use s4_link::net;
use s4_link::pairing::Invitation;
use s4_link::secret::Secret;
use s4_link::tls::{self, PeerPolicy, Role, SERVER_NAME};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async, client_async};

const LIMIT: Duration = Duration::from_millis(300);
/// Far below the 10 s default: a missing timeout fails the test.
const FAST: Duration = Duration::from_secs(3);

fn node(role: Role) -> NodeConfig {
    let mut n = NodeConfig::new(Arc::new(Identity::generate().unwrap()), role);
    n.handshake_timeout = LIMIT;
    n
}

/// Where a scripted peer stops answering.
#[derive(Clone, Copy, PartialEq)]
enum StopAt {
    /// After the WebSocket upgrade: it never sends `hello`.
    Hello,
    /// After both `auth` frames: it never sends `ready` / `pair_proof` /
    /// `paired`.
    AfterAuth,
}

/// The frames that a scripted peer read after it stalled, as `(type, reason)`.
type Seen = oneshot::Receiver<Vec<(String, String)>>;

async fn read_text<S>(ws: &mut WebSocketStream<S>) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => return t.to_string(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn send<S>(ws: &mut WebSocketStream<S>, frame: &HandshakeFrame) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(frame).unwrap();
    ws.send(Message::text(text.clone())).await.unwrap();
    text
}

/// Runs the real handshake as `side` up to `stop`, then reads (and never
/// answers) until the socket ends or `FAST` passes; reports what it read.
#[allow(clippy::too_many_arguments)]
async fn script<S>(
    mut ws: WebSocketStream<S>,
    identity: &Identity,
    role: Role,
    side: Side,
    channel: Channel,
    exporter: [u8; 32],
    peer: PeerKey,
    stop: StopAt,
    seen: oneshot::Sender<Vec<(String, String)>>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if stop == StopAt::AfterAuth {
        let local = LocalHello::new(role);
        let epoch = if side == Side::Dialer { 1 } else { 0 };
        let own = local.hello(identity, epoch, hello::new_nonce().unwrap(), channel);
        let own_text = send(&mut ws, &HandshakeFrame::Hello(own)).await;
        let peer_text = read_text(&mut ws).await;
        let own_key = identity.public_key();
        let tr = match side {
            Side::Dialer => hello::transcript(&exporter, &own_text, &peer_text, &own_key, &peer),
            Side::Listener => hello::transcript(&exporter, &peer_text, &own_text, &peer, &own_key),
        };
        send(&mut ws, &hello::sign(identity, &tr, side)).await;
        let _auth = read_text(&mut ws).await;
    }
    let mut frames = Vec::new();
    let deadline = tokio::time::sleep(FAST);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            m = ws.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    frames.push((
                        v["type"].as_str().unwrap_or("").to_owned(),
                        v["reason"].as_str().unwrap_or("").to_owned(),
                    ));
                }
                Some(Ok(_)) => {}
                _ => break,
            },
            () = &mut deadline => break,
        }
    }
    let _ = seen.send(frames);
}

/// A listener (any client key) that upgrades `channel` and then stalls.
async fn stalled_listener(
    identity: Arc<Identity>,
    role: Role,
    channel: Channel,
    stop: StopAt,
) -> (SocketAddr, Seen) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let config = tls::server_config(&identity, PeerPolicy::AnyKey).unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (sock, from) = tcp.accept().await.unwrap();
        let (conn, peer) = net::handshake_accepted(sock, from, config).await.unwrap();
        let mut exporter = [0u8; 32];
        conn.get_ref()
            .1
            .export_keying_material(&mut exporter, EXPORTER_LABEL, None)
            .unwrap();
        let ws = accept_async(conn).await.unwrap();
        script(
            ws,
            &identity,
            role,
            Side::Listener,
            channel,
            exporter,
            peer,
            stop,
            tx,
        )
        .await;
    });
    (addr, rx)
}

/// A dialer that upgrades `channel` at `addr` and then stalls.
async fn stalled_dialer(
    identity: Arc<Identity>,
    role: Role,
    addr: SocketAddr,
    listener: &Identity,
    channel: Channel,
    stop: StopAt,
) -> Seen {
    let config =
        tls::client_config(&identity, PeerPolicy::Fingerprint(listener.fingerprint())).unwrap();
    let (conn, peer) = net::connect(addr, config).await.unwrap();
    let mut exporter = [0u8; 32];
    conn.get_ref()
        .1
        .export_keying_material(&mut exporter, EXPORTER_LABEL, None)
        .unwrap();
    let url = format!("wss://{SERVER_NAME}{}", channel.path());
    let (ws, _) = client_async(url, conn).await.unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        script(
            ws,
            &identity,
            role,
            Side::Dialer,
            channel,
            exporter,
            peer,
            stop,
            tx,
        )
        .await;
    });
    rx
}

fn assert_timeout(e: &LinkError, step: &str, started: Instant) {
    assert_eq!(e.reason(), Some("timeout"), "{e}");
    assert!(e.to_string().contains(step), "{e}");
    assert!(started.elapsed() < FAST, "took {:?}", started.elapsed());
}

async fn told_timeout(seen: Seen) {
    let frames = seen.await.unwrap();
    assert!(
        frames.contains(&("close".to_owned(), "timeout".to_owned())),
        "the stalled peer was not told: {frames:?}"
    );
}

fn invitation_to(addr: SocketAddr, inviter: &Identity) -> Invitation {
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    Invitation {
        addr,
        fingerprint: inviter.fingerprint(),
        secret: Secret::new([9u8; 16]),
        expiry,
    }
}

#[tokio::test]
async fn a_listener_that_never_sends_hello_times_out_the_dial() {
    let deck = Arc::new(Identity::generate().unwrap());
    let (addr, seen) =
        stalled_listener(deck.clone(), Role::Deck, Channel::Control, StopAt::Hello).await;
    let station = node(Role::Station);
    station.pins.pin(deck.public_key(), Role::Deck);
    let t = Instant::now();
    let e = link::dial(&station, addr, 1).await.unwrap_err();
    assert_timeout(&e, "hello/auth exchange", t);
    told_timeout(seen).await;
}

#[tokio::test]
async fn a_listener_that_never_sends_ready_times_out_the_dial() {
    let deck = Arc::new(Identity::generate().unwrap());
    let (addr, seen) = stalled_listener(
        deck.clone(),
        Role::Deck,
        Channel::Control,
        StopAt::AfterAuth,
    )
    .await;
    let station = node(Role::Station);
    station.pins.pin(deck.public_key(), Role::Deck);
    let t = Instant::now();
    let e = link::dial(&station, addr, 1).await.unwrap_err();
    assert_timeout(&e, "wait for ready", t);
    told_timeout(seen).await;
}

#[tokio::test]
async fn a_dialer_that_never_sends_hello_times_out_on_the_listener() {
    let deck = node(Role::Deck);
    let stall = Arc::new(Identity::generate().unwrap());
    deck.pins.pin(stall.public_key(), Role::Station);
    let mut listener = link::listen(deck.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let t = Instant::now();
    let seen = stalled_dialer(
        stall,
        Role::Station,
        listener.local_addr,
        &deck.identity,
        Channel::Control,
        StopAt::Hello,
    )
    .await;
    let e = tokio::time::timeout(FAST, listener.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_timeout(&e, "hello/auth exchange", t);
    told_timeout(seen).await;
}

#[tokio::test]
async fn an_inviter_that_never_answers_the_proof_times_out_the_joiner() {
    let deck = Arc::new(Identity::generate().unwrap());
    let (addr, seen) =
        stalled_listener(deck.clone(), Role::Deck, Channel::Pair, StopAt::AfterAuth).await;
    let station = node(Role::Station);
    let t = Instant::now();
    let e = link::pair(&station, &invitation_to(addr, &deck), [])
        .await
        .unwrap_err();
    assert_timeout(&e, "pairing proof exchange", t);
    assert!(station.pins.is_empty(), "the joiner pins nothing");
    let frames = seen.await.unwrap();
    assert_eq!(frames.first().map(|f| f.0.as_str()), Some("pair_proof"));
    assert!(
        frames.contains(&("close".to_owned(), "timeout".to_owned())),
        "{frames:?}"
    );
}

#[tokio::test]
async fn a_joiner_that_never_sends_its_proof_times_out_and_keeps_the_invitation() {
    let deck = node(Role::Deck);
    let listener = link::listen(deck.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    deck.invites
        .create(
            listener.local_addr,
            deck.identity.fingerprint(),
            Duration::from_secs(600),
            [],
        )
        .unwrap();
    let stall = Arc::new(Identity::generate().unwrap());
    let t = Instant::now();
    let seen = stalled_dialer(
        stall.clone(),
        Role::Station,
        listener.local_addr,
        &deck.identity,
        Channel::Pair,
        StopAt::AfterAuth,
    )
    .await;
    told_timeout(seen).await;
    assert!(t.elapsed() < FAST + LIMIT, "took {:?}", t.elapsed());
    assert_eq!(deck.invites.open_count(), 1, "the invitation stays open");
    assert!(deck.pins.is_empty(), "the stalled joiner is not pinned");
}
