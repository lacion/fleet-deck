//! R13 P7 §6.4: `desktop.policy_now` with the input purpose. The answer needs
//! `desktop_input_mcp.v1`, follows the live settings, and a null monitor is
//! unresolved, so it is allowed (the target enforces the real coverage). The
//! P6 `desktop` and `capture` answers keep their bytes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, CoordinatorRead, DesktopPolicyNow, PolicyPurpose,
    ProtectedRead,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::read_with;
use super::{DispatchCheck, Fail, Live, Reader, input_policy_answer, policy_answer, policy_reply};
use crate::artifacts::capture_import::tests::{FP, rig as store_rig};
use crate::desktop::fake::{Opts, Rig};

/// No P7b feature negotiated.
const NONE: (bool, bool) = (false, false);

fn live(monitors: &[&str]) -> Live {
    Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        desktop_input: true,
        desktop_click: true,
        desktop_input_any: true,
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
    let v = answered(policy_reply(&live(&["DP-1"]), &q, true, true, NONE));
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
    let v = answered(policy_reply(&live(&[]), &q, true, true, NONE));
    assert_eq!(v["allow_desktop_input"], true);
    assert_eq!(v["monitor_allowed"], true);
    for (capture, input) in [(true, false), (false, false)] {
        let message = refused(policy_reply(&live(&[]), &q, capture, input, NONE));
        assert_eq!(
            message,
            fleetdeck_proto::coordinator_desktop_text::text(p6_code::DESKTOP_UNSUPPORTED).unwrap()
        );
    }
    // The capture feature alone does not answer the input purpose; the input
    // feature alone is enough for it.
    assert!(matches!(
        policy_reply(&live(&[]), &q, false, true, NONE),
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
        NONE,
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
        NONE,
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
        policy_reply(&live(&[]), &q, false, true, NONE),
        Outcome::Err(_)
    ));
    let q = query(PolicyPurpose::Desktop, None);
    assert!(matches!(
        policy_reply(&live(&[]), &q, false, false, NONE),
        Outcome::Ok(_)
    ));
}

#[test]
fn the_p7b_purposes_need_their_features_and_answer_their_gates() {
    // (purpose, click feature, any feature, answered)
    let cases = [
        (PolicyPurpose::InputClick, false, false, false),
        (PolicyPurpose::InputClick, true, false, true),
        (PolicyPurpose::InputClick, false, true, false),
        (PolicyPurpose::InputAny, false, false, false),
        (PolicyPurpose::InputAny, true, false, false),
        (PolicyPurpose::InputAny, false, true, true),
        (PolicyPurpose::InputAnyClick, false, false, false),
        (PolicyPurpose::InputAnyClick, true, false, false),
        (PolicyPurpose::InputAnyClick, false, true, false),
        (PolicyPurpose::InputAnyClick, true, true, true),
    ];
    let unsupported =
        fleetdeck_proto::coordinator_desktop_text::text(p6_code::DESKTOP_UNSUPPORTED).unwrap();
    for (purpose, click, any, answered) in cases {
        let q = query(purpose, None);
        // The input and capture features do not matter for these purposes.
        let reply = policy_reply(&live(&[]), &q, false, false, (click, any));
        if answered {
            assert!(matches!(reply, Outcome::Ok(_)), "{purpose:?} {click} {any}");
        } else {
            assert_eq!(refused(reply), unsupported, "{purpose:?} {click} {any}");
        }
    }
}

#[test]
fn the_p7b_answers_have_the_five_fields_and_only_the_gates_of_the_purpose() {
    let both = (true, true);
    let click = answered(policy_reply(
        &live(&["DP-1"]),
        &query(PolicyPurpose::InputClick, None),
        false,
        false,
        both,
    ));
    assert_eq!(
        click,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "allow_desktop_input": true,
            "monitor_allowed": true,
            "allow_desktop_click": true
        })
    );
    let any = answered(policy_reply(
        &live(&["DP-1"]),
        &query(PolicyPurpose::InputAny, Some("DP-2")),
        false,
        false,
        both,
    ));
    assert_eq!(
        any,
        json!({
            "allow_fleet_control": true,
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "allow_desktop_input": true,
            "monitor_allowed": false,
            "allow_desktop_input_any": true
        })
    );
    let both_gates = answered(policy_reply(
        &live(&[]),
        &query(PolicyPurpose::InputAnyClick, None),
        false,
        false,
        both,
    ));
    assert_eq!(both_gates["allow_desktop_click"], true);
    assert_eq!(both_gates["allow_desktop_input_any"], true);
}

