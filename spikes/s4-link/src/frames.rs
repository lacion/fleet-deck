//! Control channel frames (PLAN §8.3) and the link method table (§8.4, L15).
//!
//! Every link method declares the scope it needs and, if any, the feature
//! flag it needs. A request is answered with an error when the method is
//! unknown (fail closed), when its feature was not negotiated, or when the
//! caller's pairing did not grant its scope.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hello::{Negotiated, feature};

/// The largest control frame (§8.1).
pub const MAX_CONTROL_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlFrame {
    Request {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        method: String,
        #[serde(default)]
        params: Value,
    },
    Response {
        id: u64,
        #[serde(flatten)]
        outcome: Outcome,
    },
    Event {
        station_seq: u64,
        session_id: String,
        session_seq: u64,
        event: Value,
    },
    /// deck → station, after the read model committed `station_seq`.
    Ack {
        station_seq: u64,
    },
    Snapshot {
        session_id: String,
        as_of_station_seq: u64,
        state: Value,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// The sender closes the link; `reason` is a stable code.
    Close {
        reason: String,
        detail: String,
    },
}

/// `Response { id, ok | err }`: exactly one of the two keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Outcome {
    #[serde(rename = "ok")]
    Ok(Value),
    #[serde(rename = "err")]
    Err(ErrorBody),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("control frame of {0} bytes is larger than {MAX_CONTROL_FRAME}")]
    TooLarge(usize),
    #[error("bad control frame: {0}")]
    Json(#[from] serde_json::Error),
}

impl ControlFrame {
    pub fn encode(&self) -> Result<String, FrameError> {
        let text = serde_json::to_string(self)?;
        if text.len() > MAX_CONTROL_FRAME {
            return Err(FrameError::TooLarge(text.len()));
        }
        Ok(text)
    }

    pub fn decode(text: &str) -> Result<Self, FrameError> {
        if text.len() > MAX_CONTROL_FRAME {
            return Err(FrameError::TooLarge(text.len()));
        }
        Ok(serde_json::from_str(text)?)
    }
}

/// Link scopes (L15). A pairing grants a set; an exchange may narrow it but
/// never widen it (S4-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Read station state, sessions, events.
    Read,
    /// Prompt, steer, interrupt, start and stop sessions.
    Drive,
    /// Answer approvals and questions.
    Approve,
    /// Station configuration, provider setup, shutdown.
    Admin,
    /// Station → deck: coordinator calls of agents on that station (L14).
    Coordinate,
}

impl Scope {
    pub const ALL: [Scope; 5] = [
        Scope::Read,
        Scope::Drive,
        Scope::Approve,
        Scope::Admin,
        Scope::Coordinate,
    ];
}

/// One entry of the method table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub scope: Scope,
    pub feature: Option<&'static str>,
    /// Mutating methods carry a `command_id` (§8.4).
    pub mutating: bool,
}

const fn m(
    name: &'static str,
    scope: Scope,
    feature: Option<&'static str>,
    mutating: bool,
) -> MethodSpec {
    MethodSpec {
        name,
        scope,
        feature,
        mutating,
    }
}

/// The link methods of the spike. Phase 1 replaces this list with the
/// generated one from the protocol crate; the rule stays: no entry, no call.
pub const METHODS: &[MethodSpec] = &[
    m("station.info", Scope::Read, None, false),
    m("session.list", Scope::Read, None, false),
    m("events.replay", Scope::Read, None, false),
    m(
        "session.snapshot",
        Scope::Read,
        Some(feature::SNAPSHOTS),
        false,
    ),
    m(
        "artifact.open",
        Scope::Read,
        Some(feature::DATA_STREAMS),
        false,
    ),
    m("session.start", Scope::Drive, None, true),
    m("session.prompt", Scope::Drive, None, true),
    m("session.interrupt", Scope::Drive, None, true),
    m("session.stop", Scope::Drive, None, true),
    m("approval.answer", Scope::Approve, None, true),
    m("station.configure", Scope::Admin, None, true),
    m(
        "command.receipt",
        Scope::Read,
        Some(feature::COMMAND_RECEIPTS),
        false,
    ),
    m("coordinator.call", Scope::Coordinate, None, true),
];

pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|s| s.name == name)
}

