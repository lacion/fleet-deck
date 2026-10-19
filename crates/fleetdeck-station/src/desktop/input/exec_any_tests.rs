//! The input protocol under the `any_window` scope (R13 P7b §7, §8, §9, §10.1,
//! §13): the admission and the gates of the new variants, a listed window as
//! the target of keys, text and a pointer movement, the invalidation of a
//! handle at each point, and the gates of the click variants.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use fleetdeck_proto::desktop::DesktopAction as A;
use fleetdeck_proto::version::feature;
use serde_json::json;

use super::listed::{Binding, Item};
use super::rig::{Fx, Probe, all_on, foreign_active, nothing, other_client, pair, refused};
use crate::desktop::Fail;
use crate::desktop::coordinator::{
    check_live_input, check_live_window_list, claim_kind, input_off, validate,
};
use crate::desktop::coordinator_tests::{DECK, ctx, negotiated};
use crate::desktop::events::Event;

const WINDOW: &str = "0123456789abcdef0123456789abcdef";
const NOT_FOUND: &str = "No input window available to this grant matches that handle.";

// ------------------------------------------------- admission and gates

fn any_actions() -> [A; 4] {
    [
        A::InputAnyKeyPress {
            window_id: WINDOW.into(),
            chord: "Ctrl+a".into(),
        },
        A::InputAnyTypeText {
            window_id: WINDOW.into(),
            text: "hi".into(),
        },
        A::InputAnyPointerMove {
            window_id: WINDOW.into(),
            x: 1,
            y: 2,
        },
        A::InputAnyPointerClick {
            window_id: WINDOW.into(),
            x: 1,
            y: 2,
        },
    ]
}

fn click() -> A {
    A::InputPointerClick {
        window_id: WINDOW.into(),
        x: 1,
        y: 2,
    }
}

#[test]
fn the_new_variants_need_their_features_and_the_grammar() {
    let base = [feature::DESKTOP_MCP, feature::DESKTOP_INPUT_MCP];
    let with = |extra: &[&str]| negotiated(&[&base[..], extra].concat());
    let c = ctx();
    for action in &any_actions()[..3] {
        assert!(
            validate(Some(&c), action, &with(&[])).is_err(),
            "{action:?}"
        );
        assert!(validate(Some(&c), action, &with(&[feature::DESKTOP_CLICK_MCP])).is_err());
        assert!(validate(Some(&c), action, &with(&[feature::DESKTOP_INPUT_ANY])).is_ok());
    }
    // A click in a preview needs the click feature only.
    assert!(validate(Some(&c), &click(), &with(&[])).is_err());
    assert!(validate(Some(&c), &click(), &with(&[feature::DESKTOP_INPUT_ANY])).is_err());
    assert!(validate(Some(&c), &click(), &with(&[feature::DESKTOP_CLICK_MCP])).is_ok());
    // A click under the broader scope needs both.
    let any_click = &any_actions()[3];
    assert!(validate(Some(&c), any_click, &with(&[feature::DESKTOP_INPUT_ANY])).is_err());
    assert!(validate(Some(&c), any_click, &with(&[feature::DESKTOP_CLICK_MCP])).is_err());
    let both = with(&[feature::DESKTOP_INPUT_ANY, feature::DESKTOP_CLICK_MCP]);
    assert!(validate(Some(&c), any_click, &both).is_ok());
    // Without the P7 feature none is admitted.
    let any_only = negotiated(&[feature::DESKTOP_MCP, feature::DESKTOP_INPUT_ANY]);
    assert!(validate(Some(&c), &any_actions()[0], &any_only).is_err());
    // The grammar is checked again for the keyboard and movement variants.
    let ok = with(&[feature::DESKTOP_INPUT_ANY]);
    for bad in [
        A::InputAnyKeyPress {
            window_id: WINDOW.into(),
            chord: "Alt+Tab".into(),
        },
        A::InputAnyTypeText {
            window_id: "short".into(),
            text: "hi".into(),
        },
        A::InputAnyPointerMove {
            window_id: WINDOW.into(),
            x: 40_000,
            y: 0,
        },
    ] {
        assert_eq!(
            validate(Some(&c), &bad, &ok).unwrap_err().code,
            "invalid_params"
        );
    }
}

#[test]
fn the_five_new_variants_have_their_claim_kinds() {
    let kinds: Vec<_> = [click()]
        .iter()
        .chain(&any_actions())
        .map(|a| claim_kind(a).unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "input_pointer_click",
            "input_any_key_press",
            "input_any_type_text",
            "input_any_pointer_move",
            "input_any_pointer_click"
        ]
    );
}

