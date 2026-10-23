use super::*;
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::link::ControlFrame;
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

/// MCP P4b-2a: `session.coordinator_set` needs `coordinator_live.v1`.
#[test]
fn coordinator_set_needs_its_feature_and_drive_from_a_deck() {
    let (d, s) = (Role::Deck, Role::Station);
    let name = "session.coordinator_set";
    let drive = scopes(&[Scope::Drive]);
    let read = scopes(&[Scope::Read]);
    // `coordinator.v1` alone is not enough.
    let without = negotiated(&[
        feature::CONTROL,
        feature::COMMAND_RECEIPTS,
        feature::COORDINATOR,
    ]);
    assert_eq!(
        code_of(authorize(name, d, s, Some("c"), &all(), Some(&drive))),
        "ok"
    );
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

/// ADR-008 §6.2: the two non-mutating `coordinator.v2` methods.
#[test]
fn the_fleet_control_methods_have_the_adr_008_rows() {
    let check = method("coordinator.authority_check").unwrap();
    assert_eq!(check.callers, Callers::StationToDeck);
    assert_eq!(check.scope, Scope::Coordinate);
    assert_eq!(check.feature, Some(feature::COORDINATOR_V2));
    assert!(!check.mutating);
    let policy = method("coordinator.policy_now").unwrap();
    assert_eq!(policy.callers, Callers::DeckToStation);
    assert_eq!(policy.scope, Scope::Read);
    assert_eq!(policy.feature, Some(feature::COORDINATOR_V2));
    assert!(!policy.mutating);
    let diff = method("files.coordinator_diff").unwrap();
    assert_eq!(diff.callers, Callers::DeckToStation);
    assert_eq!(diff.scope, Scope::Read);
    assert_eq!(diff.feature, Some(feature::COORDINATOR_V2));
    assert!(!diff.mutating);
}

#[test]
fn a_peer_without_coordinator_v2_gets_feature_not_negotiated_for_the_new_methods() {
    let (d, s) = (Role::Deck, Role::Station);
    let v1 = negotiated(&[feature::CONTROL, feature::COORDINATOR]);
    let both = negotiated(&[
        feature::CONTROL,
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
    ]);
    let coordinate = scopes(&[Scope::Coordinate]);
    let read = scopes(&[Scope::Read]);
    let check = "coordinator.authority_check";
    let policy = "coordinator.policy_now";
    assert_eq!(
        code_of(authorize(check, s, d, None, &v1, Some(&coordinate))),
        code::FEATURE_NOT_NEGOTIATED
    );
    assert_eq!(
        code_of(authorize(check, s, d, None, &both, Some(&coordinate))),
        "ok"
    );
    assert_eq!(
        code_of(authorize(policy, d, s, None, &v1, Some(&read))),
        code::FEATURE_NOT_NEGOTIATED
    );
    assert_eq!(
        code_of(authorize(policy, d, s, None, &both, Some(&read))),
        "ok"
    );
}

/// R13 P4 task A: the three rows of `desktop_shell.v1` (D1, D2, revision 12c).
#[test]
fn the_desktop_shell_rows_are_exactly_these() {
    let rows = [
        ("fleet.summary", Callers::DeckToStation, Scope::Read),
        ("display.directive", Callers::StationToDeck, Scope::Admin),
        ("station.palette", Callers::StationToDeck, Scope::Admin),
    ];
    for (name, callers, scope) in rows {
        let spec = method(name).expect(name);
        assert_eq!(spec.callers, callers, "{name}");
        assert_eq!(spec.scope, scope, "{name}");
        assert_eq!(spec.feature, Some(feature::DESKTOP_SHELL), "{name}");
        assert!(!spec.mutating, "{name}");
    }
    assert_eq!(feature::DESKTOP_SHELL, "desktop_shell.v1");
    assert!(feature::ALL.contains(&feature::DESKTOP_SHELL));
    assert!(!feature::REQUIRED.contains(&feature::DESKTOP_SHELL));
    let carried = METHODS
        .iter()
        .filter(|s| s.feature == Some(feature::DESKTOP_SHELL))
        .count();
    assert_eq!(carried, 3);
}

/// A peer without `desktop_shell.v1` never gets a call. A station call to the
/// two station-to-deck rows passes the scope check with `admin` (the scope of
/// every paired station) and with no other scope.
#[test]
fn the_desktop_shell_rows_need_the_feature_the_role_and_the_scope() {
    let (d, s) = (Role::Deck, Role::Station);
    let without = negotiated(&[
        feature::CONTROL,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
    ]);
    let admin = scopes(&[Scope::Admin]);
    let read = scopes(&[Scope::Read]);
    let none = scopes(&[]);
    for name in ["display.directive", "station.palette"] {
        assert_eq!(
            code_of(authorize(name, s, d, None, &all(), Some(&admin))),
            "ok",
            "{name}"
        );
        assert_eq!(
            code_of(authorize(name, s, d, None, &without, Some(&admin))),
            code::FEATURE_NOT_NEGOTIATED,
            "{name}"
        );
        // The caller's own pre-check, which knows no grant, also refuses.
        assert_eq!(
            code_of(authorize(name, s, d, None, &without, None)),
            code::FEATURE_NOT_NEGOTIATED,
            "{name}"
        );
        for refused in [&read, &none] {
            assert_eq!(
                code_of(authorize(name, s, d, None, &all(), Some(refused))),
                code::FORBIDDEN,
                "{name}"
            );
        }
        assert_eq!(
            code_of(authorize(name, d, s, None, &all(), Some(&admin))),
            code::WRONG_ROLE,
            "{name}"
        );
    }
    let name = "fleet.summary";
    assert_eq!(
        code_of(authorize(name, d, s, None, &all(), Some(&read))),
        "ok"
    );
    assert_eq!(
        code_of(authorize(name, d, s, None, &without, Some(&read))),
        code::FEATURE_NOT_NEGOTIATED
    );
    assert_eq!(
        code_of(authorize(name, d, s, None, &without, None)),
        code::FEATURE_NOT_NEGOTIATED
    );
    assert_eq!(
        code_of(authorize(name, s, d, None, &all(), Some(&read))),
        code::WRONG_ROLE
    );
    assert_eq!(
        code_of(authorize(name, d, s, None, &all(), Some(&none))),
        code::FORBIDDEN
    );
}

/// R13 P5a task A: the two deck-to-station `admin` rows of the screenshot
/// result lookup and artifact deletion.
#[test]
fn the_p5a_rows_are_admin_deck_to_station_and_never_mutating() {
    for (name, wanted) in [
        ("desktop.capture_status", feature::DESKTOP_CAPTURE),
        ("artifacts.delete", feature::ARTIFACTS_DELETE),
    ] {
        let spec = method(name).expect(name);
        assert_eq!(spec.callers, Callers::DeckToStation, "{name}");
        assert_eq!(spec.scope, Scope::Admin, "{name}");
        assert_eq!(spec.feature, Some(wanted), "{name}");
        assert!(!spec.mutating, "{name}");
        assert!(feature::ALL.contains(&wanted), "{name}");
        assert!(!feature::REQUIRED.contains(&wanted), "{name}");
    }
    assert_eq!(feature::DESKTOP_CAPTURE, "desktop_capture.v1");
    assert_eq!(feature::ARTIFACTS_DELETE, "artifacts_delete.v1");
}

/// B15: the five deck-to-station rows of `mcp_apps.v1`, none mutating.
#[test]
fn app_methods_scopes() {
    for (name, scope) in [
        ("session.app_meta", Scope::Read),
        ("session.app_document", Scope::Read),
        ("session.app_call", Scope::Drive),
        ("session.app_calls", Scope::Read),
        ("session.app_release", Scope::Drive),
    ] {
        let spec = method(name).expect(name);
        assert_eq!(spec.callers, Callers::DeckToStation, "{name}");
        assert_eq!(spec.scope, scope, "{name}");
        assert_eq!(spec.feature, Some(feature::MCP_APPS), "{name}");
        assert_eq!(spec.feature, Some("mcp_apps.v1"), "{name}");
        assert!(!spec.mutating, "{name}");
    }
    assert!(feature::ALL.contains(&feature::MCP_APPS));
    assert!(!feature::REQUIRED.contains(&feature::MCP_APPS));
}
