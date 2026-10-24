//! The deck's side of a pairing request on the pair channel (DL-178,
//! tailnet design 5.3 and 5.4): `PairRequest` and `PairResume`.
//!
//! A request is admitted by the [`PairRequests`] hub (window, limits,
//! cooldown). The connection then answers `PairPending` and waits for the
//! owner, with a heartbeat and a deadline. The handshake slots are released
//! before the wait; the unauthenticated permit of the listener stays with
//! the connection until the socket closes.
//!
//! `PairResume` re-signs a stored approval over the new transcript, for the
//! same proven key, request id and nonce, while the pairing is current.

use std::net::IpAddr;
use std::time::Duration;

use fleetdeck_core::identity::PeerKey;
use fleetdeck_proto::link::Scope;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::handshake::Established;
use crate::hello::{HandshakeError, HandshakeFrame, feature};
use crate::node::{ListenerKind, Listening, NodeConfig};
use crate::pair_hub::{Admitted, Decision, PairRequests, Refusal};
use crate::pair_request::{Approval, CanonicalRequest, reason, transcript_hash};
use crate::pairing::{hex, unhex};
use crate::tls::Role;
use crate::ws::{self, LinkError, Ws};

/// The fields of a `PairRequest` frame.
pub(crate) struct Fields {
    pub request_id: String,
    pub name: String,
    pub nonce: String,
    pub offered_scopes: Vec<Scope>,
}

/// Why the wait of a pending request ended.
enum Waited {
    Decided(Decision),
    /// The station closed, or the read failed.
    Gone,
    /// The station sent a message that the deck does not expect.
    Protocol,
    /// No pong for too long.
    Dead,
    Expired,
}

fn hub_of(node: &NodeConfig) -> Option<&std::sync::Arc<PairRequests>> {
    node.pair_requests.as_ref()
}

async fn unsupported(ws: &mut Ws, detail: &str) -> LinkError {
    ws::close_reason(ws, reason::UNSUPPORTED, detail.to_owned()).await
}

/// `PairRequest`: admit, answer `PairPending`, wait for the owner.
pub(crate) async fn request(
    node: &NodeConfig,
    mut ws: Ws,
    est: &Established,
    (peer, ip): (PeerKey, IpAddr),
    on: &Listening,
    fields: Fields,
    slot: OwnedSemaphorePermit,
) -> Result<(), LinkError> {
    // The handshake slots are released before a request waits (5.4).
    drop(slot);
    let limit = on.hello;
    if !est.negotiated.has(feature::PAIR_REQUEST) {
        return Err(unsupported(&mut ws, "the feature pair_request.v1 was not negotiated").await);
    }
    let Some(hub) = hub_of(node).cloned() else {
        return Err(unsupported(&mut ws, "this node accepts no pairing requests").await);
    };
    if on.kind != ListenerKind::Tailnet {
        // A LAN or loopback listener never accepts pairing without an
        // invitation.
        let detail = "pairing requests are accepted on tailnet listeners only".to_owned();
        return Err(ws::close_reason(&mut ws, reason::WINDOW_CLOSED, detail).await);
    }
    let req = match CanonicalRequest::from_frame(
        &fields.request_id,
        &fields.name,
        &fields.nonce,
        &fields.offered_scopes,
    ) {
        Ok(r) => r,
        Err(e) => return Err(ws::refuse(&mut ws, HandshakeError::Protocol(e.to_string())).await),
    };
    let already = node.pins.role_of(&peer) == Some(Role::Station);
    let admitted = match hub.admit(req.clone(), peer, ip, already).await {
        Ok(a) => a,
        Err(refusal) => {
            if let Refusal::Store(e) = &refusal {
                tracing::warn!(error = %e, "a pairing request could not be checked");
            }
            tracing::info!(%ip, reason = refusal.code(), "pairing request refused");
            let detail = refusal_text(&refusal);
            return Err(ws::close_reason(&mut ws, refusal.code(), detail).await);
        }
    };
    let id = admitted.id.clone();
    tracing::info!(
        %ip,
        request = %id,
        station = %peer.fingerprint().short(),
        "pairing request admitted"
    );
    let pending = HandshakeFrame::PairPending {
        request_id: id.clone(),
    };
    if let Err(e) = ws::send_within(&mut ws, &pending, limit).await {
        hub.finish(&id).await;
        return Err(e);
    }
    let waited = wait(&mut ws, &hub, admitted).await;
    answer(node, ws, est, (&req, peer), &hub, &id, waited).await
}

