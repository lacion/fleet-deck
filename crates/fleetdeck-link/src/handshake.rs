//! Runs the §8.2 handshake on one WebSocket channel.
//!
//! Order on both sides: send `Hello`, read the peer `Hello`, negotiate, send
//! `Auth`, read and verify the peer `Auth`. The listener then decides about
//! the link (epoch, link table) and sends `Ready` or `Close`; the dialer waits
//! for it. Any failure sends a `Close { reason, detail }` first, so both sides
//! report the same clear reason.

use std::time::Duration;

use fleetdeck_core::identity::{Identity, PeerKey};

use crate::hello::{
    self, Attempt, Channel, HandshakeError, HandshakeFrame, Hello, LocalHello, Negotiated, Side,
    negotiate,
};
use crate::ws::{self, LinkError, STEP_HELLO, STEP_READY, Ws};

/// A channel whose `Hello`/`Auth` exchange succeeded.
#[derive(Debug)]
pub struct Established {
    pub peer_hello: Hello,
    pub negotiated: Negotiated,
    /// The signed transcript; pairing proofs are made over it.
    pub transcript: Vec<u8>,
}

/// What one side brings to a channel handshake.
pub(crate) struct Own<'a> {
    pub identity: &'a Identity,
    pub local: &'a LocalHello,
    /// `Some` on the dialer of a control or data channel.
    pub attempt: Option<&'a Attempt>,
    pub limit: Duration,
}

/// Hello + Auth on one channel, bounded by `own.limit`.
pub(crate) async fn run(
    ws: &mut Ws,
    own: &Own<'_>,
    side: Side,
    channel: Channel,
    exporter: [u8; 32],
    peer_key: PeerKey,
) -> Result<Established, LinkError> {
    match tokio::time::timeout(own.limit, steps(ws, own, side, channel, exporter, peer_key)).await {
        Ok(r) => r,
        Err(_) => Err(ws::timed_out(ws, STEP_HELLO, own.limit).await),
    }
}

async fn steps(
    ws: &mut Ws,
    own: &Own<'_>,
    side: Side,
    channel: Channel,
    exporter: [u8; 32],
    peer_key: PeerKey,
) -> Result<Established, LinkError> {
    let nonce = hello::new_nonce().map_err(LinkError::Handshake)?;
    let mine = match (side, own.attempt) {
        (Side::Dialer, Some(a)) => own.local.dialer_hello(own.identity, a, nonce, channel),
        _ => own.local.listener_hello(own.identity, nonce, channel),
    };
    let own_text = ws::send_frame(ws, &HandshakeFrame::Hello(mine.clone())).await?;
    let (peer_text, frame) = ws::recv_frame(ws).await?;
    let HandshakeFrame::Hello(peer_hello) = frame else {
        return Err(ws::refuse(ws, HandshakeError::Protocol("expected Hello".into())).await);
    };
    let negotiated = match negotiate(
        own.local,
        &mine,
        &peer_hello,
        &peer_key,
        channel,
        side.other(),
    ) {
        Ok(n) => n,
        Err(e) => return Err(ws::refuse(ws, e).await),
    };
    let own_key = own.identity.public_key();
    let tr = match side {
        Side::Dialer => hello::transcript(&exporter, &own_text, &peer_text, &own_key, &peer_key),
        Side::Listener => hello::transcript(&exporter, &peer_text, &own_text, &peer_key, &own_key),
    };
    ws::send_frame(ws, &hello::sign(own.identity, &tr, side)).await?;
    let (_, frame) = ws::recv_frame(ws).await?;
    let HandshakeFrame::Auth { signature } = frame else {
        return Err(ws::refuse(ws, HandshakeError::Protocol("expected Auth".into())).await);
    };
    if let Err(e) = hello::verify(&peer_key, &tr, side.other(), &signature) {
        return Err(ws::refuse(ws, e).await);
    }
    Ok(Established {
        peer_hello,
        negotiated,
        transcript: tr,
    })
}

/// The dialer waits for the listener's `Ready` (or its `Close`).
pub(crate) async fn await_ready(ws: &mut Ws, limit: Duration) -> Result<(), LinkError> {
    let wait = async {
        match ws::recv_frame(ws).await? {
            (_, HandshakeFrame::Ready) => Ok(()),
            _ => Err(LinkError::Protocol("expected Ready".into())),
        }
    };
    match tokio::time::timeout(limit, wait).await {
        Ok(r) => r,
        Err(_) => Err(ws::timed_out(ws, STEP_READY, limit).await),
    }
}
