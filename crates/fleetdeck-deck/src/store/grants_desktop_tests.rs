//! The desktop classes and caps of a grant (R13 P6 §2.1, §8.1): the class
//! names, the fleet-control and mutating rules, the defaults and the cap
//! rules of `create_grant`, and the columns that the store reads back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_testkit::Det;

use super::*;

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn new_grant(classes: &[GrantClass]) -> NewGrant {
    let mut g = NewGrant::new(StationId::from("alpha"), SessionId::from("coord"));
    g.classes = classes.iter().copied().collect();
    g
}

async fn invalid(s: &DeckStore, g: NewGrant) -> String {
    match s.create_grant(g).await {
        Err(DeckError::Invalid(m)) => m,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn the_new_classes_have_their_names_and_are_fleet_control_and_mutating() {
    for (class, name) in [
        (GrantClass::Desktop, "desktop"),
        (GrantClass::Capture, "capture"),
    ] {
        assert_eq!(serde_json::to_value(class).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<GrantClass>(serde_json::json!(name)).unwrap(),
            class
        );
        assert!(class.is_fleet_control());
        assert!(class.is_mutating());
    }
    // The old classes keep their rules, and an unknown name fails closed.
    assert!(!GrantClass::Read.is_fleet_control());
    assert!(!GrantClass::Read.is_mutating());
    assert!(GrantClass::Inspect.is_fleet_control());
    assert!(!GrantClass::Inspect.is_mutating());
    assert!(serde_json::from_value::<GrantClass>(serde_json::json!("input")).is_err());
}

#[tokio::test]
async fn a_grant_with_desktop_or_capture_has_the_defaults_of_the_contract() {
    let s = store().await;
    let desktop = s
        .create_grant(new_grant(&[GrantClass::Desktop]))
        .await
        .unwrap();
    assert!(desktop.is_fleet_control());
    assert_eq!(
        (
            desktop.desktop_cap,
            desktop.capture_cap,
            desktop.image_byte_cap
        ),
        (Some(300), None, None)
    );
    assert_eq!(
        (
            desktop.desktop_used,
            desktop.capture_used,
            desktop.image_byte_used,
            desktop.desktop_attempts
        ),
        (0, 0, 0, 0)
    );
    let mut capture = new_grant(&[GrantClass::Capture]);
    capture.session_id = SessionId::from("coord-2");
    let capture = s.create_grant(capture).await.unwrap();
    assert_eq!(
        (
            capture.desktop_cap,
            capture.capture_cap,
            capture.image_byte_cap
        ),
        (Some(300), Some(20), Some(67_108_864))
    );
    // The constants of `limits` are the defaults (no second copy).
    assert_eq!(limits::DESKTOP_CAP_DEFAULT, 300);
    assert_eq!(limits::CAPTURE_CAP_DEFAULT, 20);
    assert_eq!(limits::IMAGE_BYTE_CAP_DEFAULT, 67_108_864);
    // The row reads back the same.
    let back = s.grant(capture.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(back, capture);
    // A grant without the classes has no cap.
    let mut plain = new_grant(&[GrantClass::Read]);
    plain.session_id = SessionId::from("coord-3");
    let plain = s.create_grant(plain).await.unwrap();
    assert_eq!(
        (plain.desktop_cap, plain.capture_cap, plain.image_byte_cap),
        (None, None, None)
    );
}

#[tokio::test]
async fn given_caps_are_kept_at_their_limits() {
    let s = store().await;
    let mut g = new_grant(&[GrantClass::Desktop, GrantClass::Capture]);
    g.desktop_cap = Some(5000);
    g.capture_cap = Some(0);
    g.image_byte_cap = Some(1_073_741_824);
    let g = s.create_grant(g).await.unwrap();
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (Some(5000), Some(0), Some(1_073_741_824))
    );
    let mut zero = new_grant(&[GrantClass::Capture]);
    zero.session_id = SessionId::from("coord-2");
    zero.desktop_cap = Some(0);
    zero.image_byte_cap = Some(0);
    let zero = s.create_grant(zero).await.unwrap();
    assert_eq!(
        (zero.desktop_cap, zero.capture_cap, zero.image_byte_cap),
        (Some(0), Some(20), Some(0))
    );
}

#[tokio::test]
async fn the_cap_rules_refuse_with_the_fixed_texts_in_the_order_class_range_multiple() {
    let s = store().await;
    let with = |classes: &[GrantClass], edit: fn(&mut NewGrant)| {
        let mut g = new_grant(classes);
        edit(&mut g);
        g
    };
    let class_desktop = "desktop_cap needs the desktop or capture class";
    let class_capture = "capture_cap and image_byte_cap need the capture class";
    assert_eq!(
        invalid(&s, with(&[GrantClass::Read], |g| g.desktop_cap = Some(1))).await,
        class_desktop
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Inspect], |g| g.desktop_cap = Some(1))
        )
        .await,
        class_desktop
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Desktop], |g| g.capture_cap = Some(1))
        )
        .await,
        class_capture
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Desktop], |g| g.image_byte_cap =
                Some(1_048_576))
        )
        .await,
        class_capture
    );
    // The class rule comes before the range.
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Read], |g| g.desktop_cap = Some(9999))
        )
        .await,
        class_desktop
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Desktop], |g| {
                g.capture_cap = Some(9999);
                g.image_byte_cap = Some(1);
            })
        )
        .await,
        class_capture
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Desktop], |g| g.desktop_cap = Some(5001))
        )
        .await,
        "desktop_cap is 0 to 5000"
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Capture], |g| g.capture_cap = Some(5001))
        )
        .await,
        "capture_cap is 0 to 5000"
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Capture], |g| {
                g.image_byte_cap = Some(1_073_741_825);
            })
        )
        .await,
        "image_byte_cap is 0 to 1073741824"
    );
    // The range comes before the multiple: above the maximum and not a
    // multiple is a range error.
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Capture], |g| {
                g.image_byte_cap = Some(1_073_741_826);
            })
        )
        .await,
        "image_byte_cap is 0 to 1073741824"
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Capture], |g| g.image_byte_cap =
                Some(1_048_577))
        )
        .await,
        "image_byte_cap is a multiple of 1048576"
    );
    assert!(s.grants(10).await.unwrap().is_empty());
}

