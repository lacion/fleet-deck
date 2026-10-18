//! The P6 rows of the method table (R13 P6 §5.2, feature `desktop_mcp.v1`).
//! The table agreement with the ADR is in `method_table.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_link::methods::{Callers, METHODS, method};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{Holder, envelope_of};

#[test]
fn the_five_p6_rows_have_their_direction_scope_feature_and_mutating_flag() {
    for (name, callers, scope, mutating) in [
        (
            "coordinator.desktop_call",
            Callers::StationToDeck,
            Scope::Coordinate,
            true,
        ),
        (
            "coordinator.desktop_query",
            Callers::StationToDeck,
            Scope::Coordinate,
            false,
        ),
        (
            "desktop.authority_check",
            Callers::StationToDeck,
            Scope::Coordinate,
            false,
        ),
        (
            "desktop.policy_now",
            Callers::DeckToStation,
            Scope::Read,
            false,
        ),
        (
            "desktop.coordinator_read",
            Callers::DeckToStation,
            Scope::Read,
            false,
        ),
    ] {
        let spec = method(name).unwrap_or_else(|| panic!("{name} is not in the table"));
        assert_eq!(spec.callers, callers, "{name}");
        assert_eq!(spec.scope, scope, "{name}");
        assert_eq!(spec.feature, Some("desktop_mcp.v1"), "{name}");
        assert_eq!(spec.mutating, mutating, "{name}");
    }
}

#[test]
fn only_the_call_has_an_envelope_and_the_old_coordinator_rows_stay() {
    assert_eq!(
        envelope_of("coordinator.desktop_call").map(|e| (e.params, e.holder)),
        Some(("CoordinatorDesktopCallEnvelope", Holder::Deck))
    );
    for name in [
        "coordinator.desktop_query",
        "desktop.authority_check",
        "desktop.policy_now",
        "desktop.coordinator_read",
    ] {
        assert!(envelope_of(name).is_none(), "{name}");
    }
    let call = method("coordinator.call").unwrap();
    assert_eq!(call.feature, Some("coordinator.v1"));
    assert!(call.mutating);
    assert_eq!(
        method("coordinator.authority_check").unwrap().feature,
        Some("coordinator.v2")
    );
    // The names are unique in the table.
    let mut names: Vec<&str> = METHODS.iter().map(|m| m.name).collect();
    names.sort_unstable();
    let total = names.len();
    names.dedup();
    assert_eq!(names.len(), total);
}