#[test]
fn each_station_setting_changes_only_its_own_field_and_the_answer_fits() {
    use fleetdeck_proto::coordinator_desktop_wire::DesktopInputGatesAnswer;
    for purpose in [
        PolicyPurpose::InputClick,
        PolicyPurpose::InputAny,
        PolicyPurpose::InputAnyClick,
    ] {
        for (click_on, any_on) in [(true, true), (false, true), (true, false), (false, false)] {
            let mut l = live(&[]);
            l.desktop_click = click_on;
            l.desktop_input_any = any_on;
            let v = answered(policy_reply(
                &l,
                &query(purpose, None),
                false,
                false,
                (true, true),
            ));
            let a: DesktopInputGatesAnswer = serde_json::from_value(v.clone()).unwrap();
            assert!(a.fits(purpose), "{purpose:?}: {v}");
            let wants_click = !matches!(purpose, PolicyPurpose::InputAny);
            let wants_any = !matches!(purpose, PolicyPurpose::InputClick);
            assert_eq!(a.allow_desktop_click, wants_click.then_some(click_on));
            assert_eq!(a.allow_desktop_input_any, wants_any.then_some(any_on));
        }
    }
    // The input purpose keeps its five fields whatever the new settings are.
    let mut l = live(&[]);
    l.desktop_click = false;
    l.desktop_input_any = false;
    let v = answered(policy_reply(
        &l,
        &query(PolicyPurpose::Input, None),
        true,
        true,
        (true, true),
    ));
    assert_eq!(v.as_object().unwrap().len(), 5);
}

// ------------------------------------------------- the window_list read

/// A deck check that passes and keeps what it saw.
struct Seen(Mutex<Vec<(AuthorityPhase, Option<ProtectedRead>)>>);

#[async_trait]
impl DispatchCheck for Seen {
    async fn check(&self, phase: AuthorityPhase, read: Option<&ProtectedRead>) -> Result<(), Fail> {
        self.0.lock().unwrap().push((phase, read.cloned()));
        Ok(())
    }
}

fn windows_read(context: Option<CoordinatorContext>) -> CoordinatorRead {
    CoordinatorRead {
        read_id: "ab".repeat(16),
        read: ProtectedRead::InputWindows,
        read_hash: "cd".repeat(32),
        context,
    }
}

fn caller() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "grant-1".to_owned(),
        binding_id: "binding-1".to_owned(),
        generation: 1,
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
        caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
        caller_label: "bold-fox".to_owned(),
    }
}

#[tokio::test]
async fn the_window_list_read_is_served_by_the_listing_and_needs_a_context() {
    let fake = Rig::new(Opts::default());
    let s = store_rig().await;
    let dir = fake.dir.path();
    std::fs::write(
        dir.join("monitors.json"),
        json!([{"name": "DP-1", "x": 0, "y": 0, "width": 1000, "height": 800,
                "scale": 1.0, "transform": 0, "activeWorkspace": {"id": 1}}])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("clients.json"),
        json!([{"address": "0xa1", "class": "kitty", "title": "SENTINEL", "initialClass": "kitty",
                "initialTitle": "SENTINEL", "mapped": true, "hidden": false,
                "at": [10, 20], "size": [300, 200], "workspace": {"id": 1},
                "focusHistoryID": 0}])
        .to_string(),
    )
    .unwrap();
    let settings = live(&[]);
    let live_now = move || settings.clone();
    let reader = Reader {
        deck_fp: FP,
        station: "target",
        desktop: &fake.desktop,
        service: None,
        store: &s.store,
        live: &live_now,
    };
    // Without a coordinator context the read is refused.
    let check = Seen(Mutex::new(Vec::new()));
    let e = read_with(&reader, &check, &windows_read(None))
        .await
        .unwrap_err();
    assert_eq!(e.code, "desktop_failed");
    assert!(check.0.lock().unwrap().is_empty());
    // With one: the closed projection, after the authority check of the read.
    let value = read_with(&reader, &check, &windows_read(Some(caller())))
        .await
        .unwrap();
    assert_eq!(
        *check.0.lock().unwrap(),
        [(AuthorityPhase::Read, Some(ProtectedRead::InputWindows))]
    );
    assert_eq!(value["station"], "target");
    assert_eq!(value["windows"][0]["rect"]["width"], 300);
    assert_eq!(value["windows"][0]["focused"], true);
    assert!(!value.to_string().contains("SENTINEL"));
    // The broader gate is a gate of the read.
    let mut off = live(&[]);
    off.desktop_input_any = false;
    let off_now = move || off.clone();
    let reader = Reader {
        live: &off_now,
        ..reader
    };
    let e = read_with(&reader, &check, &windows_read(Some(caller())))
        .await
        .unwrap_err();
    assert_eq!(e.code, "input_any_disabled");
}