#[test]
fn the_cap_check_names_no_value() {
    let classes = BTreeSet::from([GrantClass::Read]);
    let err = check_desktop_caps(&classes, Some(123_456), None, None).unwrap_err();
    assert!(!err.contains("123456"));
}

#[tokio::test]
async fn a_grant_has_zero_recording_caps_unless_it_asks_and_the_row_reads_back() {
    let s = store().await;
    // The default of `NewGrant` and a screenshot-only grant: zero.
    assert_eq!(
        (
            new_grant(&[GrantClass::Capture]).recording_cap,
            new_grant(&[GrantClass::Capture]).recording_second_cap
        ),
        (0, 0)
    );
    let shot = s
        .create_grant(new_grant(&[GrantClass::Capture]))
        .await
        .unwrap();
    assert_eq!(
        (
            shot.recording_cap,
            shot.recording_used,
            shot.recording_second_cap,
            shot.recording_second_used
        ),
        (0, 0, 0, 0)
    );
    // A recording grant keeps its caps, and its counters start at zero.
    let mut rec = new_grant(&[GrantClass::Capture]);
    rec.session_id = SessionId::from("coord-2");
    rec.recording_cap = limits::RECORDING_CAP_MAX;
    rec.recording_second_cap = limits::RECORDING_SECOND_CAP_MAX;
    let rec = s.create_grant(rec).await.unwrap();
    assert_eq!(
        (
            rec.recording_cap,
            rec.recording_used,
            rec.recording_second_cap,
            rec.recording_second_used
        ),
        (5000, 0, 600_000, 0)
    );
    assert_eq!(s.grant(rec.grant_id.clone()).await.unwrap().unwrap(), rec);
}

#[tokio::test]
async fn the_recording_cap_rules_refuse_with_the_fixed_texts_class_first_then_range() {
    let s = store().await;
    let with = |classes: &[GrantClass], edit: fn(&mut NewGrant)| {
        let mut g = new_grant(classes);
        edit(&mut g);
        g
    };
    let class = "recording caps need the capture class";
    for classes in [
        &[GrantClass::Read][..],
        &[GrantClass::Desktop][..],
        &[GrantClass::Inspect][..],
    ] {
        assert_eq!(
            invalid(&s, with(classes, |g| g.recording_cap = 1)).await,
            class
        );
        assert_eq!(
            invalid(&s, with(classes, |g| g.recording_second_cap = 1)).await,
            class
        );
    }
    // The class rule comes before the range.
    assert_eq!(
        invalid(&s, with(&[GrantClass::Desktop], |g| g.recording_cap = 9999)).await,
        class
    );
    assert_eq!(
        invalid(&s, with(&[GrantClass::Capture], |g| g.recording_cap = 5001)).await,
        "recording_cap is 0 to 5000"
    );
    assert_eq!(
        invalid(
            &s,
            with(&[GrantClass::Capture], |g| g.recording_second_cap = 600_001)
        )
        .await,
        "recording_second_cap is 0 to 600000"
    );
    assert!(s.grants(10).await.unwrap().is_empty());
}

#[test]
fn the_recording_cap_check_names_no_value() {
    let classes = BTreeSet::from([GrantClass::Capture]);
    let err = check_recording_caps(&classes, 123_456, 0).unwrap_err();
    assert!(!err.contains("123456"));
    assert_eq!(check_recording_caps(&classes, 0, 0), Ok(()));
    assert_eq!(check_recording_caps(&classes, 5000, 600_000), Ok(()));
}
