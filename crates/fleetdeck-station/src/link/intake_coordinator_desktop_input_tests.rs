//! R13 P7 §6.4: `desktop.policy_now` with the input purpose. The answer needs
//! `desktop_input_mcp.v1`, follows the live settings, and a null monitor is
//! unresolved, so it is allowed (the target enforces the real coverage). The
//! P6 `desktop` and `capture` answers keep their bytes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::coordinator_desktop_wire::{DesktopPolicyNow, PolicyPurpose};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::{Live, input_policy_answer, policy_answer, policy_reply};

fn live(monitors: &[&str]) -> Live {
    Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        desktop_input: true,
        monitors: monitors.iter().map(|m| (*m).to_owned()).collect(),
    }
}

fn query(purpose: PolicyPurpose, monitor: Option<&str>) -> DesktopPolicyNow {
    DesktopPolicyNow {
        purpose,
        monitor: monitor.map(str::to_owned),
    }
}

fn answered(outcome: Outcome) -> Value {
    match outcome {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("an error: {e:?}"),
    }
}

fn refused(outcome: Outcome) -> String {
    match outcome {
        Outcome::Err(e) => e.message,
        Outcome::Ok(v) => panic!("an answer: {v}"),
    }
}

#[test]
fn each_flag_of_the_input_answer_follows_the_live_settings() {
    type Flag = fn(&mut Live);
    let flags: [(&str, Flag); 4] = [
        ("allow_fleet_control", |l| l.fleet_control = false),
        ("allow_desktop", |l| l.desktop = false),
        ("allow_desktop_capture", |l| l.capture = false),
        ("allow_desktop_input", |l| l.desktop_input = false),
    ];
    let all_on = serde_json::to_value(input_policy_answer(&live(&[]), Some("DP-1"))).unwrap();
    assert_eq!(
        all_on,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "allow_desktop_input": true,
            "monitor_allowed": true
        })
    );
    for (name, turn_off) in flags {
        let mut l = live(&[]);
        turn_off(&mut l);
        let v = serde_json::to_value(input_policy_answer(&l, Some("DP-1"))).unwrap();
        for key in [
            "allow_fleet_control",
            "allow_desktop",
            "allow_desktop_capture",
            "allow_desktop_input",
        ] {
            assert_eq!(v[key], key != name, "{name} {key}");
        }
        assert_eq!(v["monitor_allowed"], true, "{name}");
    }
}

#[test]
fn a_null_monitor_is_unresolved_and_allowed_for_every_monitor_list() {
    assert!(input_policy_answer(&live(&[]), None).monitor_allowed);
    assert!(input_policy_answer(&live(&["DP-1"]), None).monitor_allowed);
    assert!(input_policy_answer(&live(&["DP-1", "DP-2"]), None).monitor_allowed);
}

#[test]
fn a_configured_monitor_list_still_answers_an_input_query_with_a_null_monitor() {
    // The deck asks with a null monitor; the answer over the link must not
    // refuse every input when `capture_monitors` is set (PRE-PR r1 finding 5).
    let q = query(PolicyPurpose::Input, None);
    let v = answered(policy_reply(&live(&["DP-1"]), &q, true, true));
    assert_eq!(
        v,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "allow_desktop_input": true,
            "monitor_allowed": true
        })
    );
}

#[test]
fn a_named_monitor_follows_the_capture_monitor_rule() {
    assert!(input_policy_answer(&live(&[]), Some("DP-9")).monitor_allowed);
    assert!(input_policy_answer(&live(&["DP-1"]), Some("DP-1")).monitor_allowed);
    assert!(!input_policy_answer(&live(&["DP-1"]), Some("DP-2")).monitor_allowed);
    // The same rule as the P6 answer for a named monitor.
    for (list, name) in [
        (&[][..], "DP-9"),
        (&["DP-1"][..], "DP-1"),
        (&["DP-1"][..], "DP-2"),
    ] {
        assert_eq!(
            input_policy_answer(&live(list), Some(name)).monitor_allowed,
            policy_answer(&live(list), Some(name)).monitor_allowed
        );
    }
}

#[test]
fn the_input_purpose_is_answered_only_with_the_input_feature() {
    let q = query(PolicyPurpose::Input, None);
    let v = answered(policy_reply(&live(&[]), &q, true, true));
    assert_eq!(v["allow_desktop_input"], true);
    assert_eq!(v["monitor_allowed"], true);
    for (capture, input) in [(true, false), (false, false)] {
        let message = refused(policy_reply(&live(&[]), &q, capture, input));
        assert_eq!(
            message,
            fleetdeck_proto::coordinator_desktop_text::text(p6_code::DESKTOP_UNSUPPORTED).unwrap()
        );
    }
    // The capture feature alone does not answer the input purpose; the input
    // feature alone is enough for it.
    assert!(matches!(
        policy_reply(&live(&[]), &q, false, true),
        Outcome::Ok(_)
    ));
}

#[test]
fn the_p6_answers_keep_their_bytes() {
    let desktop = answered(policy_reply(
        &live(&["DP-1"]),
        &query(PolicyPurpose::Desktop, None),
        true,
        true,
    ));
    assert_eq!(
        desktop,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "monitor_allowed": true
        })
    );
    let capture = answered(policy_reply(
        &live(&["DP-1"]),
        &query(PolicyPurpose::Capture, Some("DP-2")),
        true,
        false,
    ));
    assert_eq!(
        capture,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "monitor_allowed": false
        })
    );
    // The capture purpose still needs `capture_mcp.v1`; the desktop purpose
    // needs nothing more.
    let q = query(PolicyPurpose::Capture, None);
    assert!(matches!(
        policy_reply(&live(&[]), &q, false, true),
        Outcome::Err(_)
    ));
    let q = query(PolicyPurpose::Desktop, None);
    assert!(matches!(
        policy_reply(&live(&[]), &q, false, false),
        Outcome::Ok(_)
    ));
}
