//! The outcome mapping and the waits of the coordinator send (R13 P6 §11.2,
//! decision O12-9 item 8, Q6, Q11).

use fleetdeck_proto::desktop::{DesktopAllowed, DesktopInfo, DesktopKind, DesktopTools, Urgency};

use super::*;

fn info() -> DesktopInfo {
    DesktopInfo {
        kind: DesktopKind::Omarchy,
        omarchy_version: None,
        monitors: Vec::new(),
        locked: None,
        dnd: None,
        idle_inhibited: None,
        walls: Vec::new(),
        allowed: DesktopAllowed {
            desktop: true,
            capture: false,
            capture_monitors: Vec::new(),
            input: false,
        },
        tools: DesktopTools {
            wtype: false,
            pointer: false,
            recorder: false,
            grim: false,
            chromium: true,
            notify_send: true,
        },
        utc_offset_minutes: None,
        offset_sampled_at: None,
        features: Vec::new(),
        previews: Vec::new(),
        host_addresses: Vec::new(),
    }
}

fn result(window_id: Option<&str>) -> DesktopActResult {
    DesktopActResult {
        address: None,
        window_id: window_id.map(str::to_owned),
        info: info(),
        capture: None,
        info_fresh: None,
    }
}

fn notify() -> DesktopAction {
    DesktopAction::Notify {
        title: "t".into(),
        body: String::new(),
        urgency: Urgency::Normal,
    }
}

fn preview() -> DesktopAction {
    DesktopAction::BrowserOpen {
        url: "http://localhost:1".into(),
        monitor: None,
        session: None,
    }
}

fn wire(code: &str, message: &str) -> Result<DesktopActResult, CallError> {
    Err(CallError::Wire {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

#[test]
fn a_success_is_an_empty_result_or_the_window_id_of_a_preview() {
    assert_eq!(
        outcome_of(&notify(), Ok(result(None))),
        OpOutcome::Done(json!({}))
    );
    assert_eq!(
        outcome_of(
            &DesktopAction::PreviewClose {
                window_id: "w".into()
            },
            Ok(result(None))
        ),
        OpOutcome::Done(json!({}))
    );
    assert_eq!(
        outcome_of(&preview(), Ok(result(Some("abc")))),
        OpOutcome::Done(json!({"window_id": "abc"}))
    );
    // A preview without a handle failed.
    assert_eq!(
        outcome_of(&preview(), Ok(result(None))),
        OpOutcome::Failed("desktop_failed")
    );
}

#[test]
fn the_codes_of_a_lost_authority_and_a_missing_window_pass_through() {
    for code in [
        "authority_lost",
        "authority_unknown",
        "desktop_window_not_found",
    ] {
        assert_eq!(
            outcome_of(&notify(), wire(code, "text")),
            OpOutcome::Failed(code),
            "{code}"
        );
    }
}

#[test]
fn any_other_station_error_maps_like_the_owner_relay() {
    let cases = [
        ("unsupported", "desktop_unsupported"),
        ("feature_not_negotiated", "desktop_unsupported"),
        ("forbidden", "desktop_forbidden"),
        ("invalid_params", "invalid_desktop_action"),
        ("not_found", "desktop_not_found"),
        ("conflict", "desktop_conflict"),
        ("too_many_requests", "desktop_overloaded"),
        ("failed", "desktop_failed"),
        ("something_new", "desktop_failed"),
        // A station timeout answer is a failure, not an unknown outcome.
        ("timeout", "desktop_timeout"),
    ];
    for (wire_code, want) in cases {
        assert_eq!(
            outcome_of(&notify(), wire(wire_code, "x")),
            OpOutcome::Failed(want),
            "{wire_code}"
        );
    }
    assert_eq!(
        outcome_of(
            &notify(),
            wire(
                "failed",
                crate::link::desktop_authority::STATION_ACT_TIMEOUT
            )
        ),
        OpOutcome::Failed("desktop_timeout")
    );
}

#[test]
fn only_a_deck_wait_that_ran_out_or_a_lost_link_is_unknown() {
    for e in [RelayError::Timeout, RelayError::Unreachable] {
        assert_eq!(
            outcome_of(&notify(), Err(CallError::Relay(e))),
            OpOutcome::Unknown
        );
    }
    // A malformed answer of the station.
    assert_eq!(
        outcome_of(&notify(), Err(CallError::Relay(RelayError::Failed))),
        OpOutcome::Failed("desktop_failed")
    );
}

#[test]
fn the_waits_are_the_deck_waits_cut_by_the_time_that_is_left() {
    let far = Instant::now() + Duration::from_secs(1_000);
    let w = waits(far);
    assert_eq!(w.long, Duration::from_millis(95_000));
    assert_eq!(w.short, Duration::from_millis(35_000));
    let near = Instant::now() + Duration::from_secs(10);
    let w = waits(near);
    assert!(w.long <= Duration::from_secs(10) && w.short <= Duration::from_secs(10));
    assert!(w.long > Duration::from_secs(9), "{:?}", w.long);
    let past = Instant::now();
    let w = waits(past);
    assert!(w.long <= Duration::from_millis(5), "{:?}", w.long);
}