#[test]
fn the_p7b_gates_have_their_codes_texts_and_place_in_the_order() {
    let any = &any_actions();
    let off = |f: fn(&mut crate::desktop::coordinator::Live), action: &A| {
        let mut live = all_on();
        f(&mut live);
        check_live_input(&live, action).unwrap_err()
    };
    // The broader gate, after the P7 gates and before the capture gate.
    let e = off(|l| l.desktop_input_any = false, &any[0]);
    assert_eq!(
        pair(&e),
        (
            "input_any_disabled",
            "Agent input in other application windows is disabled on the target station.".into()
        )
    );
    assert_eq!(
        off(
            |l| {
                l.desktop_input = false;
                l.desktop_input_any = false;
            },
            &any[1]
        ),
        input_off()
    );
    let both = off(
        |l| {
            l.capture = false;
            l.desktop_input_any = false;
        },
        &any[2],
    );
    assert_eq!(both.code, "input_any_disabled");
    // The click gate: its own code and text; the broader gate comes first.
    let e = off(|l| l.desktop_click = false, &click());
    assert_eq!(
        pair(&e),
        (
            "input_click_disabled",
            "Agent pointer clicks are disabled on the target station.".into()
        )
    );
    assert_eq!(
        off(
            |l| {
                l.desktop_click = false;
                l.desktop_input_any = false;
            },
            &any[3]
        )
        .code,
        "input_any_disabled"
    );
    assert_eq!(
        off(|l| l.desktop_click = false, &any[3]).code,
        "input_click_disabled"
    );
    // The legacy variants do not read the new gates.
    let legacy = A::InputKeyPress {
        window_id: WINDOW.into(),
        chord: "a".into(),
    };
    let mut live = all_on();
    live.desktop_click = false;
    live.desktop_input_any = false;
    assert_eq!(check_live_input(&live, &legacy), Ok(()));
    assert_eq!(
        check_live_input(&live, &any[0]).unwrap_err().code,
        "input_any_disabled"
    );
    // The read has the broader gate and no click gate.
    assert_eq!(
        check_live_window_list(&live).unwrap_err().code,
        "input_any_disabled"
    );
    live.desktop_input_any = true;
    assert_eq!(check_live_window_list(&live), Ok(()));
}

// ------------------------------------------------------ a listed window

/// A desktop whose shell window `0xbeef` is active and listed.
async fn listed_shell() -> (Fx, String) {
    let fx = Fx::new().await;
    fx.set_active(&foreign_active());
    let handle = fx.other_handle().await;
    fx.clear_logs();
    (fx, handle)
}

#[tokio::test]
async fn a_listed_window_gets_keys_with_the_p7_order_and_its_own_notification() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.input_any(&handle, "Ctrl+Shift+a"), &probe)
        .await
        .unwrap();
    assert_eq!(
        fx.timeline(),
        [
            "claim",
            "check",
            "notify-send",
            "check",
            "hyprctl dispatch focuswindow address:0xbeef",
            "check",
            "hyprctl -j activewindow",
            "wtype",
            "hyprctl -j activewindow",
        ]
    );
    assert_eq!(
        *probe.check.phases.lock().unwrap(),
        [AuthorityPhase::Execute; 3]
    );
    let notify = fx.calls_of("notify-send");
    assert_eq!(
        notify[0].args.last().unwrap(),
        "Fleet Deck is sending keyboard input for bold-fox to an application window."
    );
    // Exactly the address of the registry is focused, once.
    let focuses = fx
        .timeline()
        .iter()
        .filter(|l| l.contains("focuswindow"))
        .count();
    assert_eq!(focuses, 1);
}

#[tokio::test]
async fn text_for_a_listed_window_travels_on_stdin_only() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    let action = A::InputAnyTypeText {
        window_id: handle.clone(),
        text: "SECRET-TEXT".into(),
    };
    fx.run(&action, &probe).await.unwrap();
    assert_eq!(fx.calls_of("wtype")[0].args, ["-"]);
    assert_eq!(fx.read("wtype.stdin"), "SECRET-TEXT");
    assert!(!fx.read("calls.log").contains("SECRET"));
    assert!(!fx.read("wtype.env").contains("SECRET"));
}

