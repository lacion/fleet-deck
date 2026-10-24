//! The wire forms of the Fleet Git and artifact events (split from
//! `event_tests.rs` for the module size rule).

use super::*;
use serde_json::json;

#[test]
fn the_fleet_git_events_have_their_wire_names_and_optional_fields() {
    let push = serde_json::to_value(Event::SessionPush {
        state: crate::fleet_git::PushState::Pushed,
        ref_name: "fd/a/b".into(),
        oid: Some("abc".into()),
        desired_oid: None,
        stop_code: None,
        attempts: 0,
        next_attempt_at: None,
        code: None,
        text: None,
    })
    .unwrap();
    assert_eq!(
        push,
        json!({"kind": "session.push", "state": "pushed", "ref_name": "fd/a/b", "oid": "abc", "attempts": 0})
    );
    // An event of a newer station with an unknown push state still decodes.
    let newer: EventBody = serde_json::from_value(
        json!({"kind": "session.push", "state": "paused", "ref_name": "r", "attempts": 1}),
    )
    .unwrap();
    assert!(matches!(
        newer.known(),
        Some(Event::SessionPush {
            state: crate::fleet_git::PushState::Unknown,
            ..
        })
    ));
    let stale: EventBody = serde_json::from_value(
        json!({"kind": "workspace.base_stale", "code": "fetch_timeout", "text": "t"}),
    )
    .unwrap();
    assert_eq!(stale.kind_name(), "workspace.base_stale");
    assert!(stale.known().is_some());
}

#[test]
fn the_artifact_events_have_their_wire_names_and_optional_fields() {
    let ready = serde_json::to_value(Event::ArtifactsReady {
        path: None,
        url: Some("http://100.100.100.100:8080/t/nas/art/app/ada/".into()),
    })
    .unwrap();
    assert_eq!(
        ready,
        json!({"kind": "artifacts.ready", "url": "http://100.100.100.100:8080/t/nas/art/app/ada/"})
    );
    for (kind, name) in [
        ("artifacts.unavailable", "mount_missing"),
        ("artifacts.revoked", "share_unverified"),
    ] {
        let body: EventBody =
            serde_json::from_value(json!({"kind": kind, "code": name, "text": "t"})).unwrap();
        assert_eq!(body.kind_name(), kind);
        assert!(body.known().is_some());
    }
    // A bare `artifacts.ready` decodes (both fields are optional).
    let bare: EventBody = serde_json::from_value(json!({"kind": "artifacts.ready"})).unwrap();
    assert!(matches!(
        bare.known(),
        Some(Event::ArtifactsReady {
            path: None,
            url: None
        })
    ));
}
