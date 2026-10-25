//! The replies and the code table of the handlers (R13 P6 §11).

use super::*;

#[test]
fn the_refusal_of_a_later_task_has_the_code_and_the_fixed_text() {
    let Outcome::Err(e) = unsupported() else {
        panic!("a stub must refuse");
    };
    assert_eq!(e.code, "desktop_unsupported");
    assert_eq!(e.message, "This station's build has no desktop control.");
}

#[test]
fn a_malformed_envelope_is_the_wire_error_invalid_params() {
    let Outcome::Err(e) = invalid_params() else {
        panic!("a wire error");
    };
    assert_eq!(e.code, "invalid_params");
    assert_eq!(e.message, "The desktop tool arguments are not valid.");
}

#[test]
fn a_reply_is_the_desktop_reply_with_the_fixed_text_of_the_code() {
    let Outcome::Ok(v) = reply_err("authority_lost", Some("op1")) else {
        panic!("a reply");
    };
    assert_eq!(
        v,
        serde_json::json!({
            "status": "err",
            "error": "authority_lost",
            "message": "Desktop authority was withdrawn before this operation could continue.",
            "operation_id": "op1",
        })
    );
    let Outcome::Ok(v) = reply_err("busy", None) else {
        panic!("a reply");
    };
    assert!(v.get("operation_id").is_none(), "{v}");
    let Outcome::Ok(v) = reply_ok(serde_json::json!({"window_id": "w"})) else {
        panic!("a reply");
    };
    assert_eq!(
        v,
        serde_json::json!({"status": "ok", "result": {"window_id": "w"}})
    );
}

#[test]
fn every_code_of_the_table_has_a_text_and_no_other_text_passes() {
    let mut seen = std::collections::BTreeSet::new();
    for c in CODES {
        assert!(text(c).is_some(), "{c}");
        assert!(seen.insert(c), "{c} twice");
        assert_eq!(known_code(c), Some(c));
    }
    for other in ["", "nope", "forbidden", "unsupported ", "failed"] {
        assert_eq!(known_code(other), None, "{other}");
    }
}

#[test]
fn the_new_p6_state_starts_with_no_slot_and_no_permit() {
    let shared = Shared::default();
    assert_eq!(shared.slots().in_use(), 0);
    assert_eq!(shared.permits().len(), 0);
}