#[tokio::test]
async fn a_pointer_moves_inside_a_listed_window_with_its_own_notification() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    let action = A::InputAnyPointerMove {
        window_id: handle,
        x: 10,
        y: 20,
    };
    fx.run(&action, &probe).await.unwrap();
    // The window is at (2000, 100): the global point is (2010, 120).
    assert!(
        fx.timeline()
            .contains(&"hyprctl dispatch movecursor 2010 120".to_owned()),
        "{:?}",
        fx.timeline()
    );
    assert_eq!(
        fx.calls_of("notify-send")[0].args.last().unwrap(),
        "Fleet Deck is moving the pointer for bold-fox in an application window."
    );
    // Outside the window: unavailable, with the generic text.
    let outside = A::InputAnyPointerMove {
        window_id: fx.other_handle().await,
        x: 500,
        y: 0,
    };
    let e = refused(&fx, &probe, &outside).await;
    assert_eq!(
        pair(&e),
        (
            "input_destination_unavailable",
            "The pointer destination is not available inside the target window.".into()
        )
    );
}

#[tokio::test]
async fn an_own_preview_still_works_under_the_any_variants_and_needs_their_gate() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.input_any(&fx.window_id, "a"), &probe)
        .await
        .unwrap();
    assert!(fx.timeline().contains(&fx.focus_line()));
    // The any gate also guards an action aimed at an owned preview.
    let mut off = Probe::new(&fx);
    off.live.desktop_input_any = false;
    fx.clear_logs();
    let e = refused(&fx, &off, &fx.input_any(&fx.window_id, "a")).await;
    assert_eq!(
        pair(&e),
        (
            "input_any_disabled",
            "Agent input in other application windows is disabled on the target station.".into()
        )
    );
    assert!(fx.calls_of("wtype").is_empty());
    assert_eq!(off.claim.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_legacy_variants_resolve_previews_only() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    let legacy = A::InputKeyPress {
        window_id: handle,
        chord: "a".into(),
    };
    nothing(&fx, &probe, &legacy, "desktop_window_not_found").await;
}

// ---------------------------------------------------- invalid handles

#[tokio::test]
async fn an_unknown_foreign_or_expired_handle_gets_the_same_refusal() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    let unknown = refused(&fx, &probe, &fx.input_any(&"0".repeat(32), "a")).await;
    assert_eq!(
        pair(&unknown),
        ("desktop_window_not_found", NOT_FOUND.into())
    );
    // Another caller session, another generation, another grant.
    let mut other = Probe::new(&fx);
    other.context.caller_session = SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZC");
    let mut next = Probe::new(&fx);
    next.context.generation += 1;
    let mut grant = Probe::new(&fx);
    grant.context.grant_id = "grant-2".into();
    for foreign in [&other, &next, &grant] {
        let e = refused(&fx, foreign, &fx.input_any(&handle, "a")).await;
        assert_eq!(e, unknown);
    }
    // After the life of the handle.
    let mut late = Probe::new(&fx);
    late.clock = Some(Arc::new(|| Instant::now() + Duration::from_secs(61)));
    let e = refused(&fx, &late, &fx.input_any(&handle, "a")).await;
    assert_eq!(e, unknown);
    for p in [&probe, &other, &next, &grant, &late] {
        assert_eq!(p.claim.calls.load(Ordering::SeqCst), 0);
    }
    assert!(fx.timeline().is_empty(), "{:?}", fx.timeline());
}

#[tokio::test]
async fn a_closed_a_reopened_window_and_a_new_epoch_refuse_the_handle() {
    let probe_for = |fx: &Fx| Probe::new(fx);
    // A close.
    let (fx, handle) = listed_shell().await;
    fx.desktop().events.inject(Event::Close {
        address: "0xbeef".into(),
    });
    nothing(
        &fx,
        &probe_for(&fx),
        &fx.input_any(&handle, "a"),
        "desktop_window_not_found",
    )
    .await;
    // A close and a reopen of the same address: another incarnation.
    let (fx, handle) = listed_shell().await;
    for event in [
        Event::Close {
            address: "0xbeef".into(),
        },
        Event::Open {
            address: "0xbeef".into(),
            class: "alacritty".into(),
        },
    ] {
        fx.desktop().events.inject(event);
    }
    nothing(
        &fx,
        &probe_for(&fx),
        &fx.input_any(&handle, "a"),
        "desktop_window_not_found",
    )
    .await;
    // A gap in the event stream: a new epoch.
    let (fx, handle) = listed_shell().await;
    fx.desktop().events.inject_gap();
    nothing(
        &fx,
        &probe_for(&fx),
        &fx.input_any(&handle, "a"),
        "desktop_window_not_found",
    )
    .await;
}

