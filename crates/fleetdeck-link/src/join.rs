//! The pair channel (PLAN §8.5, ADR-004 §2.8 with the G11 changes of §9.7).
//!
//! The joiner dials `/fdlink/v1/pair` of the inviter and trusts only the
//! fingerprint in the invitation. Both sides run the normal handshake (both
//! keys proven, transcript bound to the TLS session) with epoch 0. The joiner
//! then sends `PairProof { invitation, proof }`. The inviter redeems that one
//! invitation, stores the joiner's grants, pins the joiner and answers
//! `Paired { proof }`. The joiner pins the inviter only after it checked that
//! answer. The pair connection then closes; the dialer opens the link.
//!
//! The inviter admits each pairing attempt through its [`PairLimiter`]
//! (10 attempts per source IP and minute, 4 handshakes at a time; G11).
//!
//! [`PairLimiter`]: crate::pairing::PairLimiter

use std::collections::BTreeSet;
use std::net::IpAddr;

use fleetdeck_core::identity::PeerKey;
use fleetdeck_proto::link::Scope;

use crate::handshake;
use crate::hello::{Channel, HandshakeError, HandshakeFrame, Side};
use crate::node::NodeConfig;
use crate::pairing::{Invitation, InvitationId, PairError, PairedPeer, reason};
use crate::tls::{self, PeerPolicy};
use crate::ws::{self, LinkError, Ws};

/// The step name of a pairing proof that did not arrive in time.
pub const STEP_PROOF: &str = "pairing proof";

/// Joins the node that issued `invitation`. On success the inviter's key is
/// pinned with its role, and `node.sink` stores `scopes` for it. The caller
/// then dials the inviter with [`crate::dial`] or waits for its dial.
pub async fn pair(
    node: &NodeConfig,
    invitation: &Invitation,
    scopes: impl IntoIterator<Item = Scope>,
) -> Result<PairedPeer, LinkError> {
    if invitation.is_expired() {
        // An expired invitation never reaches the wire.
        let e = PairError::Expired;
        return Err(LinkError::closed(e.reason(), e.to_string()));
    }
    let limit = node.handshake_timeout;
    let config = tls::client_config(
        &node.identity,
        PeerPolicy::Fingerprint(invitation.fingerprint),
    )?;
    let (mut ws, peer, exporter) =
        ws::connect(invitation.addr, config, Channel::Pair, limit).await?;
    let est = handshake::run(
        &mut ws,
        &node.own(None),
        Side::Dialer,
        Channel::Pair,
        exporter,
        peer,
    )
    .await?;
    let proof = HandshakeFrame::PairProof {
        invitation: invitation.id.to_string(),
        proof: invitation.joiner_proof(&est.transcript),
    };
    ws::send_within(&mut ws, &proof, limit).await?;
    let answer = match tokio::time::timeout(limit, ws::recv_frame(&mut ws)).await {
        Ok(Ok((_, HandshakeFrame::Paired { proof }))) => proof,
        Ok(Ok(_)) => {
            let e = HandshakeError::Protocol("expected Paired".into());
            return Err(ws::refuse(&mut ws, e).await);
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ws::timed_out(&mut ws, STEP_PROOF, limit).await),
    };
    if let Err(e) = invitation.check_inviter_proof(&est.transcript, &answer) {
        return Err(ws::close_reason(&mut ws, e.reason(), e.to_string()).await);
    }
    let paired = PairedPeer {
        key: peer,
        role: node.peer_role(),
        scopes: scopes.into_iter().collect(),
    };
    if let Err(detail) = node.sink.paired(&paired) {
        return Err(ws::close_reason(&mut ws, reason::PAIRING_FAILED, detail).await);
    }
    node.pins.pin(peer, paired.role);
    ws::close_quietly(&mut ws).await;
    tracing::info!(peer = %peer.fingerprint().short(), "paired");
    Ok(paired)
}

/// The inviter's side of the pair channel, after the WebSocket upgrade.
pub(crate) async fn accept(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    ip: IpAddr,
    exporter: [u8; 32],
) -> Result<(), LinkError> {
    let limit = node.handshake_timeout;
    let Some(invitations) = node.invitations.clone() else {
        let e = HandshakeError::Protocol("pairing is not open on this listener".into());
        return Err(ws::refuse(&mut ws, e).await);
    };
    let _slot = match node.pair_limiter.admit(ip) {
        Ok(permit) => permit,
        Err(e) => {
            // Read the joiner's Hello first, so it gets the reason and not a
            // broken pipe (DL-025).
            let _ = tokio::time::timeout(limit, ws::recv_text(&mut ws)).await;
            tracing::warn!(%ip, reason = e.reason(), "pairing attempt refused");
            return Err(ws::close_reason(&mut ws, e.reason(), e.to_string()).await);
        }
    };
    let est = handshake::run(
        &mut ws,
        &node.own(None),
        Side::Listener,
        Channel::Pair,
        exporter,
        peer,
    )
    .await?;
    let (id, proof) = match tokio::time::timeout(limit, ws::recv_frame(&mut ws)).await {
        Ok(Ok((_, HandshakeFrame::PairProof { invitation, proof }))) => (invitation, proof),
        Ok(Ok(_)) => {
            let e = HandshakeError::Protocol("expected PairProof".into());
            return Err(ws::refuse(&mut ws, e).await);
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ws::timed_out(&mut ws, STEP_PROOF, limit).await),
    };
    let Some(id) = InvitationId::parse(&id) else {
        let e = PairError::InvalidProof;
        return Err(ws::close_reason(&mut ws, e.reason(), e.to_string()).await);
    };
    let redeemed = match invitations.redeem(&id, &est.transcript, &proof) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%ip, invitation = %id, reason = e.reason(), "pairing proof refused");
            return Err(ws::close_reason(&mut ws, e.reason(), e.to_string()).await);
        }
    };
    let paired = PairedPeer {
        key: peer,
        role: node.peer_role(),
        scopes: redeemed.scopes.into_iter().collect::<BTreeSet<_>>(),
    };
    if let Err(detail) = node.sink.paired(&paired) {
        return Err(ws::close_reason(&mut ws, reason::PAIRING_FAILED, detail).await);
    }
    node.pins.pin(peer, paired.role);
    let answer = HandshakeFrame::Paired {
        proof: redeemed.answer,
    };
    ws::send_within(&mut ws, &answer, limit).await?;
    ws::close_quietly(&mut ws).await;
    tracing::info!(peer = %peer.fingerprint().short(), invitation = %id, "paired");
    invitations.notify_paired(paired);
    Ok(())
}
