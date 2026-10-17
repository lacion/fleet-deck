//! The direction and feature check of stream frames (ADR-004 §9.6), split
//! from the method table to keep that file small.

use fleetdeck_proto::link::ControlFrame;
use fleetdeck_proto::version::{Negotiated, feature};

use super::code;
use crate::tls::Role;

/// Why a stream frame may not cross the link. The link closes with `reason`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}: {detail}")]
pub struct FrameRefused {
    pub reason: &'static str,
    pub detail: String,
}

/// Checks the fixed direction and the feature of a stream frame sent by a
/// node with role `sender` (ADR-004 §9.6): events and snapshots go station →
/// deck, acks go deck → station, snapshots need `snapshots.v1`. Request,
/// response, ping, pong and close go both ways.
pub fn check_frame(
    frame: &ControlFrame,
    sender: Role,
    negotiated: &Negotiated,
) -> Result<(), FrameRefused> {
    let (from, snapshot) = match frame {
        ControlFrame::Event { .. } => (Role::Station, false),
        ControlFrame::SnapshotBegin { .. }
        | ControlFrame::SnapshotSession { .. }
        | ControlFrame::SnapshotEnd { .. } => (Role::Station, true),
        ControlFrame::Ack { .. } => (Role::Deck, false),
        ControlFrame::Request { .. }
        | ControlFrame::Response { .. }
        | ControlFrame::Ping { .. }
        | ControlFrame::Pong { .. }
        | ControlFrame::Close { .. } => return Ok(()),
    };
    if sender != from {
        return Err(FrameRefused {
            reason: code::WRONG_ROLE,
            detail: format!(
                "a {} sent a `{}` frame, which only a {} sends",
                role_name(sender),
                frame.type_name(),
                role_name(from)
            ),
        });
    }
    if snapshot && !negotiated.has(feature::SNAPSHOTS) {
        return Err(FrameRefused {
            reason: code::PROTOCOL_ERROR,
            detail: format!(
                "a `{}` frame needs feature {}, which this link did not negotiate",
                frame.type_name(),
                feature::SNAPSHOTS
            ),
        });
    }
    Ok(())
}

/// `deck` or `station`.
#[must_use]
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Deck => "deck",
        Role::Station => "station",
    }
}
