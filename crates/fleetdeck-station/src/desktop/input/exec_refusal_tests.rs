//! The refusals of the input protocol (R13 P7 §7.1, §7.2, §7.3, §13): the
//! preview ownership, the board barrier, the focus and age checks and the
//! observations that fail closed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::DesktopAction as A;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::json;

use super::rig::{
    Fx, Probe, calls, foreign_active, nothing, other_client, pair, refused, scripted_clock,
};
use crate::desktop::coordinator::Extras;
use crate::desktop::coordinator_tests::{DECK, ctx};
use crate::desktop::events::Kind;

const NOT_FOUND: &str = "No window opened by this grant matches that handle.";

// ------------------------------------------------- the preview ownership

fn other_contexts() -> Vec<(CoordinatorContext, Extras)> {
    let base = ctx();
    let link = |fp: &str| Extras {
        deck_fp: fp.to_owned(),
        tailnet_host: None,
    };
    vec![
        (
            CoordinatorContext {
                caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
                ..base.clone()
            },
            link(DECK),
        ),
        (
            CoordinatorContext {
                grant_id: "grant-2".into(),
                ..base.clone()
            },
            link(DECK),
        ),
        (
            CoordinatorContext {
                caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
                ..base
            },
            link(DECK),
        ),
        (ctx(), link(&"cd".repeat(32))),
    ]
}

#[tokio::test]
async fn a_foreign_or_unknown_handle_gets_one_answer_and_nothing_runs() {
    let fx = Fx::new().await;
    let mut answers = Vec::new();
    for (context, extras) in other_contexts() {
        let mut probe = Probe::new(&fx);
        probe.context = context;
        probe.extras = extras;
        let e = refused(&fx, &probe, &fx.key("a")).await;
        assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
        answers.push(pair(&e));
    }
    // An unknown handle, and a handle of a window that the owner opened (it
    // has no preview record).
    let probe = Probe::new(&fx);
    for handle in ["ab".repeat(16), "00".repeat(16)] {
        let action = A::InputKeyPress {
            window_id: handle,
            chord: "a".into(),
        };
        answers.push(pair(&refused(&fx, &probe, &action).await));
    }
    assert!(
        answers
            .iter()
            .all(|a| *a == ("desktop_window_not_found", NOT_FOUND.into())),
        "{answers:?}"
    );
    assert_eq!(calls(&fx), 0);
}

#[tokio::test]
async fn a_window_that_is_gone_from_the_compositor_is_not_found() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.set_clients(&[other_client()]);
    nothing(&fx, &probe, &fx.key("a"), "desktop_window_not_found").await;
    // The same address with another class is another window: never found by
    // a similar class or title.
    let mut changed = fx.preview_client();
    changed["class"] = json!("chromium");
    fx.set_clients(&[changed]);
    nothing(&fx, &probe, &fx.key("a"), "desktop_window_not_found").await;
}

#[tokio::test]
async fn a_preview_that_leaves_the_registry_stops_the_protocol() {
    for n in [2_usize, 3] {
        let fx = Fx::new().await;
        let mut probe = Probe::new(&fx);
        let desktop = Arc::clone(fx.desktop());
        let address = fx.record.address.clone();
        probe.check = fx.check(
            None,
            Some(Box::new(move |k| {
                if k == n {
                    desktop.events.forget(&address);
                }
            })),
        );
        let e = refused(&fx, &probe, &fx.key("a")).await;
        assert_eq!(e.code, "desktop_window_not_found", "check {n}");
        assert!(fx.calls_of("wtype").is_empty());
        // Step 11 revalidates right after the check: no focus. Step 17 ends
        // the protocol after the final read: no spawn.
        let focused = fx.timeline().contains(&fx.focus_line());
        assert_eq!(focused, n == 3, "check {n}: {:?}", fx.timeline());
    }
}

// ----------------------------------------------------- the board barrier

#[tokio::test]
async fn a_board_like_title_is_refused_before_the_claim_and_the_disclosure() {
    for title in [
        "Fleet Deck - board",
        "my fleetdeck",
        "Board at deck.example:8443",
    ] {
        let fx = Fx::new().await;
        let mut probe = Probe::new(&fx);
        probe.deck_hosts = vec!["deck.example".into()];
        let mut client = fx.preview_client();
        client["title"] = json!(title);
        fx.set_clients(&[client]);
        nothing(&fx, &probe, &fx.key("a"), "input_board_forbidden").await;
    }
    // The initial title counts too.
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let mut client = fx.preview_client();
    client["initialTitle"] = json!("FLEET DECK");
    fx.set_clients(&[client]);
    let e = refused(&fx, &probe, &fx.pointer(1, 1)).await;
    assert_eq!(
        pair(&e),
        (
            "input_board_forbidden",
            "Fleet Deck board and wall windows cannot receive agent input.".into()
        )
    );
}

#[tokio::test]
async fn a_wall_record_is_a_board_window_even_when_the_handle_is_owned() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    // A wall window that the station opened, under a preview handle of the
    // caller: the record kind and the class `fleetdeck-wall-*` both refuse.
    let address = crate::desktop::tests::run(
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
    let wall = fx.desktop().events.record(&address).unwrap();
    assert_eq!(wall.kind, Kind::Wall);
    let owner = crate::desktop::coordinator::preview_owner(DECK, &ctx());
    fx.desktop()
        .events
        .register_preview(&"cd".repeat(16), owner, "bold-fox", &address)
        .unwrap();
    fx.set_clients(&[json!({
        "address": address, "class": wall.class, "title": "wall", "initialClass": wall.class,
        "initialTitle": "wall", "mapped": true, "hidden": false,
        "at": [0, 0], "size": [800, 600], "workspace": {"id": 1}
    })]);
    fx.clear_logs();
    let action = A::InputKeyPress {
        window_id: "cd".repeat(16),
        chord: "a".into(),
    };
    nothing(&fx, &probe, &action, "input_board_forbidden").await;
}