fn refusal_text(r: &Refusal) -> String {
    match r {
        Refusal::WindowClosed => "the deck does not accept pairing requests now",
        Refusal::AlreadyPaired => "this station key is already paired with this deck",
        Refusal::Denied => "the owner denied a request of this key a short time ago",
        Refusal::RequestIdConflict => "the request id belongs to another request or key",
        Refusal::Busy => "the deck has too many pairing requests; try again later",
        Refusal::Store(_) => "the deck could not check the request",
    }
    .to_owned()
}

/// Waits for the decision of the owner, with the heartbeat and the
/// deadline of the request.
async fn wait(ws: &mut Ws, hub: &PairRequests, admitted: Admitted) -> Waited {
    let Admitted {
        id,
        mut decision,
        expires,
    } = admitted;
    let cfg = hub.config();
    let mut ping = tokio::time::interval(cfg.ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_pong = Instant::now();
    let waited = loop {
        tokio::select! {
            d = &mut decision => break match d {
                Ok(d) => Waited::Decided(d),
                Err(_) => Waited::Gone,
            },
            msg = ws.next() => match msg {
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(Message::Ping(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Text(_) | Message::Binary(_))) => break Waited::Protocol,
                Some(Ok(Message::Close(_)) | Err(_)) | None => break Waited::Gone,
            },
            _ = ping.tick() => {
                if last_pong.elapsed() >= cfg.dead_after {
                    break Waited::Dead;
                }
                let sent = tokio::time::timeout(
                    ws::CLEANUP_LIMIT.max(Duration::from_secs(1)),
                    ws.send(Message::Ping(Vec::new().into())),
                )
                .await;
                if !matches!(sent, Ok(Ok(()))) {
                    break Waited::Gone;
                }
            },
            () = tokio::time::sleep_until(expires) => {
                // A decision that raced the timer wins.
                if hub.finish(&id).await {
                    break Waited::Expired;
                }
            },
        }
    };
    if !matches!(waited, Waited::Decided(_) | Waited::Expired) {
        // A disconnect cancels the request (serialized against approve).
        hub.finish(&id).await;
    }
    waited
}

/// Sends the answer of the wait and ends the connection.
async fn answer(
    node: &NodeConfig,
    mut ws: Ws,
    est: &Established,
    (req, peer): (&CanonicalRequest, PeerKey),
    hub: &PairRequests,
    id: &str,
    waited: Waited,
) -> Result<(), LinkError> {
    match waited {
        Waited::Decided(Decision::Approved) => {
            let signature = Approval {
                transcript_hash: transcript_hash(&est.transcript),
                station: peer.fingerprint(),
                deck: node.identity.fingerprint(),
                request: req,
                granted_scopes: hub.granted_scopes(),
            }
            .sign(&node.identity);
            let frame = HandshakeFrame::PairApproved {
                request_id: id.to_owned(),
                granted_scopes: hub.granted_scopes().iter().copied().collect(),
                signature,
            };
            // The approval is committed; a failed write is the lost-answer
            // case, and the station resumes.
            ws::send_within(&mut ws, &frame, ws::CLEANUP_LIMIT).await?;
            ws::close_quietly(&mut ws).await;
            tracing::info!(request = %id, station = %peer.fingerprint().short(), "pairing request answered");
            Ok(())
        }
        Waited::Decided(Decision::Closed(code)) => {
            Err(ws::close_reason(&mut ws, code, closed_text(code)).await)
        }
        Waited::Expired => {
            Err(ws::close_reason(&mut ws, reason::EXPIRED, closed_text(reason::EXPIRED)).await)
        }
        Waited::Dead => Err(LinkError::Timeout(
            "pairing request heartbeat",
            hub.config().dead_after,
        )),
        Waited::Protocol => {
            let e = HandshakeError::Protocol("unexpected message while the request waits".into());
            Err(ws::refuse(&mut ws, e).await)
        }
        Waited::Gone => Err(LinkError::closed(
            reason::CANCELLED,
            "the station closed the connection before the owner decided",
        )),
    }
}

fn closed_text(code: &str) -> String {
    match code {
        reason::DENIED => "the owner denied the request",
        reason::EXPIRED => "the request expired before the owner decided",
        reason::CANCELLED => "the request was cancelled",
        reason::WINDOW_CLOSED => "the pairing window closed",
        _ => "the request ended",
    }
    .to_owned()
}

async fn unknown_request(ws: &mut Ws) -> LinkError {
    ws::close_reason(
        ws,
        reason::UNKNOWN_REQUEST,
        "the deck holds no approval for this request".to_owned(),
    )
    .await
}

/// `PairResume`: re-signs the stored approval over this connection.
pub(crate) async fn resume(
    node: &NodeConfig,
    mut ws: Ws,
    est: &Established,
    peer: PeerKey,
    request_id: &str,
    nonce: &str,
) -> Result<(), LinkError> {
    if !est.negotiated.has(feature::PAIR_REQUEST) {
        return Err(unsupported(&mut ws, "the feature pair_request.v1 was not negotiated").await);
    }
    let Some(hub) = hub_of(node) else {
        return Err(unsupported(&mut ws, "this node accepts no pairing requests").await);
    };
    if unhex::<16>(request_id).is_none() || unhex::<32>(nonce).is_none() {
        let e = HandshakeError::Protocol(
            "a resume needs a 32 hex request id and a 64 hex nonce".into(),
        );
        return Err(ws::refuse(&mut ws, e).await);
    }
    let done = match hub.approver().completion(request_id).await {
        Ok(Some(c)) if c.station_key == peer && hex(&c.nonce) == nonce => c,
        Ok(_) => return Err(unknown_request(&mut ws).await),
        Err(e) => {
            tracing::warn!(error = %e, "a stored approval could not be read");
            return Err(ws::close_reason(
                &mut ws,
                "pairing_failed",
                "the deck could not read its records".to_owned(),
            )
            .await);
        }
    };
    // The pairing must be current: the key is still pinned as a station and
    // the deck key is the one that signed the approval.
    if node.pins.role_of(&peer) != Some(Role::Station)
        || done.deck_key != node.identity.public_key()
    {
        return Err(ws::close_reason(
            &mut ws,
            reason::REVOKED,
            "this pairing was removed or replaced".to_owned(),
        )
        .await);
    }
    let Ok(req) = CanonicalRequest::new(
        unhex::<16>(&done.request_id).unwrap_or_default(),
        done.requested_name.clone(),
        done.nonce,
        done.offered_scopes.clone(),
    ) else {
        return Err(unknown_request(&mut ws).await);
    };
    let signature = Approval {
        transcript_hash: transcript_hash(&est.transcript),
        station: peer.fingerprint(),
        deck: node.identity.fingerprint(),
        request: &req,
        granted_scopes: &done.granted_scopes,
    }
    .sign(&node.identity);
    let frame = HandshakeFrame::PairApproved {
        request_id: done.request_id.clone(),
        granted_scopes: done.granted_scopes.iter().copied().collect(),
        signature,
    };
    ws::send_within(&mut ws, &frame, ws::CLEANUP_LIMIT).await?;
    ws::close_quietly(&mut ws).await;
    tracing::info!(request = %done.request_id, station = %peer.fingerprint().short(), "pairing approval resumed");
    Ok(())
}
