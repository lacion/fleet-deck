//! R13 P5b-2 §13.5: the station-internal `recording` class of a binding
//! comes only from `allow_recording` with the `capture` class.

#![allow(clippy::unwrap_used)]

use fleetdeck_proto::command::CoordinatorSpec;

use super::{DeckFeatures, ToolClass, grant_classes};
use crate::manager::coordinator_spec_tests::coordinator_spec;

fn spec(classes: &[&str], allow_recording: bool) -> CoordinatorSpec {
    CoordinatorSpec {
        classes: classes.iter().map(|c| (*c).to_owned()).collect(),
        allow_recording,
        ..coordinator_spec()
    }
}

#[test]
fn the_recording_class_needs_the_flag_and_the_capture_class() {
    let has = |classes: &[&str], flag: bool| {
        grant_classes(&spec(classes, flag), DeckFeatures::default())
            .unwrap()
            .contains(&ToolClass::Recording)
    };
    assert!(has(&["capture"], true));
    assert!(has(&["desktop", "capture"], true));
    // The flag alone, or the capture class alone, gives nothing.
    assert!(!has(&["desktop"], true));
    assert!(!has(&["read"], true));
    assert!(!has(&["capture"], false));
    assert!(!has(&["desktop", "capture"], false));
}

#[test]
fn a_spec_that_names_the_recording_class_is_refused_as_unknown() {
    for flag in [false, true] {
        let err = grant_classes(
            &spec(&["capture", "recording"], flag),
            DeckFeatures::default(),
        )
        .unwrap_err();
        assert_eq!(err, "unknown grant class `recording`");
    }
}
