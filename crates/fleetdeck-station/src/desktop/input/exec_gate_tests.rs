//! The admission and the live gates of an input operation (R13 P7 §3.3,
//! §6.2, §7.3 steps 1 and 2, §10): the feature and the grammar, the claim
//! kinds, every gate, the authority checks and the start lifetime.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::desktop::DesktopAction as A;
use fleetdeck_proto::version::feature;

use super::rig::{Fx, Probe, all_on, calls, nothing, pair, refused};
use crate::desktop::Fail;
use crate::desktop::coordinator::{
    Live, check_live_input, claim_kind, fleet_off, input_off, lost, unknown, validate,
};
use crate::desktop::coordinator_tests::{ctx, negotiated};

// -------------------------------------------------------- the admission

#[test]
fn an_input_action_needs_a_coordinator_the_feature_and_valid_arguments() {
    let window = "0123456789abcdef0123456789abcdef".to_owned();
    let actions = [
        A::InputKeyPress {
            window_id: window.clone(),
            chord: "Ctrl+a".into(),
        },
        A::InputTypeText {
            window_id: window.clone(),
            text: "hi".into(),
        },
        A::InputPointerMove {
            window_id: window,
            x: 1,
            y: 2,
        },
    ];
    let with = negotiated(&[feature::DESKTOP_MCP, feature::DESKTOP_INPUT_MCP]);
    let without = negotiated(&[feature::DESKTOP_MCP, feature::CAPTURE_MCP]);
    let c = ctx();
    for action in &actions {
        assert!(validate(Some(&c), action, &with).is_ok(), "{action:?}");
        // Without the feature, or without a coordinator, or without the
        // desktop feature: the refused text.
        for refusal in [
            validate(Some(&c), action, &without).unwrap_err(),
            validate(None, action, &with).unwrap_err(),
            validate(Some(&c), action, &negotiated(&[feature::DESKTOP_INPUT_MCP])).unwrap_err(),
        ] {
            assert_eq!(refusal.code, "forbidden", "{action:?}");
        }
    }
    // Arguments that break the grammar are `invalid_params`.
    for bad in [
        A::InputKeyPress {
            window_id: "x".into(),
            chord: "a".into(),
        },
        A::InputKeyPress {
            window_id: "0123456789abcdef0123456789abcdef".into(),
            chord: "Alt+Tab".into(),
        },
        A::InputTypeText {
            window_id: "0123456789abcdef0123456789abcdef".into(),
            text: "a\nb".into(),
        },
    ] {
        let e = validate(Some(&c), &bad, &with).unwrap_err();
        assert_eq!(e.code, "invalid_params", "{bad:?}");
    }
}

#[test]
fn the_input_actions_have_their_claim_kinds() {
    let window = "0123456789abcdef0123456789abcdef".to_owned();
    assert_eq!(
        claim_kind(&A::InputKeyPress {
            window_id: window.clone(),
            chord: "a".into()
        }),
        Some("input_key_press")
    );
    assert_eq!(
        claim_kind(&A::InputTypeText {
            window_id: window.clone(),
            text: "a".into()
        }),
        Some("input_type_text")
    );
    assert_eq!(
        claim_kind(&A::InputPointerMove {
            window_id: window,
            x: 0,
            y: 0
        }),
        Some("input_pointer_move")
    );
}

#[test]
fn the_input_gates_are_read_in_the_order_of_the_contract_with_fixed_texts() {
    let off = |f: fn(&mut Live)| {
        let mut live = all_on();
        f(&mut live);
        check_live_input(&live).unwrap_err()
    };
    assert_eq!(check_live_input(&all_on()), Ok(()));
    assert_eq!(off(|l| l.fleet_control = false), fleet_off());
    assert_eq!(off(|l| l.desktop = false), Fail::forbidden());
    assert_eq!(off(|l| l.desktop_input = false), input_off());
    assert_eq!(
        pair(&off(|l| l.capture = false)),
        (
            "forbidden",
            "Desktop screenshots are off on this station.".into()
        )
    );
    assert_eq!(
        pair(&input_off()),
        ("forbidden", "Desktop input is off on this station.".into())
    );
    // Everything off: the first gate answers.
    let mut live = all_on();
    live.fleet_control = false;
    live.desktop = false;
    live.desktop_input = false;
    live.capture = false;
    assert_eq!(check_live_input(&live), Err(fleet_off()));
    live.fleet_control = true;
    assert_eq!(check_live_input(&live), Err(Fail::forbidden()));
    live.desktop = true;
    assert_eq!(check_live_input(&live), Err(input_off()));
}

#[tokio::test]
async fn a_grammar_failure_runs_nothing() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    nothing(&fx, &probe, &fx.key("Alt+Tab"), "invalid_params").await;
}