/// Decides if the peer may call `name` on this node. The order is fixed:
/// unknown method, then feature, then scope, then the `command_id` rule.
pub fn authorize(
    name: &str,
    command_id: Option<&str>,
    negotiated: &Negotiated,
    granted: &BTreeSet<Scope>,
) -> Result<&'static MethodSpec, ErrorBody> {
    let spec = method(name)
        .ok_or_else(|| ErrorBody::new("unknown_method", format!("no link method {name}")))?;
    if let Some(f) = spec.feature
        && !negotiated.has(f)
    {
        return Err(ErrorBody::new(
            "feature_not_negotiated",
            format!("{name} needs feature {f}, which this link did not negotiate"),
        ));
    }
    if !granted.contains(&spec.scope) {
        return Err(ErrorBody::new(
            "forbidden",
            format!(
                "{name} needs scope {:?}, which the pairing did not grant",
                spec.scope
            ),
        ));
    }
    if spec.mutating && command_id.is_none_or(str::is_empty) {
        return Err(ErrorBody::new(
            "missing_command_id",
            format!("{name} changes state and needs a command_id"),
        ));
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn negotiated(features: &[&str]) -> Negotiated {
        Negotiated {
            version: 1,
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    #[test]
    fn frames_have_the_plan_shapes() {
        let ok = ControlFrame::Response {
            id: 3,
            outcome: Outcome::Ok(json!({"a":1})),
        };
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"type":"response","id":3,"ok":{"a":1}})
        );
        let err = ControlFrame::Response {
            id: 4,
            outcome: Outcome::Err(ErrorBody::new("forbidden", "no")),
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"type":"response","id":4,"err":{"code":"forbidden","message":"no"}})
        );
        let req =
            ControlFrame::decode(r#"{"type":"request","id":1,"method":"station.info"}"#).unwrap();
        assert_eq!(
            req,
            ControlFrame::Request {
                id: 1,
                command_id: None,
                method: "station.info".into(),
                params: Value::Null
            }
        );
        for f in [
            ControlFrame::Event {
                station_seq: 9,
                session_id: "s".into(),
                session_seq: 2,
                event: json!({"kind":"turn.started"}),
            },
            ControlFrame::Ack { station_seq: 9 },
            ControlFrame::Snapshot {
                session_id: "s".into(),
                as_of_station_seq: 9,
                state: json!({}),
            },
            ControlFrame::Ping { nonce: 1 },
            ControlFrame::Pong { nonce: 1 },
            ok,
            err,
        ] {
            assert_eq!(ControlFrame::decode(&f.encode().unwrap()).unwrap(), f);
        }
    }

    #[test]
    fn a_response_needs_ok_or_err() {
        assert!(ControlFrame::decode(r#"{"type":"response","id":1}"#).is_err());
        assert!(ControlFrame::decode(r#"{"type":"teleport","id":1}"#).is_err());
    }

    #[test]
    fn oversized_frames_are_refused_both_ways() {
        let big = ControlFrame::Event {
            station_seq: 1,
            session_id: "s".into(),
            session_seq: 1,
            event: json!("x".repeat(MAX_CONTROL_FRAME)),
        };
        assert!(matches!(big.encode(), Err(FrameError::TooLarge(_))));
        assert!(matches!(
            ControlFrame::decode(&" ".repeat(MAX_CONTROL_FRAME + 1)),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn authorization_checks_method_feature_scope_and_command_id() {
        let all = negotiated(feature::ALL);
        let read = BTreeSet::from([Scope::Read]);
        let full: BTreeSet<Scope> = Scope::ALL.into_iter().collect();
        assert!(authorize("station.info", None, &all, &read).is_ok());
        assert_eq!(
            authorize("rm.rf", None, &all, &full).unwrap_err().code,
            "unknown_method"
        );
        assert_eq!(
            authorize("session.prompt", Some("01J"), &all, &read)
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert!(authorize("session.prompt", Some("01J"), &all, &full).is_ok());
        assert_eq!(
            authorize("session.prompt", None, &all, &full)
                .unwrap_err()
                .code,
            "missing_command_id"
        );
        let control_only = negotiated(&[feature::CONTROL]);
        assert_eq!(
            authorize("artifact.open", None, &control_only, &full)
                .unwrap_err()
                .code,
            "feature_not_negotiated"
        );
    }

    #[test]
    fn every_method_name_is_unique() {
        let names: BTreeSet<_> = METHODS.iter().map(|m| m.name).collect();
        assert_eq!(names.len(), METHODS.len());
    }
}
