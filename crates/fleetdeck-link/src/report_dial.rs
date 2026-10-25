//! The reporter's side of a setup report (BG-3, design
//! `docs/v2/design/board-gaps.md` section 4.2): dial the pair channel of the
//! deck pinned to its fingerprint, send one `ProvisionReport`, read the one
//! `ProvisionReportReply` and close.
//!
//! This function stores nothing and never falls back to an unpinned
//! connection: a deck key other than `deck` is `fingerprint_mismatch`.

use std::net::SocketAddr;
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;

use crate::hello::{HandshakeError, HandshakeFrame, feature};
use crate::node::NodeConfig;
use crate::pair_dial::open_pair;
use crate::pair_request::reason;
use crate::report_accept::{MAX_FRAME_BYTES, ReportReply, ValidReport};
use crate::ws::{self, LinkError};

/// Closes the connection and returns the protocol error.
async fn protocol(ws: &mut ws::Ws, what: &str) -> LinkError {
    ws::close_quietly(ws).await;
    LinkError::Protocol(what.to_owned())
}

/// Sends `report` to the deck at `addr` with link key fingerprint `deck`.
///
/// # Errors
/// [`LinkError`]; a deck key other than `deck` is `fingerprint_mismatch`; a
/// deck without `provision_report.v1` is `unsupported`; a refusal of the
/// deck (`invalid_request`, `already_paired`, `unsupported`) keeps its
/// reason; an answer that is not a valid reply is [`LinkError::Protocol`].
/// `limit` bounds the send and the wait for the reply.
pub async fn send_report(
    node: &NodeConfig,
    addr: SocketAddr,
    deck: Fingerprint,
    report: &ValidReport,
    limit: Duration,
) -> Result<ReportReply, LinkError> {
    let (mut ws, _peer, est) = open_pair(node, addr, deck).await?;
    if !est.negotiated.has(feature::PROVISION_REPORT) {
        return Err(ws::close_reason(
            &mut ws,
            reason::UNSUPPORTED,
            "This deck does not accept setup reports".to_owned(),
        )
        .await);
    }
    ws::send_within(&mut ws, &report.frame(), limit).await?;
    let frame =
        match tokio::time::timeout(limit, ws::recv_frame_capped(&mut ws, MAX_FRAME_BYTES)).await {
            Ok(Ok((_, frame))) => frame,
            Ok(Err(LinkError::Handshake(HandshakeError::BadHello(_)))) => {
                return Err(protocol(&mut ws, "the reply is not a setup report reply").await);
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ws::timed_out(&mut ws, "report reply", limit).await),
        };
    let HandshakeFrame::ProvisionReportReply {
        status,
        retry_after_seconds,
        retry_id,
        terminal_id,
        state_digest,
    } = frame
    else {
        return Err(protocol(&mut ws, "expected ProvisionReportReply").await);
    };
    let reply = ReportReply {
        status,
        retry_after_seconds,
        retry_id,
        terminal_id,
        state_digest,
    };
    let Some(reply) = reply.checked() else {
        return Err(protocol(&mut ws, "the reply has wrong retry fields").await);
    };
    ws::close_quietly(&mut ws).await;
    Ok(reply)
}
