//! The link method table and the authorization of control frames
//! (ADR-004 §9.6, GATE-M1 G6 and G7).
//!
//! Every check uses the roles that were bound at pairing (the role of the
//! pinned peer key and the own role), never the dial direction. The table
//! must equal the table in ADR-004 §9.6; `tests/method_table.rs` compares
//! them.

use std::collections::BTreeSet;

use fleetdeck_proto::link::{ControlFrame, Scope, WireError};
use fleetdeck_proto::version::{Negotiated, feature};

use crate::tls::Role;

/// Who may call a method, by the roles bound at pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callers {
    /// The deck calls, the station answers.
    DeckToStation,
    /// The station calls, the deck answers.
    StationToDeck,
    /// Either side calls the other.
    Either,
}

impl Callers {
    /// True if a node with role `caller` may call a node with role `receiver`.
    #[must_use]
    pub fn allows(self, caller: Role, receiver: Role) -> bool {
        match self {
            Self::DeckToStation => caller == Role::Deck && receiver == Role::Station,
            Self::StationToDeck => caller == Role::Station && receiver == Role::Deck,
            Self::Either => caller != receiver,
        }
    }
}

/// One row of the method table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodSpec {
    pub name: &'static str,
    pub callers: Callers,
    pub scope: Scope,
    /// A feature that both sides must have negotiated.
    pub feature: Option<&'static str>,
    /// A mutating method needs a `command_id` and a typed envelope.
    pub mutating: bool,
}

const fn m(
    name: &'static str,
    callers: Callers,
    scope: Scope,
    feature: Option<&'static str>,
    mutating: bool,
) -> MethodSpec {
    MethodSpec {
        name,
        callers,
        scope,
        feature,
        mutating,
    }
}

const fn drive(name: &'static str) -> MethodSpec {
    m(name, Callers::DeckToStation, Scope::Drive, None, true)
}

use Callers::{DeckToStation, Either, StationToDeck};

/// Method table v1 (Revision 2).
pub const METHODS: &[MethodSpec] = &[
    m("station.info", DeckToStation, Scope::Read, None, false),
    m("session.list", DeckToStation, Scope::Read, None, false),
    m("events.replay", DeckToStation, Scope::Read, None, false),
    m(
        "session.snapshot",
        DeckToStation,
        Scope::Read,
        Some(feature::SNAPSHOTS),
        false,
    ),
    m(
        "command.receipt",
        DeckToStation,
        Scope::Read,
        Some(feature::COMMAND_RECEIPTS),
        false,
    ),
    m(
        "artifact.open",
        DeckToStation,
        Scope::Read,
        Some(feature::DATA_STREAMS),
        false,
    ),
    // The spawn form's directory picker (M2 P4-4, DL-061): a read with the
    // drive scope, because only a caller that may start a session needs it.
    m("workspace.list", DeckToStation, Scope::Drive, None, false),
    m(
        "files.list",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.read",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.search",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "files.diff",
        DeckToStation,
        Scope::Drive,
        Some(feature::FILES),
        false,
    ),
    m(
        "shell.list",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        false,
    ),
    m(
        "shell.attach",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        false,
    ),
    m(
        "shell.open",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.resize",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.close",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL),
        true,
    ),
    m(
        "shell.handover",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL_HANDOVER),
        true,
    ),
    m(
        "shell.release",
        DeckToStation,
        Scope::Drive,
        Some(feature::SHELL_HANDOVER),
        true,
    ),
    drive("session.start"),
    drive("session.resume"),
    drive("session.fork"),
    drive("session.prompt"),
    drive("session.steer"),
    drive("session.follow_up"),
    drive("session.interrupt"),
    drive("session.set_mode"),
    drive("session.set_model"),
    drive("session.stop"),
    m(
        "session.revert",
        DeckToStation,
        Scope::Drive,
        Some(feature::REVERT),
        true,
    ),
    m(
        "session.resolve_revert",
        DeckToStation,
        Scope::Drive,
        Some(feature::REVERT),
        true,
    ),
    m(
        "session.set_auto_continue",
        DeckToStation,
        Scope::Drive,
        Some(feature::AUTO_CONTINUE),
        true,
    ),
    m(
        "session.coordinator_renew",
        DeckToStation,
        Scope::Drive,
        Some(feature::COORDINATOR),
        true,
    ),
    m(
        "session.coordinator_revoke",
        DeckToStation,
        Scope::Drive,
        Some(feature::COORDINATOR),
        true,
    ),
    m(
        "session.import",
        DeckToStation,
        Scope::Drive,
        Some(feature::IMPORT),
        true,
    ),
    m(
        "session.rename",
        DeckToStation,
        Scope::Drive,
        Some(feature::RENAME),
        true,
    ),
    m("approval.answer", DeckToStation, Scope::Approve, None, true),
    m("station.configure", DeckToStation, Scope::Admin, None, true),
    m("link.rotate_key", Either, Scope::Admin, None, true),
    m(
        "coordinator.query",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        false,
    ),
    m(
        "coordinator.call",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        true,
    ),
];