#[tokio::test]
async fn a_title_that_turns_into_a_board_title_is_refused_by_the_final_check() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.set_active(&fx.preview_active("Fleet Deck"));
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "input_board_forbidden");
    // The disclosure and the focus already happened; no input started.
    assert!(fx.timeline().contains(&fx.focus_line()));
    assert!(fx.calls_of("wtype").is_empty());
}

// --------------------------------------------------- focus and its ages

#[tokio::test]
async fn a_focus_change_that_the_final_check_sees_is_refused_before_the_spawn() {
    for foreign in [
        foreign_active(),
        // A foreign board window: the same generic answer, no mention of it.
        json!({"address": "0xdead", "class": "fleetdeck-board", "title": "Fleet Deck",
               "initialClass": "fleetdeck-board", "initialTitle": "Fleet Deck"}),
        json!({}),
    ] {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        fx.focus_goes_to(&foreign);
        let e = refused(&fx, &probe, &fx.text("x")).await;
        assert_eq!(
            pair(&e),
            (
                "focus_changed",
                "The preview no longer has focus. Input stopped; some input may already have been sent."
                    .into()
            )
        );
        assert!(fx.calls_of("wtype").is_empty());
        assert_eq!(fx.timeline().last().unwrap(), "hyprctl -j activewindow");
    }
}

#[tokio::test]
async fn an_authority_answer_older_than_500_ms_refuses_the_spawn() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    // The answer at 0, the focus snapshot at 590, the final checks at 600.
    probe.clock = Some(scripted_clock(&[0, 590, 600]));
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "authority_unknown");
    assert!(fx.calls_of("wtype").is_empty());
}

#[tokio::test]
async fn a_focus_snapshot_older_than_100_ms_refuses_the_spawn() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(scripted_clock(&[0, 50, 160]));
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(e.code, "input_focus_unknown");
    assert_eq!(
        e.message,
        "The preview's input focus could not be verified. Input was not started."
    );
    assert!(fx.calls_of("wtype").is_empty());
}

#[tokio::test]
async fn an_active_window_read_that_fails_stalls_or_is_malformed_fails_closed() {
    for flag in ["active_fail", "active_garbage", "active_stall"] {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        fx.flag(flag, true);
        let started = std::time::Instant::now();
        let e = refused(&fx, &probe, &fx.key("a")).await;
        assert_eq!(e.code, "input_focus_unknown", "{flag}");
        assert!(fx.calls_of("wtype").is_empty(), "{flag}");
        // The stall ends at the 250 ms limit, not after the 5 s of the fake.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{flag}"
        );
    }
}

// ----------------------------------------------- the observations (5, 6)

#[tokio::test]
async fn a_locked_or_unknown_lock_state_refuses_before_the_claim() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("hyprlock", true);
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(
        pair(&e),
        (
            "screen_locked",
            "The screen is locked or its lock state is unknown.".into()
        )
    );
    assert_eq!(calls(&fx), 0);
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    // `pgrep` is not there: the lock state is unknown, which is never
    // "unlocked".
    fx.flag("hyprlock", false);
    std::fs::remove_file(fx.root().join("bin/pgrep")).unwrap();
    nothing(&fx, &probe, &fx.key("a"), "screen_locked").await;
}

#[tokio::test]
async fn a_monitor_outside_capture_monitors_refuses_and_a_listed_one_passes() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.live.monitors = vec!["HDMI-A-1".into()];
    let e = refused(&fx, &probe, &fx.key("a")).await;
    assert_eq!(
        pair(&e),
        (
            "forbidden",
            "This monitor is not allowed for screenshots.".into()
        )
    );
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    assert_eq!(calls(&fx), 0);
    probe.live.monitors = vec!["DP-1".into()];
    fx.run(&fx.key("a"), &probe).await.unwrap();
    // A preview over two monitors needs both.
    let mut wide = fx.preview_client();
    wide["at"] = json!([3300, 100]);
    wide["size"] = json!([400, 300]);
    fx.set_clients(&[wide]);
    fx.clear_logs();
    let probe2 = Probe::new(&fx);
    let mut only_one = probe2;
    only_one.live.monitors = vec!["DP-1".into()];
    assert_eq!(
        refused(&fx, &only_one, &fx.key("a")).await.code,
        "forbidden"
    );
    only_one.live.monitors = vec!["DP-1".into(), "HDMI-A-1".into()];
    fx.run(&fx.key("a"), &only_one).await.unwrap();
}

#[tokio::test]
async fn an_unreadable_snapshot_fails_closed_with_no_claim() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    for garbage in ["not json", "{}", "[{\"address\":\"0x1\"}]", ""] {
        fx.write_text("clients.json", garbage);
        nothing(&fx, &probe, &fx.key("a"), "input_focus_unknown").await;
    }
    fx.set_clients(&[fx.preview_client()]);
    fx.write_text("monitors.json", "[{\"name\":\"DP-1\"}]");
    nothing(&fx, &probe, &fx.key("a"), "input_focus_unknown").await;
    fx.write_text("monitors.json", "[]");
    nothing(&fx, &probe, &fx.key("a"), "input_focus_unknown").await;
}

#[tokio::test]
async fn a_preview_launched_on_a_deck_origin_never_gets_input() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.desktop().mark_board_origin(&fx.window_id);
    // The marked handle is owned, so the answer names the board: before the
    // claim, the disclosure and the focus, for every kind of input.
    for action in [fx.key("a"), fx.text("x"), fx.pointer(1, 1)] {
        nothing(&fx, &probe, &action, "input_board_forbidden").await;
    }
}