#[tokio::test]
async fn a_changed_class_refuses_the_handle_before_the_claim() {
    let (fx, handle) = listed_shell().await;
    let mut renamed = other_client();
    renamed["class"] = json!("kitty");
    fx.set_clients(&[fx.preview_client(), renamed]);
    let probe = Probe::new(&fx);
    nothing(
        &fx,
        &probe,
        &fx.input_any(&handle, "a"),
        "desktop_window_not_found",
    )
    .await;
}

// ------------------------------------------------------------- barrier

#[tokio::test]
async fn a_board_title_on_a_listed_window_is_refused_before_the_claim() {
    for (title, hosts) in [
        ("Fleet Deck - board", vec![]),
        ("my fleetdeck", vec![]),
        (
            "Board at deck.example:8443",
            vec!["deck.example".to_owned()],
        ),
    ] {
        let (fx, handle) = listed_shell().await;
        let mut probe = Probe::new(&fx);
        probe.deck_hosts = hosts;
        let mut shell = other_client();
        shell["title"] = json!(title);
        fx.set_clients(&[fx.preview_client(), shell]);
        nothing(
            &fx,
            &probe,
            &fx.input_any(&handle, "a"),
            "input_board_forbidden",
        )
        .await;
    }
}

#[tokio::test]
async fn a_wall_record_at_the_address_of_a_listed_handle_is_refused() {
    let (fx, _) = listed_shell().await;
    let wall = crate::desktop::tests::run(
        &fx.rig,
        A::WallOpen {
            display_id: "d1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: crate::desktop::tests::url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    // The name rules do not match this window; only the wall record does.
    let mut client = Fx::window(&wall, (300, 300), (50, 50), 1);
    client["class"] = json!("plain-class");
    fx.set_clients(&[client]);
    let events = &fx.desktop().events;
    let item = Item {
        address: wall.clone(),
        class: "plain-class".into(),
        incarnation: events.observe(&wall),
    };
    let who = Binding::of(DECK, &ctx());
    let issued = fx
        .desktop()
        .listed()
        .issue(
            &who,
            crate::desktop::fake::SIGNATURE,
            events.epoch(),
            &[item],
            Instant::now(),
        )
        .unwrap();
    fx.set_active(
        &json!({"address": wall, "class": "plain-class", "title": "x",
                          "initialClass": "plain-class", "initialTitle": "x"}),
    );
    fx.clear_logs();
    let probe = Probe::new(&fx);
    let action = fx.input_any(&issued.handles[0].0, "a");
    nothing(&fx, &probe, &action, "input_board_forbidden").await;
}

/// A deck-origin mark on a preview record at the address of a listed handle,
/// set after the handle was issued, refuses the input (R13 P7b §8).
#[tokio::test]
async fn a_preview_marked_after_its_listing_refuses_the_listed_handle() {
    // Before the survey: nothing runs, no claim.
    let fx = Fx::new().await;
    fx.set_active(&fx.preview_active(super::rig::WINDOW_TITLE));
    let handle = Fx::handle_at(&fx.listing().await, 100, 100);
    fx.clear_logs();
    fx.desktop().mark_board_origin(&fx.window_id);
    let probe = Probe::new(&fx);
    nothing(
        &fx,
        &probe,
        &fx.input_any(&handle, "a"),
        "input_board_forbidden",
    )
    .await;
    // During the protocol, before the injection: the mark at the third
    // check is seen by the final observation; `wtype` never starts.
    let fx = Fx::new().await;
    fx.set_active(&fx.preview_active(super::rig::WINDOW_TITLE));
    let handle = Fx::handle_at(&fx.listing().await, 100, 100);
    fx.clear_logs();
    let desktop = Arc::clone(fx.desktop());
    let id = fx.window_id.clone();
    let mut probe = Probe::new(&fx);
    probe.check = fx.check(
        None,
        Some(Box::new(move |k| {
            if k == 3 {
                desktop.mark_board_origin(&id);
            }
        })),
    );
    let e = refused(&fx, &probe, &fx.input_any(&handle, "a")).await;
    assert_eq!(e.code, "input_board_forbidden");
    assert!(fx.calls_of("wtype").is_empty());
}

// ------------------------------------------------- the shown workspace

/// The owner switches away after the first survey and before the focus step
/// (here: at the claim): the station checks the shown workspace again right
/// before `focuswindow`, refuses, and never switches the workspace.
#[tokio::test]
async fn a_switch_away_before_the_focus_step_is_refused_without_a_focus() {
    let (fx, handle) = listed_shell().await;
    fx.write("claim_monitors.json", &fx.monitors_away());
    let probe = Probe::new(&fx);
    let e = refused(&fx, &probe, &fx.input_any(&handle, "a")).await;
    assert_eq!(e.code, "desktop_window_not_found", "{e:?}");
    assert!(
        !fx.timeline().iter().any(|l| l.contains("focuswindow")),
        "{:?}",
        fx.timeline()
    );
    assert!(fx.calls_of("wtype").is_empty());
}

/// The owner switches away from the workspace of a listed window after the
/// listing: the input is refused and the station never dispatches a focus
/// (R13 P7b §8: no workspace switch).
#[tokio::test]
async fn a_listed_window_on_a_workspace_that_is_no_longer_shown_is_refused() {
    let (fx, handle) = listed_shell().await;
    fx.write("monitors.json", &fx.monitors_away());
    let probe = Probe::new(&fx);
    nothing(
        &fx,
        &probe,
        &fx.input_any(&handle, "a"),
        "desktop_window_not_found",
    )
    .await;
    assert!(
        !fx.timeline().iter().any(|l| l.contains("focuswindow")),
        "{:?}",
        fx.timeline()
    );
    assert!(fx.calls_of("wtype").is_empty());
    // A window that is hidden or no longer mapped is refused the same way.
    for (key, value) in [("hidden", true), ("mapped", false)] {
        let (fx, handle) = listed_shell().await;
        let mut shell = other_client();
        shell[key] = json!(value);
        fx.set_clients(&[fx.preview_client(), shell]);
        let probe = Probe::new(&fx);
        nothing(
            &fx,
            &probe,
            &fx.input_any(&handle, "a"),
            "desktop_window_not_found",
        )
        .await;
    }
}

// ----------------------------------------------------------- interruption

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: {what}");
}

/// Runs `action` against a `wtype` that stalls, and `disturb` once it runs.
async fn disturbed(fx: &Fx, probe: &Probe, action: &A, disturb: impl FnOnce()) -> Fail {
    fx.flag("wtype_stall", true);
    let root = fx.root();
    let driver = async {
        until("the child runs", || root.join("wtype.child").exists()).await;
        disturb();
    };
    let (result, ()) = tokio::join!(fx.run(action, probe), driver);
    result.unwrap_err()
}

#[tokio::test]
async fn a_close_a_new_epoch_or_the_end_of_the_life_interrupts_the_input() {
    // A close of the listed window.
    let (fx, handle) = listed_shell().await;
    let events = fx.desktop().events.clone();
    let e = disturbed(&fx, &Probe::new(&fx), &fx.input_any(&handle, "a"), || {
        events.inject(Event::Close {
            address: "0xbeef".into(),
        });
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    // A gap in the event stream.
    let (fx, handle) = listed_shell().await;
    let events = fx.desktop().events.clone();
    let e = disturbed(&fx, &Probe::new(&fx), &fx.input_any(&handle, "a"), || {
        events.inject_gap();
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    // The expiry of the handle.
    let (fx, handle) = listed_shell().await;
    let jump = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&jump);
    let mut probe = Probe::new(&fx);
    probe.clock = Some(Arc::new(move || {
        let now = Instant::now();
        if seen.load(Ordering::SeqCst) {
            now + Duration::from_secs(61)
        } else {
            now
        }
    }));
    let e = disturbed(&fx, &probe, &fx.input_any(&handle, "a"), || {
        jump.store(true, Ordering::SeqCst);
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert!(!fx.desktop().input.is_held());
}

// ----------------------------------------------------------------- clicks

#[tokio::test]
async fn the_click_gates_come_first_and_the_broader_gate_guards_only_its_variants() {
    let fx = Fx::new().await;
    let clicks = [
        A::InputPointerClick {
            window_id: fx.window_id.clone(),
            x: 5,
            y: 5,
        },
        A::InputAnyPointerClick {
            window_id: fx.window_id.clone(),
            x: 5,
            y: 5,
        },
    ];
    let mut no_click = Probe::new(&fx);
    no_click.live.desktop_click = false;
    for action in &clicks {
        nothing(&fx, &no_click, action, "input_click_disabled").await;
    }
    let mut no_any = Probe::new(&fx);
    no_any.live.desktop_input_any = false;
    nothing(&fx, &no_any, &clicks[1], "input_any_disabled").await;
    // A click in a preview does not read the broader gate.
    fx.run(&clicks[0], &no_any).await.unwrap();
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
}
