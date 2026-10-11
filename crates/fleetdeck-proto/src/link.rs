//! Control channel frames (ADR-004 §2.6 with GATE-M1 G4, G6, G8).
//!
//! JSON text frames tagged by `type`. Handshake and pairing frames belong to
//! `fleetdeck-link`; the method table with scopes and roles is there too.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::event::EventEnvelope;
use crate::ids::{CommandId, IncarnationId};
use crate::session::SessionSummary;

/// The largest control frame in bytes (ADR-004 §2.6).
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;

/// A typed error on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[error("{code}: {message}")]
#[ts(export_to = "link/")]
pub struct WireError {
    /// Stable code, for example `forbidden` or `unknown_method`.
    pub code: String,
    pub message: String,
    /// Typed details, for example a `Reject` or an `Unsupported` result.
    pub data: Option<Value>,
}

impl WireError {
    /// An error without details.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            data: None,
        }
    }
}

/// The result of a request: `{ "ok": value }` or `{ "err": {...} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum Outcome {
    Ok(Value),
    Err(WireError),
}

/// A permission that a pairing grants to the peer key (PLAN §9, L15).
/// The method table of `fleetdeck-link` names the scope of each method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum Scope {
    /// Read station state, sessions, events and artifacts.
    Read,
    /// Start, prompt, steer, interrupt, configure and stop sessions.
    Drive,
    /// Answer approvals and questions.
    Approve,
    /// Station configuration and key rotation.
    Admin,
    /// Station → deck: coordinator calls of agents on that station (L14).
    Coordinate,
}

impl Scope {
    /// Every scope, in table order.
    pub const ALL: [Self; 5] = [
        Self::Read,
        Self::Drive,
        Self::Approve,
        Self::Admin,
        Self::Coordinate,
    ];

    /// The wire name (`read`, `drive`, ...).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Drive => "drive",
            Self::Approve => "approve",
            Self::Admin => "admin",
            Self::Coordinate => "coordinate",
        }
    }
}

/// The projection of one session at a watermark (G4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "link/")]
pub struct SessionSnapshot {
    pub summary: SessionSummary,
}

/// One control frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "link/")]
pub enum ControlFrame {
    Request {
        /// Unique per direction, chosen by the sender.
        id: u64,
        /// Required for a mutating method.
        command_id: Option<CommandId>,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        #[serde(flatten)]
        outcome: Outcome,
    },
    /// Station → deck: one committed event.
    Event {
        incarnation: IncarnationId,
        #[serde(flatten)]
        envelope: EventEnvelope,
    },
    /// Deck → station after the read model committed `station_seq`.
    Ack {
        incarnation: IncarnationId,
        station_seq: u64,
    },
    /// Station → deck: a snapshot batch starts (needs `snapshots.v1`).
    SnapshotBegin {
        incarnation: IncarnationId,
        /// Every event up to this `station_seq` is in the snapshot.
        watermark: u64,
        session_count: u64,
    },
    SnapshotSession {
        incarnation: IncarnationId,
        snapshot: SessionSnapshot,
    },
    /// The batch is complete; sessions that the deck has but the batch did
    /// not name are removed.
    SnapshotEnd {
        incarnation: IncarnationId,
        watermark: u64,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// The last frame before the socket closes.
    Close {
        reason: String,
        detail: String,
    },
}

impl ControlFrame {
    /// The `type` tag of the frame.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Request { .. } => "request",
            Self::Response { .. } => "response",
            Self::Event { .. } => "event",
            Self::Ack { .. } => "ack",
            Self::SnapshotBegin { .. } => "snapshot_begin",
            Self::SnapshotSession { .. } => "snapshot_session",
            Self::SnapshotEnd { .. } => "snapshot_end",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::Close { .. } => "close",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use serde_json::json;

    fn round_trip(frame: &ControlFrame) -> Value {
        let v = serde_json::to_value(frame).unwrap();
        let back: ControlFrame = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(&back, frame);
        assert_eq!(v["type"], frame.type_name());
        v
    }

    #[test]
    fn responses_have_ok_or_err_beside_the_id() {
        let ok = ControlFrame::Response {
            id: 4,
            outcome: Outcome::Ok(json!({"a": 1})),
        };
        assert_eq!(
            round_trip(&ok),
            json!({"type": "response", "id": 4, "ok": {"a": 1}})
        );
        let err = ControlFrame::Response {
            id: 5,
            outcome: Outcome::Err(WireError::new("forbidden", "no")),
        };
        assert_eq!(
            round_trip(&err),
            json!({"type": "response", "id": 5, "err": {"code": "forbidden", "message": "no", "data": null}})
        );
    }

    #[test]
    fn events_flatten_the_envelope_and_carry_the_incarnation() {
        let frame = ControlFrame::Event {
            incarnation: "inc-1".into(),
            envelope: EventEnvelope {
                station_seq: 9,
                session_id: "s1".into(),
                session_seq: 2,
                at: 1,
                proto_version: 1,
                raw_ref: None,
                event: Event::ModelChanged { model: "m".into() }.into(),
            },
        };
        let v = round_trip(&frame);
        assert_eq!(v["incarnation"], "inc-1");
        assert_eq!(v["station_seq"], 9);
        assert_eq!(v["event"]["kind"], "model.changed");
    }

    #[test]
    fn every_other_frame_round_trips() {
        let summary = crate::session::SessionSummary::new("s1".into());
        for frame in [
            ControlFrame::Request {
                id: 1,
                command_id: Some("01J0000000000000000000000A".into()),
                method: "session.interrupt".into(),
                params: json!({"session_id": "s1"}),
            },
            ControlFrame::Ack {
                incarnation: "i".into(),
                station_seq: 3,
            },
            ControlFrame::SnapshotBegin {
                incarnation: "i".into(),
                watermark: 3,
                session_count: 1,
            },
            ControlFrame::SnapshotSession {
                incarnation: "i".into(),
                snapshot: SessionSnapshot { summary },
            },
            ControlFrame::SnapshotEnd {
                incarnation: "i".into(),
                watermark: 3,
            },
            ControlFrame::Ping { nonce: 1 },
            ControlFrame::Pong { nonce: 1 },
            ControlFrame::Close {
                reason: "superseded".into(),
                detail: String::new(),
            },
        ] {
            round_trip(&frame);
        }
    }
}
