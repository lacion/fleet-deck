//! The start checks of a coordinator spec (ADR-006 §4, §5.4, §8.2 test 12;
//! DL-215).

#![allow(clippy::unwrap_used)]

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::event::{ProviderKind, RunOrigin};

use super::coordinator_command::check_spec;
use super::tests::coordinator_spec;
use crate::store::DecideError;

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
        let (got, _) = code(check_spec(origin, ProviderKind::Claude, &spec, import));
        assert_eq!(got, want, "{origin:?} import={import}");
    }
    let mut empty = coordinator_spec();
    empty.classes.clear();
    let (got, _) = code(check_spec(
        RunOrigin::Board,
        ProviderKind::Claude,
        &empty,
        false,
    ));
    assert_eq!(got, RejectCode::InvalidCommand);
}
