//! The pair channel (§8.5): the joiner dials the inviter, runs the normal
//! handshake (both keys proven, transcript bound to the TLS session), and
//! sends `PairProof`. The inviter redeems the invitation, pins the joiner,
//! grants the invitation's scopes and answers `Paired`. The joiner pins the
//! inviter only after it checked that answer.

use std::collections::BTreeSet;

use super::handshake::{
    self, STEP_PROOF, close_quietly, recv_frame, refuse, send_frame, send_within, timed_out,
};
use super::{LinkError, NodeConfig, Ws, connect_ws};
use crate::frames::Scope;
use crate::hello::{Channel, HandshakeError, HandshakeFrame, Side};
use crate::identity::PeerKey;
use crate::pairing::{Invitation, PairError, PairedPeer};
use crate::tls::{self, PeerPolicy};

/// Joins the node that issued `invitation`. On success the inviter's key is
/// pinned with its role and gets `scopes` on this node. The caller then dials
/// the inviter with [`super::dial`] or waits for it to dial.
pub async fn pair(
    node: &NodeConfig,
    invitation: &Invitation,
    scopes: impl IntoIterator<Item = Scope>,
) -> Result<PeerKey, LinkError> {
    if invitation.is_expired() {
        // Checked before any packet: an old invitation never reaches the wire.
        return Err(LinkError::Handshake(PairError::Expired.into()));
    }
    let config = tls::client_config(
        &node.identity,
        PeerPolicy::Fingerprint(invitation.fingerprint),
    )?;
    let (mut ws, peer, exporter) = connect_ws(
        invitation.addr,
        config,
        Channel::Pair,
        node.handshake_timeout,
    )
    .await?;
    let steps = async {
        let est = handshake::run(
            &mut ws,
            node,
            Side::Dialer,
            Channel::Pair,
            0,
            exporter,
            peer,
        )
        .await?;
        let proof = invitation.joiner_proof(&est.transcript);
        send_frame(&mut ws, &HandshakeFrame::PairProof { proof }).await?;
        match recv_frame(&mut ws).await? {
            (_, HandshakeFrame::Paired { proof }) => {
                if let Err(e) = invitation.check_inviter_proof(&est.transcript, &proof) {
                    return Err(refuse(&mut ws, e.into()).await);
                }
                Ok(())
            }
            _ => Err(refuse(&mut ws, HandshakeError::Protocol("expected Paired".into())).await),
        }
    };
    match tokio::time::timeout(node.handshake_timeout, steps).await {
        Ok(r) => r?,
        Err(_) => return Err(timed_out(&mut ws, STEP_PROOF, node.handshake_timeout).await),
    }
    node.pins.pin(peer, node.peer_role());
    node.grants.grant(peer, scopes);
    close_quietly(&mut ws).await;
    Ok(peer)
}

/// The inviter's side, after the handshake on the pair channel.
pub(super) async fn accept(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    transcript: &[u8],
) -> Result<(), LinkError> {
    let proof = match tokio::time::timeout(node.handshake_timeout, recv_frame(&mut ws)).await {
        Ok(Ok((_, HandshakeFrame::PairProof { proof }))) => proof,
        Ok(Ok(_)) => {
            return Err(refuse(
                &mut ws,
                HandshakeError::Protocol("expected PairProof".into()),
            )
            .await);
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(timed_out(&mut ws, STEP_PROOF, node.handshake_timeout).await),
    };
    let redeemed = match node.invites.redeem(transcript, &proof) {
        Ok(r) => r,
        Err(e) => return Err(refuse(&mut ws, e.into()).await),
    };
    let role = node.peer_role();
    node.pins.pin(peer, role);
    node.grants.grant(peer, redeemed.scopes.iter().copied());
    send_within(
        &mut ws,
        &HandshakeFrame::Paired {
            proof: redeemed.answer,
        },
        node.handshake_timeout,
    )
    .await?;
    close_quietly(&mut ws).await;
    node.invites.notify_paired(PairedPeer {
        key: peer,
        role,
        scopes: redeemed.scopes.into_iter().collect::<BTreeSet<_>>(),
    });
    Ok(())
}
