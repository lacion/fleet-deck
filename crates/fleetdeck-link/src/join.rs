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
use tokio::sync::OwnedSemaphorePermit;

use crate::handshake;
use crate::hello::{Channel, HandshakeError, HandshakeFrame, Side};
use crate::node::{Listening, NodeConfig};
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
    if let Err(detail) = node.sink.paired(&paired).await {
        return Err(ws::close_reason(&mut ws, reason::PAIRING_FAILED, detail).await);
    }
    node.pins.pin(peer, paired.role);
    ws::close_quietly(&mut ws).await;
    tracing::info!(peer = %peer.fingerprint().short(), "paired");
    Ok(paired)
}

/// Admits one pairing attempt of `ip`: the rate limit and one of the 4
/// handshake slots. A refused attempt reads the joiner's Hello first (when
/// `read_hello`), so it gets the reason and not a broken pipe (DL-025).
async fn admit_attempt(
    node: &NodeConfig,
    ws: &mut Ws,
    ip: IpAddr,
    limit: std::time::Duration,
    max_message: usize,
    read_hello: bool,
) -> Result<OwnedSemaphorePermit, LinkError> {
    match node.pair_limiter.admit(ip) {
        Ok(permit) => Ok(permit),
        Err(e) => {
            if read_hello {
                let _ = tokio::time::timeout(limit, ws::recv_text_capped(ws, max_message)).await;
            }
            tracing::warn!(%ip, reason = e.reason(), "pairing attempt refused");
            Err(ws::close_reason(ws, e.reason(), e.to_string()).await)
        }
    }
}

/// The inviter's side of the pair channel, after the WebSocket upgrade.
///
/// A key that is not pinned takes a handshake slot first, as before. A
/// pinned station key can only resume an approval (`PairResume`) or pair
/// again: it takes its slot after its first frame, and `PairResume` takes
/// none (it only re-signs a stored approval for the same proven key).
pub(crate) async fn accept(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    ip: IpAddr,
    exporter: [u8; 32],
    on: &Listening,
) -> Result<(), LinkError> {
    let limit = on.hello;
    let pinned = node.pins.role_of(&peer) == Some(node.peer_role());
    let mut slot = None;
    if !pinned {
        slot = Some(admit_attempt(node, &mut ws, ip, limit, on.max_message, true).await?);
    }
    let est = handshake::run(
        &mut ws,
        &node.own_on(on),
        Side::Listener,
        Channel::Pair,
        exporter,
        peer,
    )
    .await?;
    let frame = match tokio::time::timeout(limit, ws::recv_frame_capped(&mut ws, on.max_message))
        .await
    {
        Ok(Ok((_, frame))) => frame,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ws::timed_out(&mut ws, STEP_PROOF, limit).await),
    };
    if let HandshakeFrame::PairResume { request_id, nonce } = frame {
        return crate::pair_accept::resume(node, ws, &est, peer, &request_id, &nonce).await;
    }
    let slot = match slot {
        Some(slot) => slot,
        None => admit_attempt(node, &mut ws, ip, limit, on.max_message, false).await?,
    };
    match frame {
        HandshakeFrame::PairProof { invitation, proof } => {
            invite(node, ws, &est, peer, ip, (invitation, proof), limit, slot).await
        }
        HandshakeFrame::PairRequest {
            request_id,
            name,
            nonce,
            offered_scopes,
        } => {
            let fields = crate::pair_accept::Fields {
                request_id,
                name,
                nonce,
                offered_scopes,
            };
            crate::pair_accept::request(node, ws, &est, (peer, ip), on, fields, slot).await
        }
        _ => {
            let e = HandshakeError::Protocol("expected PairProof".into());
            Err(ws::refuse(&mut ws, e).await)
        }
    }
}

/// The invitation proof of the pair channel.
#[allow(clippy::too_many_arguments)]
async fn invite(
    node: &NodeConfig,
    mut ws: Ws,
    est: &crate::handshake::Established,
    peer: PeerKey,
    ip: IpAddr,
    (id, proof): (String, String),
    limit: std::time::Duration,
    _slot: OwnedSemaphorePermit,
) -> Result<(), LinkError> {
    let Some(invitations) = node.invitations.clone() else {
        let e = HandshakeError::Protocol("pairing is not open on this listener".into());
        return Err(ws::refuse(&mut ws, e).await);
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
    if let Err(detail) = node.sink.paired(&paired).await {
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
