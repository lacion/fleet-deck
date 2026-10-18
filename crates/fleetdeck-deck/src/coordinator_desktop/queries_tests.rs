//! The status mapping and the `desktop_info` shape (R13 P6 §4.1, §4.8).

use fleetdeck_proto::coordinator_desktop::{
    CoordinatorCapabilities, CoordinatorMonitor, CoordinatorWall,
};
use fleetdeck_proto::desktop::DesktopKind;

use super::*;
use crate::store::coordinator_desktop::{Charges, OpClass};

fn op(state: OpState, tool: OpTool) -> DesktopOp {
    DesktopOp {
        operation_id: "01OP".into(),
        grant_id: "g1".into(),
        request_key: "key".into(),
        payload_hash: "hash".into(),
        tool,
        class: OpClass::Desktop,
        binding_id: "b1".into(),
        generation: 1,
        caller_station: StationId::from("caller"),
        caller_session: "coord-1".into(),
        target_station: StationId::from("target"),
        action_hash: None,
        resource: json!({}),
        state,
        result: None,
        code: None,
        charged: Charges {
            desktop: 1,
            capture: 0,
        },
        created_at: 1,
        updated_at: 2,
        seconds: None,
    }
}

fn status(state: OpState, result: Option<Value>, code: Option<&str>) -> Value {
    let mut row = op(state, OpTool::Notify);
    row.result = result;
    row.code = code.map(str::to_owned);
    serde_json::to_value(status_of("req-1", &row)).expect("json")
}

#[test]
fn a_prepared_or_attempted_operation_is_pending() {
    for state in [OpState::Prepared, OpState::Attempted] {
        assert_eq!(
            status(state, None, None),
            json!({"request_id": "req-1", "operation_id": "01OP", "station": "target",
                   "state": "pending"})
        );
    }
}

#[test]
fn a_done_operation_returns_its_result_and_an_unknown_one_returns_no_result() {
    assert_eq!(
        status(OpState::Done, Some(json!({"window_id": "w1"})), None),
        json!({"request_id": "req-1", "operation_id": "01OP", "station": "target",
               "state": "done", "result": {"window_id": "w1"}})
    );
    assert_eq!(
        status(OpState::Unknown, None, None),
        json!({"request_id": "req-1", "operation_id": "01OP", "station": "target",
               "state": "unknown"})
    );
}

#[test]
fn a_failed_operation_returns_its_code_with_the_fixed_text() {
    assert_eq!(
        status(OpState::Failed, None, Some("authority_lost")),
        json!({"request_id": "req-1", "operation_id": "01OP", "station": "target",
               "state": "failed", "error": "authority_lost",
               "message": "Desktop authority was withdrawn before this operation could continue."})
    );
}

#[test]
fn a_request_without_an_operation_is_unknown_or_the_stored_budget_refusal() {
    assert_eq!(
        serde_json::to_value(status_without_op("req-1", None)).expect("json"),
        json!({"request_id": "req-1", "state": "unknown"})
    );
    assert_eq!(
        serde_json::to_value(status_without_op("req-1", Some("desktop_cap_reached")))
            .expect("json"),
        json!({"request_id": "req-1", "state": "failed", "error": "desktop_cap_reached",
               "message": "This grant has used its desktop action limit."})
    );
}

fn monitor(name: &str, capture_allowed: bool) -> CoordinatorMonitor {
    CoordinatorMonitor {
        name: name.into(),
        width: 1920,
        height: 1080,
        scale_pct: 100,
        focused: false,
        dpms_on: Some(true),
        capture_allowed,
    }
}

fn info() -> CoordinatorDesktopInfo {
    CoordinatorDesktopInfo {
        station: String::new(),
        kind: DesktopKind::Omarchy,
        monitors: vec![monitor("DP-1", true), monitor("DP-2", false)],
        locked: Some(false),
        capabilities: CoordinatorCapabilities {
            notify: true,
            preview: true,
            screenshot: true,
        },
        walls: vec![CoordinatorWall {
            display_id: "d1".into(),
            monitor: "DP-1".into(),
        }],
    }
}

#[test]
fn the_station_id_is_the_target_and_a_grant_with_both_classes_sees_everything() {
    let v = shape_info(info(), &StationId::from("target"), (true, true)).expect("a result");
    assert_eq!(v["station"], "target");
    assert_eq!(v["monitors"].as_array().expect("monitors").len(), 2);
    assert_eq!(v["walls"].as_array().expect("walls").len(), 1);
    assert_eq!(
        v["capabilities"],
        json!({"notify": true, "preview": true, "screenshot": true})
    );
}

#[test]
fn a_capture_only_grant_sees_the_capture_monitors_and_no_walls_or_desktop_capabilities() {
    let v = shape_info(info(), &StationId::from("target"), (false, true)).expect("a result");
    let monitors = v["monitors"].as_array().expect("monitors");
    assert_eq!(monitors.len(), 1);
    assert_eq!(monitors[0]["name"], "DP-1");
    assert_eq!(v["walls"], json!([]));
    assert_eq!(
        v["capabilities"],
        json!({"notify": false, "preview": false, "screenshot": true})
    );
}

#[test]
fn a_desktop_only_grant_has_no_screenshot_capability() {
    let v = shape_info(info(), &StationId::from("target"), (true, false)).expect("a result");
    assert_eq!(
        v["capabilities"],
        json!({"notify": true, "preview": true, "screenshot": false})
    );
    assert_eq!(v["monitors"].as_array().expect("monitors").len(), 2);
}

#[test]
fn the_limits_of_the_result_are_enforced_again() {
    let mut many = info();
    many.monitors = (0..65).map(|i| monitor(&format!("M-{i}"), true)).collect();
    assert_eq!(
        shape_info(many, &StationId::from("t"), (true, true)).err(),
        Some("desktop_result_too_large")
    );
    let mut ok = info();
    ok.monitors = (0..64).map(|i| monitor(&format!("M-{i}"), true)).collect();
    assert!(shape_info(ok, &StationId::from("t"), (true, true)).is_ok());
    let mut walls = info();
    walls.walls = (0..65)
        .map(|i| CoordinatorWall {
            display_id: format!("d{i}"),
            monitor: "DP-1".into(),
        })
        .collect();
    assert_eq!(
        shape_info(walls, &StationId::from("t"), (true, true)).err(),
        Some("desktop_result_too_large")
    );
    // Over 16 KiB serialized, with few entries.
    let mut big = info();
    big.monitors = vec![monitor(&"m".repeat(17_000), true)];
    assert_eq!(
        shape_info(big, &StationId::from("t"), (true, true)).err(),
        Some("desktop_result_too_large")
    );
}
