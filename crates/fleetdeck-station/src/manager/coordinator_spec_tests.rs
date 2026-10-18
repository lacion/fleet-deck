//! The start checks of a coordinator spec (ADR-006 §4, §5.4, §8.2 test 12;
//! DL-215).

#![allow(clippy::unwrap_used)]

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::event::{ProviderKind, RunOrigin};

use super::coordinator_command::check_spec;
use crate::coordinator::Bindings;
use crate::store::DecideError;

/// A coordinator spec for the start checks (ADR-006 §4).
pub(super) fn coordinator_spec() -> fleetdeck_proto::command::CoordinatorSpec {
    fleetdeck_proto::command::CoordinatorSpec {
        grant_id: "grant-1".to_owned(),
        classes: vec!["read".to_owned()],
        idle_ttl_ms: 60_000,
        expires_at: 4_102_444_800_000,
        control_skill: fleetdeck_proto::command::ControlSkill {
            version: "1".to_owned(),
            text: "You coordinate the fleet.".to_owned(),
        },
        scope: fleetdeck_proto::command::GrantScope::Lineage,
        answer_cap: None,
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        allow_recording: false,
        recording_cap: None,
        recording_second_cap: None,
        stations: Vec::new(),
    }
}

fn code(r: Result<(), DecideError>) -> (RejectCode, String) {
    match r {
        Err(DecideError::Reject(reject)) => (reject.code, reject.message),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// §8.2 test 12: while no live smoke has verified the handoff, an owner
/// start with a grant is refused `unsupported` with the capability reason,
/// for every provider.
#[test]
fn a_coordinator_spec_for_an_unverified_handoff_is_refused_unsupported() {
    for provider in [ProviderKind::Claude, ProviderKind::Codex] {
        let (code, message) = code(check_spec(
            RunOrigin::Board,
            provider,
            &coordinator_spec(),
            false,
            &Bindings::default(),
        ));
        assert_eq!(code, RejectCode::Unsupported);
        assert_eq!(message, fleetdeck_providers::coordinator::UNVERIFIED);
    }
}

/// Only an owner start may carry a grant: a coordinator `spawn` with a
/// grant is `forbidden` (§4), a local or import start and an incomplete
/// spec are `invalid_command`.
#[test]
fn only_an_owner_start_may_carry_a_complete_coordinator_spec() {
    let spec = coordinator_spec();
    let cases = [
        (RunOrigin::Coordinator, false, RejectCode::Forbidden),
        (RunOrigin::Mail, false, RejectCode::Forbidden),
        (RunOrigin::Local, false, RejectCode::InvalidCommand),
        (RunOrigin::Board, true, RejectCode::InvalidCommand),
    ];
    for (origin, import, want) in cases {
        let (got, _) = code(check_spec(
            origin,
            ProviderKind::Claude,
            &spec,
            import,
            &Bindings::default(),
        ));
        assert_eq!(got, want, "{origin:?} import={import}");
    }
    let mut empty = coordinator_spec();
    empty.classes.clear();
    let (got, _) = code(check_spec(
        RunOrigin::Board,
        ProviderKind::Claude,
        &empty,
        false,
        &Bindings::default(),
    ));
    assert_eq!(got, RejectCode::InvalidCommand);
}

/// DL-219: the handoff check uses the probed versions of the provider
/// family; every probed instance needs a verified version, and an
/// unprobed family stays `handoff_unverified`.
#[test]
fn the_handoff_check_needs_a_verified_probed_version_of_every_instance() {
    let bindings = Bindings::default();
    let spec = coordinator_spec();
    let start = |b: &Bindings, p| check_spec(RunOrigin::Board, p, &spec, false, b);
    assert_eq!(
        start(&bindings, ProviderKind::Claude).map_err(|_| ()),
        Err(())
    );
    bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
    bindings.record_version(ProviderKind::Codex, "0.159.0".to_owned());
    assert!(start(&bindings, ProviderKind::Claude).is_ok());
    let (c, m) = code(start(&bindings, ProviderKind::Codex));
    assert_eq!(
        (c, m.as_str()),
        (RejectCode::Unsupported, "handoff_unverified")
    );
    // DL-220: Codex from 0.160.0 is verified; every probed instance counts.
    bindings.record_version(ProviderKind::Codex, "0.160.0".to_owned());
    let (c, _) = code(start(&bindings, ProviderKind::Codex));
    assert_eq!(
        c,
        RejectCode::Unsupported,
        "the 0.159.0 instance still blocks"
    );
    bindings.record_version(ProviderKind::Claude, "2.1.0".to_owned());
    let (c, _) = code(start(&bindings, ProviderKind::Claude));
    assert_eq!(c, RejectCode::Unsupported);
}
