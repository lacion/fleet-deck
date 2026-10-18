//! `desktop.policy_now` and `desktop.coordinator_read` of the station link
//! (R13 P6, feature `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §5.5,
//! §5.7). Task F1 only routes them: each refuses with `desktop_unsupported`
//! until the station target task (T2) gives it behavior.

use fleetdeck_proto::coordinator_desktop_text::{code, text};
use fleetdeck_proto::link::{Outcome, WireError};

/// The refusal of both stubs: the code and the fixed text of §11.2.
pub(super) fn unsupported() -> Outcome {
    Outcome::Err(WireError::new(
        code::DESKTOP_UNSUPPORTED,
        text(code::DESKTOP_UNSUPPORTED).unwrap_or(code::DESKTOP_UNSUPPORTED),
    ))
}

/// `desktop.policy_now` (§5.5): a stub.
pub(super) fn policy_now(_params: serde_json::Value) -> Outcome {
    unsupported()
}

/// `desktop.coordinator_read` (§5.7): a stub.
pub(super) fn coordinator_read(_params: serde_json::Value) -> Outcome {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_stubs_refuse_with_the_fixed_code_and_text() {
        for outcome in [
            policy_now(serde_json::json!({"purpose": "desktop", "monitor": null})),
            coordinator_read(serde_json::json!({"read_id": "r"})),
        ] {
            let Outcome::Err(e) = outcome else {
                panic!("a stub must refuse");
            };
            assert_eq!(e.code, "desktop_unsupported");
            assert_eq!(e.message, "This station's build has no desktop control.");
        }
    }
}
