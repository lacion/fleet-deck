//! The pure tests of the provider update routes: the body, the error table,
//! the audit rows and the method rows. The tests through HTTP, with a fake
//! station, are in `tests/cli_update.rs`.

use axum::http::{Method, StatusCode};
use fleetdeck_proto::cli_update::{CliUpdateRunAnswer, InstallMethod, ProviderUpdateState};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{Scope, WireError};

use super::*;
use crate::http::methods;

fn body(check_only: bool, force: bool) -> RunBody {
    RunBody {
        provider: ProviderKind::Claude,
        check_only,
        force,
        elevation: None,
    }
}

fn answer(refused: Option<&str>) -> CliUpdateRunAnswer {
    CliUpdateRunAnswer {
        state: ProviderUpdateState {
            provider: ProviderKind::Claude,
            method: InstallMethod::Native,
            updatable: true,
            blocked: None,
            channel: "stable".to_owned(),
            installed: None,
            available: None,
            checked_at: None,
            last: None,
            quarantined: None,
        },
        attempt_id: None,
        refused: refused.map(str::to_owned),
        busy: Vec::new(),
    }
}

#[test]
fn the_body_has_a_provider_and_defaults_for_the_rest() {
    let b: RunBody = serde_json::from_value(json!({ "provider": "codex" })).unwrap();
    assert_eq!(b.provider, ProviderKind::Codex);
    assert!(!b.check_only && !b.force && b.elevation.is_none());
    let b: RunBody = serde_json::from_value(
        json!({ "provider": "claude", "check_only": true, "force": true, "elevation": "t" }),
    )
    .unwrap();
    assert!(b.check_only && b.force);
    assert_eq!(b.elevation.as_deref(), Some("t"));
}

#[test]
fn the_body_refuses_an_unknown_field_and_a_missing_provider() {
    assert!(serde_json::from_value::<RunBody>(json!({ "provider": "claude", "x": 1 })).is_err());
    assert!(serde_json::from_value::<RunBody>(json!({ "check_only": true })).is_err());
}

#[test]
fn the_elevation_error_names_the_update_key() {
    let e = elevation_required();
    assert_eq!(
        (e.status, e.code),
        (StatusCode::BAD_REQUEST, "elevation_required")
    );
    assert_eq!(e.details.unwrap()["keys"], json!(["update"]));
}

#[test]
fn a_station_error_has_a_fixed_text() {
    for code in ["unsupported", "unknown_method"] {
        let e = station_error(&WireError::new(code, "secret text"));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::NOT_IMPLEMENTED, "cli_update_unsupported")
        );
        assert!(!e.message.contains("secret"));
    }
    let e = station_error(&WireError::new("internal", "secret text"));
    assert_eq!(
        (e.status, e.code),
        (StatusCode::BAD_GATEWAY, "station_error")
    );
    assert!(!e.message.contains("secret"));
}

#[test]
fn the_audit_row_of_an_ok_check_and_an_ok_update() {
    let row = run_audit("Lighting", &body(true, false), &Ok(answer(None)));
    assert_eq!(row.action, "cli_update.check");
    assert_eq!(row.actor, "owner");
    assert_eq!(row.target.as_deref(), Some("Lighting/claude"));
    assert_eq!(row.result, AuditResult::Ok);
    assert_eq!(row.detail, None);
    let row = run_audit("Lighting", &body(false, false), &Ok(answer(None)));
    assert_eq!(row.action, "cli_update.update");
    assert_eq!(row.result, AuditResult::Ok);
}

#[test]
fn the_audit_row_of_a_refused_answer_has_the_reason_code() {
    let row = run_audit(
        "Lighting",
        &body(false, false),
        &Ok(answer(Some("sessions_running"))),
    );
    assert_eq!(row.result, AuditResult::Refused);
    assert_eq!(row.detail.as_deref(), Some("sessions_running"));
}

#[test]
fn the_audit_row_names_force() {
    let row = run_audit("Lighting", &body(false, true), &Ok(answer(None)));
    assert_eq!(row.detail.as_deref(), Some("force"));
    let row = run_audit(
        "Lighting",
        &body(false, true),
        &Ok(answer(Some("another_update"))),
    );
    assert_eq!(row.detail.as_deref(), Some("another_update force"));
}

#[test]
fn the_audit_row_of_an_error_has_its_code() {
    let e = ApiError::new(StatusCode::CONFLICT, "station_unreachable", "x");
    let row = run_audit("Lighting", &body(true, false), &Err(e));
    assert_eq!(row.result, AuditResult::Failed);
    assert_eq!(row.detail.as_deref(), Some("station_unreachable"));
    let row = run_audit("Lighting", &body(false, false), &Err(elevation_required()));
    assert_eq!(row.result, AuditResult::Refused);
    assert_eq!(row.detail.as_deref(), Some("elevation_required"));
}

#[test]
fn the_method_rows_are_admin_and_only_the_post_mutates() {
    let path = "/api/stations/{station}/cli-update";
    let get = methods::find(&Method::GET, path).unwrap();
    assert_eq!(get.name, "station.cli_update_status");
    assert_eq!((get.scope, get.mutating), (Some(Scope::Admin), false));
    assert!(!get.member && !get.display);
    let post = methods::find(&Method::POST, path).unwrap();
    assert_eq!(post.name, "station.cli_update_run");
    assert_eq!((post.scope, post.mutating), (Some(Scope::Admin), true));
    assert!(!post.member && !post.display);
}