// ------------------------------------------------------------ the gates

#[tokio::test]
async fn each_gate_that_is_off_stops_the_call_before_any_desktop_work() {
    type TurnOff = fn(&mut Live);
    let cases: [(TurnOff, Fail); 4] = [
        (|l| l.fleet_control = false, fleet_off()),
        (|l| l.desktop = false, Fail::forbidden()),
        (|l| l.desktop_input = false, input_off()),
        (
            |l| l.capture = false,
            check_live_input(&{
                let mut l = all_on();
                l.capture = false;
                l
            })
            .unwrap_err(),
        ),
    ];
    for (turn_off, expected) in cases {
        let fx = Fx::new().await;
        let mut probe = Probe::new(&fx);
        turn_off(&mut probe.live);
        let e = refused(&fx, &probe, &fx.key("a")).await;
        assert_eq!(e, expected);
        assert_eq!(calls(&fx), 0);
        assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_gate_turned_off_at_any_check_stops_the_protocol_there() {
    for (n, expect) in [
        (1_usize, vec!["claim", "check"]),
        (2, vec!["claim", "check", "notify-send", "check"]),
        (
            3,
            vec!["claim", "check", "notify-send", "check", "FOCUS", "check"],
        ),
    ] {
        let fx = Fx::new().await;
        let root = fx.root();
        let off = root.join("gates_off");
        let mut probe = Probe::new(&fx);
        let flag = off.clone();
        probe.check = fx.check(
            None,
            Some(Box::new(move |k| {
                if k == n {
                    std::fs::write(&flag, "").unwrap();
                }
            })),
        );
        probe.gates = Some(Arc::new(move || {
            if off.exists() {
                Err(input_off())
            } else {
                Ok(())
            }
        }));
        let e = refused(&fx, &probe, &fx.text("x")).await;
        assert_eq!(e, input_off(), "check {n}");
        let want: Vec<String> = expect
            .iter()
            .map(|s| {
                if *s == "FOCUS" {
                    fx.focus_line()
                } else {
                    (*s).to_owned()
                }
            })
            .collect();
        assert_eq!(fx.timeline(), want, "check {n}");
        assert!(fx.calls_of("wtype").is_empty());
    }
}

#[tokio::test]
async fn a_gate_turned_off_after_the_final_check_stops_the_spawn() {
    let fx = Fx::new().await;
    let off = fx.root().join("gates_off");
    let mut probe = Probe::new(&fx);
    let flag = off.clone();
    probe.gates = Some(Arc::new(move || {
        if flag.exists() {
            Err(input_off())
        } else {
            Ok(())
        }
    }));
    // The final active-window read turns the gate off.
    fx.write_text("on_active.sh", &format!(": > {}", off.display()));
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e, input_off());
    assert!(fx.calls_of("wtype").is_empty());
    assert_eq!(fx.timeline().last().unwrap(), "hyprctl -j activewindow");
}

#[tokio::test]
async fn a_withdrawn_or_unknown_authority_stops_the_protocol_at_each_check() {
    for (fail, wire) in [(lost(), "authority_lost"), (unknown(), "authority_unknown")] {
        for (n, steps) in [(1_usize, 2_usize), (2, 4), (3, 6)] {
            let fx = Fx::new().await;
            let mut probe = Probe::new(&fx);
            probe.check = fx.check(Some((n, fail.clone())), None);
            let e = refused(&fx, &probe, &fx.key("a")).await;
            assert_eq!(e.code, wire);
            // Nothing after the failed check: no notification before the
            // first, no focus before the second, no read before the third.
            assert_eq!(fx.timeline().len(), steps, "{:?}", fx.timeline());
            assert!(fx.calls_of("wtype").is_empty());
        }
    }
}

#[tokio::test]
async fn a_check_after_the_start_lifetime_does_not_buy_a_new_one() {
    // The authority lived 10 s when the second check ran: no focus.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let harness = Arc::clone(&probe.harness);
    probe.check = fx.check(
        None,
        Some(Box::new(move |k| {
            if k == 2 {
                harness.advance_mono(11_000);
            }
        })),
    );
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "failed");
    assert_eq!(
        e.message,
        "The authorization of this desktop action expired."
    );
    assert_eq!(fx.timeline(), ["claim", "check", "notify-send", "check"]);
}

#[tokio::test]
async fn the_start_lifetime_is_read_again_right_before_the_spawn() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let harness = Arc::clone(&probe.harness);
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // The clock reads after the third check: the second read is the focus
    // snapshot. The authority expires there.
    probe.clock = Some(Arc::new(move || {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            harness.advance_mono(11_000);
        }
        std::time::Instant::now()
    }));
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "failed");
    assert!(fx.calls_of("wtype").is_empty());
}
