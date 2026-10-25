//! R13 P5b-2 §14: a coordinator `screen_record` is admitted only on a link
//! with `desktop_mcp.v1`, `desktop_recording.v1`, `artifacts_video.v1` and
//! `capture_recording_mcp.v1`; otherwise the existing refusal.

#![allow(clippy::unwrap_used)]

use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::version::feature;

use super::{refusal, validate};
use crate::desktop::coordinator_tests::{ctx, negotiated};

fn record() -> DesktopAction {
    DesktopAction::ScreenRecord {
        monitor: "DP-1".to_owned(),
        seconds: 5,
    }
}

#[test]
fn a_coordinator_recording_needs_the_three_recording_features() {
    let all = [
        feature::DESKTOP_MCP,
        feature::DESKTOP_RECORDING,
        feature::ARTIFACTS_VIDEO,
        feature::CAPTURE_RECORDING_MCP,
    ];
    let context = ctx();
    assert_eq!(
        validate(Some(&context), &record(), &negotiated(&all)).unwrap(),
        &context
    );
    // Each missing feature gives the existing refusal.
    for missing in all {
        let have: Vec<&str> = all.iter().copied().filter(|f| *f != missing).collect();
        let e = validate(Some(&context), &record(), &negotiated(&have)).unwrap_err();
        assert_eq!(e, refusal(), "{missing}");
    }
    // `capture_mcp.v1` alone does not give a recording.
    let screenshot_only = negotiated(&[feature::DESKTOP_MCP, feature::CAPTURE_MCP]);
    assert_eq!(
        validate(Some(&context), &record(), &screenshot_only).unwrap_err(),
        refusal()
    );
    // A recording without a coordinator context is still refused.
    assert_eq!(
        validate(None, &record(), &negotiated(&all)).unwrap_err(),
        refusal()
    );
}

#[test]
fn the_screenshot_checks_are_unchanged() {
    let shot = DesktopAction::ScreenCapture {
        monitor: "DP-1".to_owned(),
    };
    let context = ctx();
    assert!(
        validate(
            Some(&context),
            &shot,
            &negotiated(&[feature::DESKTOP_MCP, feature::CAPTURE_MCP])
        )
        .is_ok()
    );
    assert!(validate(Some(&context), &shot, &negotiated(&[feature::DESKTOP_MCP])).is_err());
}
