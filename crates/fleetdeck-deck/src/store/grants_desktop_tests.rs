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
        // R13 P7 §3.1: the input class is fleet control and mutating too.
        (GrantClass::Input, "input"),
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
    for unknown in ["input_enabled", "Input", "keyboard"] {
        assert!(serde_json::from_value::<GrantClass>(serde_json::json!(unknown)).is_err());
    }
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

const INPUT_CLASSES: [GrantClass; 3] =
    [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

fn input_grant(edit: fn(&mut NewGrant)) -> NewGrant {
    let mut g = new_grant(&INPUT_CLASSES);
    g.allow_input = true;
    edit(&mut g);
    g
}

#[tokio::test]
async fn a_grant_has_no_input_allowance_unless_it_asks_and_the_row_reads_back() {
    let s = store().await;
    // The default of `NewGrant`, a grant with the input class but no
    // allowance, and an older-style grant: input disabled, zero caps.
    let none = NewGrant::new(StationId::from("alpha"), SessionId::from("coord"));
    assert_eq!(
        (
            none.allow_input,
            none.input_scope,
            none.input_cap,
            none.input_character_cap
        ),
        (false, None, 0, 0)
    );
    let classed = s.create_grant(new_grant(&INPUT_CLASSES)).await.unwrap();
    let plain = new_grant(&[GrantClass::Desktop]);
    let plain = s
        .create_grant(NewGrant {
            session_id: SessionId::from("c2"),
            ..plain
        })
        .await
        .unwrap();
    for g in [&classed, &plain] {
        assert!(!g.allow_input && g.input_scope.is_none(), "{g:?}");
        assert_eq!(
            (
                g.input_cap,
                g.input_used,
                g.input_character_cap,
                g.input_character_used
            ),
            (0, 0, 0, 0)
        );
    }
    assert!(classed.is_fleet_control());
    // An opted-in grant gets the scope `own_windows`, keeps its caps and
    // starts its counters at zero; the row reads back as stored.
    let mut g = input_grant(|g| {
        g.session_id = SessionId::from("c3");
        g.input_cap = limits::INPUT_CAP_MAX;
        g.input_character_cap = limits::INPUT_CHARACTER_CAP_MAX;
    });
    let on = s.create_grant(g.clone()).await.unwrap();
    assert!(on.allow_input);
    assert_eq!(on.input_scope, Some(InputScope::OwnWindows));
    assert_eq!(
        (
            on.input_cap,
            on.input_used,
            on.input_character_cap,
            on.input_character_used
        ),
        (5000, 0, 1_000_000, 0)
    );
    assert_eq!(s.grant(on.grant_id.clone()).await.unwrap().unwrap(), on);
    // Caps of zero are an allowance with no new accepted calls, not "none".
    g.session_id = SessionId::from("c4");
    g.input_cap = 0;
    g.input_character_cap = 0;
    let zero = s.create_grant(g).await.unwrap();
    assert!(zero.allow_input);
    assert_eq!((zero.input_cap, zero.input_character_cap), (0, 0));
}

#[tokio::test]
async fn the_input_rules_refuse_with_the_fixed_texts_class_first_then_the_opt_in_then_range() {
    let s = store().await;
    let classes = "allow_input needs the desktop, capture and input classes";
    for missing in INPUT_CLASSES {
        let mut g = input_grant(|_| {});
        g.classes.remove(&missing);
        assert_eq!(invalid(&s, g).await, classes, "{missing:?}");
    }
    let mut read = input_grant(|_| {});
    read.classes = BTreeSet::from([GrantClass::Read]);
    assert_eq!(invalid(&s, read).await, classes);
    let off = |edit: fn(&mut NewGrant)| {
        let mut g = new_grant(&INPUT_CLASSES);
        edit(&mut g);
        g
    };
    assert_eq!(
        invalid(&s, off(|g| g.input_scope = Some(InputScope::OwnWindows))).await,
        "input_scope needs allow_input"
    );
    assert_eq!(
        invalid(&s, off(|g| g.input_cap = 1)).await,
        "input caps need allow_input"
    );
    assert_eq!(
        invalid(&s, off(|g| g.input_character_cap = 1)).await,
        "input caps need allow_input"
    );
    // The class rule comes before the others, the opt-in rule before the range.
    let mut g = new_grant(&[GrantClass::Read]);
    g.allow_input = true;
    g.input_cap = 9999;
    assert_eq!(invalid(&s, g).await, classes);
    assert_eq!(
        invalid(&s, off(|g| g.input_cap = 9999)).await,
        "input caps need allow_input"
    );
    assert_eq!(
        invalid(&s, input_grant(|g| g.input_cap = 5001)).await,
        "input_cap is 0 to 5000"
    );
    assert_eq!(
        invalid(&s, input_grant(|g| g.input_character_cap = 1_000_001)).await,
        "input_character_cap is 0 to 1000000"
    );
    assert!(s.grants(10).await.unwrap().is_empty());
}

#[test]
fn the_input_check_names_no_value() {
    let classes = BTreeSet::from(INPUT_CLASSES);
    let err = check_input_grant(&classes, (true, false), None, (123_456, 0)).unwrap_err();
    assert!(!err.contains("123456"));
    let err = check_input_grant(&classes, (true, false), None, (0, 7_654_321)).unwrap_err();
    assert!(!err.contains("7654321"));
    assert_eq!(
        check_input_grant(&classes, (true, false), None, (5000, 1_000_000)),
        Ok(())
    );
    assert_eq!(
        check_input_grant(&classes, (false, false), None, (0, 0)),
        Ok(())
    );
}

#[tokio::test]
async fn a_grant_stores_the_click_allowance_and_the_any_window_scope_and_reads_them_back() {
    let s = store().await;
    // The default has no click allowance, and neither has an older-style grant.
    assert!(!NewGrant::new(StationId::from("alpha"), SessionId::from("coord")).allow_click);
    let plain = s.create_grant(input_grant(|_| {})).await.unwrap();
    assert!(plain.allow_input && !plain.allow_click);
    assert_eq!(plain.input_scope, Some(InputScope::OwnWindows));
    // A click allowance with the own scope.
    let mut g = input_grant(|_| {});
    g.session_id = SessionId::from("c2");
    g.allow_click = true;
    let click = s.create_grant(g).await.unwrap();
    assert!(click.allow_click);
    assert_eq!(click.input_scope, Some(InputScope::OwnWindows));
    assert_eq!(
        s.grant(click.grant_id.clone()).await.unwrap().unwrap(),
        click
    );
    // The broader scope, with and without the click allowance.
    for (session, allow_click) in [("c3", false), ("c4", true)] {
        let mut g = input_grant(|_| {});
        g.session_id = SessionId::from(session);
        g.input_scope = Some(InputScope::AnyWindow);
        g.allow_click = allow_click;
        let wide = s.create_grant(g).await.unwrap();
        assert!(wide.allow_input);
        assert_eq!(wide.input_scope, Some(InputScope::AnyWindow));
        assert_eq!(wide.allow_click, allow_click);
        assert_eq!(s.grant(wide.grant_id.clone()).await.unwrap().unwrap(), wide);
    }
}

#[tokio::test]
async fn the_click_rule_has_its_fixed_text_and_the_scope_rules_are_unchanged() {
    let s = store().await;
    assert_eq!(
        invalid(&s, new_grant_with_click(&INPUT_CLASSES, false)).await,
        "allow_click needs allow_input"
    );
    // With the click flag and a scope but no allowance, the scope rule that
    // came first in P7 still comes first.
    let mut g = new_grant_with_click(&INPUT_CLASSES, false);
    g.input_scope = Some(InputScope::AnyWindow);
    assert_eq!(invalid(&s, g).await, "input_scope needs allow_input");
    // The class rule comes before the click rule.
    let mut g = new_grant(&[GrantClass::Read]);
    g.allow_input = true;
    g.allow_click = true;
    assert_eq!(
        invalid(&s, g).await,
        "allow_input needs the desktop, capture and input classes"
    );
    assert!(s.grants(10).await.unwrap().is_empty());
}

fn new_grant_with_click(classes: &[GrantClass], allow_input: bool) -> NewGrant {
    let mut g = new_grant(classes);
    g.allow_input = allow_input;
    g.allow_click = true;
    g
}

#[test]
fn the_stored_scope_text_maps_both_ways_and_fails_closed() {
    use crate::store::grants_input::{input_scope_of, scope_text};
    assert_eq!(scope_text(InputScope::OwnWindows), "own_windows");
    assert_eq!(scope_text(InputScope::AnyWindow), "any_window");
    assert_eq!(
        input_scope_of(1, Some("own_windows")),
        Some(InputScope::OwnWindows)
    );
    assert_eq!(
        input_scope_of(1, Some("any_window")),
        Some(InputScope::AnyWindow)
    );
    for text in [None, Some("any"), Some("AnyWindow"), Some("")] {
        assert_eq!(input_scope_of(1, text), None, "{text:?}");
    }
    // Without the allowance a stored scope is no scope.
    assert_eq!(input_scope_of(0, Some("any_window")), None);
}

#[test]
fn the_click_check_names_no_value_and_a_click_with_input_passes() {
    let classes = BTreeSet::from(INPUT_CLASSES);
    assert_eq!(
        check_input_grant(&classes, (false, true), None, (0, 0)),
        Err("allow_click needs allow_input")
    );
    assert_eq!(
        check_input_grant(&classes, (true, true), Some(InputScope::AnyWindow), (5, 6)),
        Ok(())
    );
}
