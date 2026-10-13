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
