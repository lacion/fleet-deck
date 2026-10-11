//! Runs the §8.2 handshake on one WebSocket channel.
//!
//! Order on both sides: send `Hello`, read the peer `Hello`, negotiate, send
//! `Auth`, read and verify the peer `Auth`. The listener then decides about
//! the link epoch and sends `Ready` or `Close`; the dialer waits for it. Any
//! failure sends a `Close { reason, detail }` first, so both sides report the
//! same clear reason.

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::{LinkError, NodeConfig, Ws};
use crate::hello::{
    self, Channel, HandshakeError, HandshakeFrame, Hello, Negotiated, Side, negotiate,
};
use crate::identity::PeerKey;
use crate::net::HANDSHAKE_TIMEOUT;

pub(crate) struct Established {
    pub peer_hello: Hello,
    pub negotiated: Negotiated,
}

pub(crate) async fn send_frame(ws: &mut Ws, frame: &HandshakeFrame) -> Result<String, LinkError> {
    let text = serde_json::to_string(frame).map_err(|e| LinkError::Protocol(e.to_string()))?;
    ws.send(Message::text(text.clone()))
        .await
        .map_err(LinkError::ws)?;
    Ok(text)
}

/// The next text message; WebSocket ping/pong is skipped.
pub(crate) async fn recv_text(ws: &mut Ws) -> Result<String, LinkError> {
    loop {
        match ws.next().await {
            None => return Err(LinkError::closed("eof", "the peer closed the connection")),
            Some(Err(e)) => return Err(LinkError::ws(e)),
            Some(Ok(Message::Text(t))) => return Ok(t.as_str().to_owned()),
            Some(Ok(Message::Close(_))) => {
                return Err(LinkError::closed("eof", "the peer closed the WebSocket"));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                return Err(LinkError::Protocol(
                    "binary message during the handshake".into(),
                ));
            }
        }
    }
}

async fn recv_frame(ws: &mut Ws) -> Result<(String, HandshakeFrame), LinkError> {
    let text = recv_text(ws).await?;
    let frame = serde_json::from_str(&text)
        .map_err(|e| LinkError::Handshake(HandshakeError::BadHello(e.to_string())))?;
    match frame {
        HandshakeFrame::Close { reason, detail } => {
            Err(LinkError::Handshake(HandshakeError::PeerClosed {
                reason,
                detail,
            }))
        }
        f => Ok((text, f)),
    }
}

/// Sends the `Close` frame for `e` (best effort) and returns `e`.
pub(crate) async fn refuse(ws: &mut Ws, e: HandshakeError) -> LinkError {
    let _ = send_frame(ws, &e.close_frame()).await;
    let _ = ws.close(None).await;
    LinkError::Handshake(e)
}

/// Hello + Auth on one channel, bounded by [`HANDSHAKE_TIMEOUT`].
pub(crate) async fn run(
    ws: &mut Ws,
    node: &NodeConfig,
    side: Side,
    channel: Channel,
    epoch: u64,
    exporter: [u8; 32],
    peer_key: PeerKey,
) -> Result<Established, LinkError> {
    match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        steps(ws, node, side, channel, epoch, exporter, peer_key),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(refuse(ws, HandshakeError::Protocol("handshake timeout".into())).await),
    }
}

async fn steps(
    ws: &mut Ws,
    node: &NodeConfig,
    side: Side,
    channel: Channel,
    epoch: u64,
    exporter: [u8; 32],
    peer_key: PeerKey,
) -> Result<Established, LinkError> {
    let nonce = hello::new_nonce().map_err(LinkError::Handshake)?;
    let own = node.local.hello(&node.identity, epoch, nonce, channel);
    let own_text = send_frame(ws, &HandshakeFrame::Hello(own.clone())).await?;
    let (peer_text, frame) = recv_frame(ws).await?;
    let HandshakeFrame::Hello(peer_hello) = frame else {
        return Err(refuse(ws, HandshakeError::Protocol("expected Hello".into())).await);
    };
    let negotiated = match negotiate(&node.local, &own, &peer_hello, &peer_key, channel) {
        Ok(n) => n,
        Err(e) => return Err(refuse(ws, e).await),
    };
    let own_key = node.identity.public_key();
    let tr = match side {
        Side::Dialer => hello::transcript(&exporter, &own_text, &peer_text, &own_key, &peer_key),
        Side::Listener => hello::transcript(&exporter, &peer_text, &own_text, &peer_key, &own_key),
    };
    send_frame(ws, &hello::sign(&node.identity, &tr, side)).await?;
    let (_, frame) = recv_frame(ws).await?;
    let HandshakeFrame::Auth { signature } = frame else {
        return Err(refuse(ws, HandshakeError::Protocol("expected Auth".into())).await);
    };
    if let Err(e) = hello::verify(&peer_key, &tr, side.other(), &signature) {
        return Err(refuse(ws, e).await);
    }
    Ok(Established {
        peer_hello,
        negotiated,
    })
}

/// The dialer waits for the listener's `Ready` (or its `Close`).
pub(crate) async fn await_ready(ws: &mut Ws) -> Result<(), LinkError> {
    let wait = async {
        match recv_frame(ws).await? {
            (_, HandshakeFrame::Ready) => Ok(()),
            _ => Err(LinkError::Protocol("expected Ready".into())),
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, wait)
        .await
        .map_err(|_| LinkError::Protocol("no Ready from the listener".into()))?
}
