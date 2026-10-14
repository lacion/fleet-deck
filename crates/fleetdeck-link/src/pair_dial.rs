//! The station's side of a pairing request (DL-178, tailnet design 5.2 and
//! 5.3): dial the pair channel of a deck pinned to its confirmed link key
//! fingerprint, send `PairRequest` (or `PairResume`) and verify the signed
//! `PairApproved`.
//!
//! This function stores nothing. The caller writes its retry file before it
//! calls, and it stores the deck pin, its grants and the dial target in one
//! local transaction after the signature is verified.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::Duration;

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_proto::link::Scope;

use crate::handshake;
use crate::hello::{Channel, HandshakeError, HandshakeFrame, Side, feature};
use crate::net::NetError;
use crate::node::NodeConfig;
use crate::pair_request::{Approval, CanonicalRequest, reason, transcript_hash};
use crate::tls::{self, PeerPolicy};
use crate::ws::{self, LinkError};

/// How long the station waits for the owner (the deck waits 10 minutes).
pub const WAIT_FOR_OWNER: Duration = Duration::from_secs(11 * 60);

/// Which frame the station sends first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairMode {
    /// A new request; the deck answers `PairPending` and waits.
    Request,
    /// The stored approval of a request that was sent before.
    Resume,
}

/// A verified approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairApproved {
    /// The deck key that TLS proved and that signed the approval.
    pub deck: PeerKey,
    /// What the deck grants to this station on the deck.
    pub granted_scopes: BTreeSet<Scope>,
}

fn is_certificate_failure(e: &std::io::Error) -> bool {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|inner| matches!(inner, rustls::Error::InvalidCertificate(_)))
}

/// A failed connect: a deck key other than the confirmed one is
/// `fingerprint_mismatch`; a deck that ended the connection after the
/// handshake did not accept this key (the window is closed, or the deck is
/// older than this feature).
fn connect_failure(e: LinkError) -> LinkError {
    match &e {
        LinkError::Net(NetError::Io(io)) if is_certificate_failure(io) => LinkError::closed(
            reason::FINGERPRINT_MISMATCH,
            "the deck link key is not the confirmed fingerprint",
        ),
        LinkError::Ws(_) => LinkError::closed(
            reason::WINDOW_CLOSED,
            "the deck did not accept the connection: its pairing window is closed, or it does not accept pairing requests",
        ),
        _ => e,
    }
}

/// Opens the pair channel of the deck at `addr`, pinned to the link key
/// fingerprint `deck`, and runs the handshake. It sends no pairing frame.
///
/// # Errors
/// [`LinkError`]; a deck key other than `deck` is `fingerprint_mismatch`.
#[doc(hidden)]
pub async fn open_pair(
    node: &NodeConfig,
    addr: SocketAddr,
    deck: Fingerprint,
) -> Result<(ws::Ws, PeerKey, crate::Established), LinkError> {
    let limit = node.handshake_timeout;
    let config = tls::client_config(&node.identity, PeerPolicy::Fingerprint(deck))?;
    let (mut ws, peer, exporter) = ws::connect(addr, config, Channel::Pair, limit)
        .await
        .map_err(connect_failure)?;
    let est = handshake::run(
        &mut ws,
        &node.own(None),
        Side::Dialer,
        Channel::Pair,
        exporter,
        peer,
    )
    .await?;
    Ok((ws, peer, est))
}

/// Asks the deck at `addr` with link key fingerprint `deck` to approve
/// `request` (or to answer the stored approval again).
///
/// # Errors
/// [`LinkError`] with a stable [`reason`] code: `window_closed`, `busy`,
/// `already_paired`, `request_id_conflict`, `denied`, `expired`,
/// `cancelled`, `unknown_request`, `revoked`, `fingerprint_mismatch`,
/// `unsupported`, `timeout`, `bad_signature`.
///
/// `before_send` runs once, after the handshake and before `PairRequest`
/// leaves (never for a resume): the caller writes its retry file there. An
/// error ends the attempt with `pairing_failed` and sends nothing.
pub async fn request_pairing(
    node: &NodeConfig,
    addr: SocketAddr,
    deck: Fingerprint,
    request: &CanonicalRequest,
    mode: PairMode,
    wait: Duration,
    before_send: impl FnOnce() -> Result<(), String>,
) -> Result<PairApproved, LinkError> {
    let limit = node.handshake_timeout;
    let (mut ws, peer, est) = open_pair(node, addr, deck).await?;
    if !est.negotiated.has(feature::PAIR_REQUEST) {
        return Err(ws::close_reason(
            &mut ws,
            reason::UNSUPPORTED,
            "This deck does not accept pairing requests; use an invitation".to_owned(),
        )
        .await);
    }
    let id = request.id_hex();
    let first = match mode {
        PairMode::Request => HandshakeFrame::PairRequest {
            request_id: id.clone(),
            name: request.name.clone(),
            nonce: request.nonce_hex(),
            offered_scopes: request.scopes_vec(),
        },
        PairMode::Resume => HandshakeFrame::PairResume {
            request_id: id.clone(),
            nonce: request.nonce_hex(),
        },
    };
    if mode == PairMode::Request
        && let Err(detail) = before_send()
    {
        return Err(ws::close_reason(&mut ws, "pairing_failed", detail).await);
    }
    ws::send_within(&mut ws, &first, limit).await?;
    if mode == PairMode::Request {
        match tokio::time::timeout(limit, ws::recv_frame(&mut ws)).await {
            Ok(Ok((_, HandshakeFrame::PairPending { request_id }))) if request_id == id => {}
            Ok(Ok(_)) => {
                let e = HandshakeError::Protocol("expected PairPending".into());
                return Err(ws::refuse(&mut ws, e).await);
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ws::timed_out(&mut ws, "pending answer", limit).await),
        }
    }
    let wait = if mode == PairMode::Request {
        wait
    } else {
        limit
    };
    let (granted, signature) = match tokio::time::timeout(wait, ws::recv_frame(&mut ws)).await {
        Ok(Ok((
            _,
            HandshakeFrame::PairApproved {
                request_id,
                granted_scopes,
                signature,
            },
        ))) if request_id == id => (granted_scopes, signature),
        Ok(Ok(_)) => {
            let e = HandshakeError::Protocol("expected PairApproved".into());
            return Err(ws::refuse(&mut ws, e).await);
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(ws::timed_out(&mut ws, "wait for the owner", wait).await),
    };
    let granted_scopes: BTreeSet<Scope> = granted.into_iter().collect();
    let approval = Approval {
        transcript_hash: transcript_hash(&est.transcript),
        station: node.identity.fingerprint(),
        deck: peer.fingerprint(),
        request,
        granted_scopes: &granted_scopes,
    };
    if !approval.verify(&peer, &signature) {
        return Err(ws::refuse(&mut ws, HandshakeError::BadSignature).await);
    }
    ws::close_quietly(&mut ws).await;
    Ok(PairApproved {
        deck: peer,
        granted_scopes,
    })
}