/// The row of `name`.
#[must_use]
pub fn method(name: &str) -> Option<&'static MethodSpec> {
    METHODS.iter().find(|s| s.name == name)
}

/// The request error codes of ADR-004 §2.11 and §9.8.
pub mod code {
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    pub const WRONG_ROLE: &str = "wrong_role";
    pub const FEATURE_NOT_NEGOTIATED: &str = "feature_not_negotiated";
    pub const FORBIDDEN: &str = "forbidden";
    pub const MISSING_COMMAND_ID: &str = "missing_command_id";
    pub const TOO_MANY_REQUESTS: &str = "too_many_requests";
    pub const PROTOCOL_ERROR: &str = "protocol_error";
    pub const TIMEOUT: &str = "timeout";
}

/// Checks a request in the ADR order: `unknown_method`, `wrong_role`,
/// `feature_not_negotiated`, `forbidden`, `missing_command_id`.
///
/// `granted` is `None` for the caller's own pre-check, which cannot know the
/// scopes that the receiver granted.
pub fn authorize(
    name: &str,
    caller: Role,
    receiver: Role,
    command_id: Option<&str>,
    negotiated: &Negotiated,
    granted: Option<&BTreeSet<Scope>>,
) -> Result<&'static MethodSpec, WireError> {
    let spec = method(name)
        .ok_or_else(|| WireError::new(code::UNKNOWN_METHOD, format!("no link method `{name}`")))?;
    if !spec.callers.allows(caller, receiver) {
        return Err(WireError::new(
            code::WRONG_ROLE,
            format!(
                "a {} cannot call `{name}` on a {}",
                role_name(caller),
                role_name(receiver)
            ),
        ));
    }
    if let Some(f) = spec.feature
        && !negotiated.has(f)
    {
        return Err(WireError::new(
            code::FEATURE_NOT_NEGOTIATED,
            format!("`{name}` needs feature {f}, which this link did not negotiate"),
        ));
    }
    if let Some(granted) = granted
        && !granted.contains(&spec.scope)
    {
        return Err(WireError::new(
            code::FORBIDDEN,
            format!(
                "`{name}` needs scope {}, which the pairing did not grant",
                spec.scope.name()
            ),
        ));
    }
    if spec.mutating && command_id.is_none_or(str::is_empty) {
        return Err(WireError::new(
            code::MISSING_COMMAND_ID,
            format!("`{name}` changes state and needs a command_id"),
        ));
    }
    Ok(spec)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use fleetdeck_proto::IncarnationId;
    use fleetdeck_proto::version::PROTO_MAX;

    fn negotiated(features: &[&str]) -> Negotiated {
        Negotiated {
            version: PROTO_MAX,
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    fn all() -> Negotiated {
        negotiated(feature::ALL)
    }

    fn scopes(s: &[Scope]) -> BTreeSet<Scope> {
        s.iter().copied().collect()
    }

    fn code_of(r: Result<&MethodSpec, WireError>) -> String {
        r.map_or_else(|e| e.code, |_| "ok".to_owned())
    }

    /// ADR-006 §8.3a 26b (DL-212).
    #[test]
    fn coordinator_renew_and_revoke_from_the_wrong_role_without_the_feature_or_without_drive_are_refused()
     {
        let (d, s) = (Role::Deck, Role::Station);
        let drive = scopes(&[Scope::Drive]);
        let read = scopes(&[Scope::Read]);
        let without = negotiated(&[feature::CONTROL, feature::COMMAND_RECEIPTS]);
        for name in ["session.coordinator_renew", "session.coordinator_revoke"] {
            // The deck sends it to a station: accepted with drive.
            assert_eq!(
                code_of(authorize(name, d, s, Some("c"), &all(), Some(&drive))),
                "ok"
            );
            // A station cannot send it.
            assert_eq!(
                code_of(authorize(name, s, d, Some("c"), &all(), Some(&drive))),
                "wrong_role"
            );
            assert_eq!(
                code_of(authorize(name, d, s, Some("c"), &without, Some(&drive))),
                code::FEATURE_NOT_NEGOTIATED
            );
            assert_eq!(
                code_of(authorize(name, d, s, Some("c"), &all(), Some(&read))),
                code::FORBIDDEN
            );
            assert_eq!(
                code_of(authorize(name, d, s, None, &all(), Some(&drive))),
                code::MISSING_COMMAND_ID
            );
        }
    }

    /// ADR-006 §8.3 26 (DL-214): without `coordinator.v1` a station's
    /// coordinator traffic is refused before any scope check, as an M2 deck
    /// does (it knows no coordinator shapes).
    #[test]
    fn a_peer_without_the_coordinator_feature_gets_the_m2_behavior() {
        let (d, s) = (Role::Deck, Role::Station);
        let coordinate = scopes(&[Scope::Coordinate]);
        let m2 = negotiated(&[feature::CONTROL, feature::COMMAND_RECEIPTS]);
        for (name, id) in [("coordinator.call", Some("c")), ("coordinator.query", None)] {
            assert_eq!(
                code_of(authorize(name, s, d, id, &m2, Some(&coordinate))),
                code::FEATURE_NOT_NEGOTIATED
            );
            assert_eq!(
                code_of(authorize(name, s, d, id, &all(), Some(&coordinate))),
                "ok"
            );
        }
    }

    #[test]
    fn request_checks_run_in_the_adr_order() {
        let none = scopes(&[]);
        let every = scopes(&Scope::ALL);
        let (d, s) = (Role::Deck, Role::Station);
        let n = negotiated(&[feature::CONTROL]);
        let cases = [
            // unknown_method before everything else.
            (
                authorize("x.y", s, s, None, &n, Some(&none)),
                "unknown_method",
            ),
            // wrong_role before the feature and the scope.
            (
                authorize("session.snapshot", s, d, None, &n, Some(&none)),
                "wrong_role",
            ),
            (
                authorize("coordinator.call", d, s, Some("c"), &all(), Some(&every)),
                "wrong_role",
            ),
            (
                authorize("link.rotate_key", d, d, Some("c"), &all(), Some(&every)),
                "wrong_role",
            ),
            // feature before the scope.
            (
                authorize("session.snapshot", d, s, None, &n, Some(&none)),
                "feature_not_negotiated",
            ),
            // forbidden before missing_command_id.
            (
                authorize(
                    "session.prompt",
                    d,
                    s,
                    None,
                    &n,
                    Some(&scopes(&[Scope::Read])),
                ),
                "forbidden",
            ),
            (
                authorize(
                    "session.prompt",
                    d,
                    s,
                    None,
                    &n,
                    Some(&scopes(&[Scope::Drive])),
                ),
                "missing_command_id",
            ),
            (
                authorize("session.prompt", d, s, Some(""), &n, Some(&every)),
                "missing_command_id",
            ),
            (
                authorize(
                    "session.prompt",
                    d,
                    s,
                    Some("c1"),
                    &n,
                    Some(&scopes(&[Scope::Drive])),
                ),
                "ok",
            ),
            (
                authorize(
                    "link.rotate_key",
                    s,
                    d,
                    Some("c"),
                    &n,
                    Some(&scopes(&[Scope::Admin])),
                ),
                "ok",
            ),
            (
                authorize(
                    "coordinator.query",
                    s,
                    d,
                    None,
                    &all(),
                    Some(&scopes(&[Scope::Coordinate])),
                ),
                "ok",
            ),
            // The caller's own pre-check skips the scope.
            (
                authorize("approval.answer", d, s, Some("c"), &n, None),
                "ok",
            ),
        ];
        for (i, (got, want)) in cases.into_iter().enumerate() {
            assert_eq!(code_of(got), want, "case {i}");
        }
    }

    #[test]
    fn stream_frames_have_a_fixed_direction_and_snapshots_need_their_feature() {
        let inc = || IncarnationId::from("inc-1".to_owned());
        let ack = ControlFrame::Ack {
            incarnation: inc(),
            station_seq: 1,
        };
        let begin = ControlFrame::SnapshotBegin {
            incarnation: inc(),
            watermark: 1,
            session_count: 0,
        };
        let end = ControlFrame::SnapshotEnd {
            incarnation: inc(),
            watermark: 1,
            coordinator: None,
        };
        let ping = ControlFrame::Ping { nonce: 1 };
        let n = negotiated(&[feature::CONTROL]);
        assert_eq!(check_frame(&ack, Role::Deck, &n), Ok(()));
        assert_eq!(
            check_frame(&ack, Role::Station, &n).unwrap_err().reason,
            "wrong_role"
        );
        assert_eq!(
            check_frame(&begin, Role::Deck, &all()).unwrap_err().reason,
            "wrong_role"
        );
        assert_eq!(
            check_frame(&begin, Role::Station, &n).unwrap_err().reason,
            "protocol_error"
        );
        assert_eq!(check_frame(&end, Role::Station, &all()), Ok(()));
        assert_eq!(check_frame(&ping, Role::Station, &n), Ok(()));
        assert_eq!(check_frame(&ping, Role::Deck, &n), Ok(()));
    }

    #[test]
    fn every_mutating_method_has_exactly_its_envelope_and_no_other_method_has_one() {
        for spec in METHODS {
            let envelope = fleetdeck_proto::mutation::envelope_of(spec.name);
            assert_eq!(envelope.is_some(), spec.mutating, "{}", spec.name);
        }
        for name in fleetdeck_proto::mutation::SESSION_METHODS {
            assert!(method(name).is_some_and(|s| s.mutating), "{name}");
        }
    }
}
