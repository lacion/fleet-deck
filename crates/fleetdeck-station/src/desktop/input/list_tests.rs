//! The `window_list` read on the fake desktop (R13 P7b §6, §6.1): the closed
//! projection, the eligibility rules, the limits, the order of the checks and
//! the exposure of the handles.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use serde_json::{Value, json};

use crate::desktop::Fail;
use crate::desktop::coordinator::{Live, fleet_off, lost};
use crate::desktop::coordinator_tests::{FakeCheck, ctx};
use crate::desktop::input::rig::{Fx, all_on, other_client};

type LiveFn = Arc<dyn Fn() -> Live + Send + Sync>;

fn live_of(live: Live) -> LiveFn {
    Arc::new(move || live.clone())
}

async fn list(fx: &Fx, live: LiveFn) -> Result<Value, Fail> {
    fx.list(&*live, &*FakeCheck::ok(), Some(ctx())).await
}

/// The rectangles of the listed windows, as `(x, y)`.
fn origins(listing: &Value) -> Vec<(i64, i64)> {
    let mut found: Vec<(i64, i64)> = listing["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["rect"]["x"].as_i64().unwrap(),
                w["rect"]["y"].as_i64().unwrap(),
            )
        })
        .collect();
    found.sort_unstable();
    found
}

fn reads(fx: &Fx) -> usize {
    fx.rig.reads().len()
}

// ------------------------------------------------------------ projection

#[tokio::test]
async fn the_answer_is_the_closed_projection_with_geometry_and_handles() {
    let fx = Fx::new().await;
    let mut shell = other_client();
    shell["focusHistoryID"] = json!(0);
    fx.set_clients(&[fx.preview_client(), shell]);
    let listing = fx.listing().await;
    assert_eq!(listing["station"], "target");
    assert_eq!(
        listing["monitors"],
        json!([
            {"name": "DP-1", "x": 0, "y": 0, "width": 3440, "height": 1440},
            {"name": "HDMI-A-1", "x": 3440, "y": 0, "width": 1920, "height": 1080}
        ])
    );
    let windows = listing["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    let preview = &windows[0];
    assert_eq!(
        preview["rect"],
        json!({"x": 100, "y": 100, "width": 800, "height": 600})
    );
    assert_eq!(preview["monitors"], json!(["DP-1"]));
    assert_eq!(preview["focused"], false);
    assert_eq!(preview["expires_in_ms"], 60_000);
    let shell = &windows[1];
    assert_eq!(shell["focused"], true);
    for w in windows {
        let id = w["window_id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        let mut keys: Vec<&String> = w.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["expires_in_ms", "focused", "monitors", "rect", "window_id"]
        );
    }
    assert_eq!(fx.desktop().listed().len(), 2);
}

#[tokio::test]
async fn no_title_class_address_or_pid_reaches_the_answer() {
    let fx = Fx::new().await;
    let mut shell = other_client();
    shell["title"] = json!("SENTINEL-TITLE");
    shell["initialTitle"] = json!("SENTINEL-INITIAL-TITLE");
    shell["class"] = json!("sentinel-class");
    shell["initialClass"] = json!("sentinel-initial-class");
    shell["address"] = json!("0xdeadbeefcafe");
    shell["pid"] = json!(424_242);
    shell["workspace"] = json!({"id": 1, "name": "SENTINEL-WORKSPACE"});
    shell["focusHistoryID"] = json!(0);
    shell["xwayland"] = json!("SENTINEL-URL");
    fx.set_clients(&[shell]);
    let listing = fx.listing().await;
    let text = listing.to_string();
    for sentinel in [
        "SENTINEL",
        "sentinel",
        "deadbeef",
        "424242",
        "kitty",
        "alacritty",
    ] {
        assert!(!text.contains(sentinel), "{sentinel} in {text}");
    }
    assert_eq!(listing["windows"].as_array().unwrap().len(), 1);
}

// ----------------------------------------------------------- eligibility

#[tokio::test]
async fn only_mapped_visible_shown_positive_windows_are_listed() {
    let fx = Fx::new().await;
    let mut hidden = Fx::window("0x11", (10, 10), (50, 50), 1);
    hidden["hidden"] = json!(true);
    let mut unmapped = Fx::window("0x12", (20, 20), (50, 50), 1);
    unmapped["mapped"] = json!(false);
    let elsewhere = Fx::window("0x13", (30, 30), (50, 50), 9);
    let flat = Fx::window("0x14", (40, 40), (0, 50), 1);
    let thin = Fx::window("0x15", (50, 50), (50, -3), 1);
    let special_hidden = Fx::window("0x16", (60, 60), (50, 50), -98);
    let special_shown = Fx::window("0x17", (70, 70), (50, 50), -99);
    let side = Fx::window("0x18", (3500, 10), (50, 50), 5);
    let huge = Fx::window("0x19", (5_000_000_000, 80), (50, 50), 1);
    let wide = Fx::window("0x1a", (90, 90), (5_000_000_000, 50), 1);
    let nowhere = Fx::window("0x1b", (-9000, -9000), (50, 50), 1);
    let good = Fx::window("0x1c", (200, 200), (50, 50), 1);
    fx.set_clients(&[
        hidden,
        unmapped,
        elsewhere,
        flat,
        thin,
        special_hidden,
        special_shown,
        side,
        huge,
        wide,
        nowhere,
        good,
    ]);
    // A monitor that shows the special workspace -99 (the other shows none).
    let mut monitors: Value = serde_json::from_str(&fx.read("monitors.json")).unwrap();
    monitors[0]["specialWorkspace"] = json!({"id": -99, "name": "special:a"});
    monitors[1]["specialWorkspace"] = json!({"id": 0, "name": ""});
    fx.write("monitors.json", &monitors);
    let listing = fx.listing().await;
    assert_eq!(origins(&listing), [(70, 70), (200, 200), (3500, 10)]);
}

#[tokio::test]
async fn board_wall_and_deck_windows_are_left_out_without_a_count() {
    let fx = Fx::new().await;
    let wall = crate::desktop::tests::run(
        &fx.rig,
        fleetdeck_proto::desktop::DesktopAction::WallOpen {
            display_id: "d1".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: crate::desktop::tests::url(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    // Only the wall record names this window as a wall: its class is plain.
    let by_record = Fx::window(&wall, (300, 300), (50, 50), 1);
    let mut by_title = Fx::window("0x21", (310, 310), (50, 50), 1);
    by_title["title"] = json!("Fleet Deck - board");
    let mut by_initial = Fx::window("0x22", (320, 320), (50, 50), 1);
    by_initial["initialTitle"] = json!("my fleetdeck");
    let mut by_class = Fx::window("0x23", (330, 330), (50, 50), 1);
    by_class["class"] = json!("fleetdeck-board-1");
    let mut by_host = Fx::window("0x24", (340, 340), (50, 50), 1);
    by_host["title"] = json!("Board at deck.example:8443");
    let mut marked = fx.preview_client();
    marked["title"] = json!("fine");
    let good = Fx::window("0x25", (350, 350), (50, 50), 1);
    fx.set_clients(&[
        by_record, by_title, by_initial, by_class, by_host, marked, good,
    ]);
    fx.desktop().mark_board_origin(&fx.window_id);
    let live = live_of(all_on());
    let hosts = vec!["deck.example".to_owned()];
    let listing = fx
        .list_with(&*live, &*FakeCheck::ok(), Some(ctx()), &hosts)
        .await
        .unwrap();
    assert_eq!(origins(&listing), [(350, 350)]);
}

#[tokio::test]
async fn a_window_on_a_monitor_that_is_not_permitted_is_left_out() {
    let fx = Fx::new().await;
    let left = Fx::window("0x31", (10, 10), (50, 50), 1);
    let right = Fx::window("0x32", (3500, 10), (50, 50), 5);
    // Spans the border of the two monitors.
    let both = Fx::window("0x33", (3400, 300), (100, 50), 1);
    fx.set_clients(&[left, right, both]);
    let mut live = all_on();
    live.monitors = vec!["DP-1".to_owned()];
    let listing = list(&fx, live_of(live)).await.unwrap();
    // The window that overlaps HDMI-A-1 is out, also when it overlaps DP-1.
    assert_eq!(origins(&listing), [(10, 10)]);
    assert_eq!(
        listing["monitors"].as_array().unwrap().len(),
        1,
        "{listing}"
    );
    assert_eq!(listing["monitors"][0]["name"], "DP-1");
    // An empty list permits every monitor.
    let all = list(&fx, live_of(all_on())).await.unwrap();
    assert_eq!(origins(&all), [(10, 10), (3400, 300), (3500, 10)]);
    assert_eq!(all["windows"][2]["monitors"], json!(["DP-1", "HDMI-A-1"]));
}

// ---------------------------------------------------------------- limits

#[tokio::test]
async fn sixty_four_windows_fit_and_sixty_five_do_not() {
    let fx = Fx::new().await;
    let many = |n: i64| -> Vec<Value> {
        (0..n)
            .map(|i| Fx::window(&format!("0x{:x}", 0x100 + i), (i * 40, 10), (30, 30), 1))
            .collect()
    };
    fx.set_clients(&many(64));
    let ok = fx.listing().await;
    assert_eq!(ok["windows"].as_array().unwrap().len(), 64);
    let held = fx.desktop().listed().len();
    fx.set_clients(&many(65));
    let e = list(&fx, live_of(all_on())).await.unwrap_err();
    assert_eq!(e.code, "desktop_result_too_large");
    assert_eq!(fx.desktop().listed().len(), held, "no handle was issued");
}

#[tokio::test]
async fn sixty_five_monitors_are_too_large() {
    let fx = Fx::new().await;
    let monitors: Vec<Value> = (0..65)
        .map(|i| {
            json!({"id": i, "name": format!("M-{i}"), "width": 100, "height": 100,
                   "x": i * 100, "y": 0, "scale": 1.0, "transform": 0,
                   "activeWorkspace": {"id": 1, "name": "1"}})
        })
        .collect();
    fx.write("monitors.json", &Value::Array(monitors));
    let e = list(&fx, live_of(all_on())).await.unwrap_err();
    assert_eq!(e.code, "desktop_result_too_large");
    assert_eq!(fx.desktop().listed().len(), 0);
}

#[tokio::test]
async fn an_answer_over_sixteen_kib_is_too_large_and_leaves_no_handle() {
    let fx = Fx::new().await;
    // 64 monitors on one spot and 64 windows over all of them: every window
    // names 64 monitors.
    let monitors: Vec<Value> = (0..64)
        .map(|i| {
            json!({"id": i, "name": format!("MONITOR-WITH-A-LONG-NAME-{i:03}"),
                   "width": 1000, "height": 1000, "x": 0, "y": 0, "scale": 1.0,
                   "transform": 0, "activeWorkspace": {"id": 1, "name": "1"}})
        })
        .collect();
    fx.write("monitors.json", &Value::Array(monitors));
    let clients: Vec<Value> = (0..64)
        .map(|i| Fx::window(&format!("0x{:x}", 0x200 + i), (i, i), (30, 30), 1))
        .collect();
    fx.set_clients(&clients);
    let e = list(&fx, live_of(all_on())).await.unwrap_err();
    assert_eq!(e.code, "desktop_result_too_large");
    assert_eq!(fx.desktop().listed().len(), 0, "the handles were removed");
}

// ------------------------------------------------------ order and refusals

#[tokio::test]
async fn an_epoch_change_during_the_read_fails_without_a_handle_or_a_check() {
    let fx = Fx::new().await;
    let desktop = Arc::clone(fx.desktop());
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let live = all_on();
    // The gate is read before each program run; a gap in the event stream
    // happens while the snapshot is taken.
    let live_now: LiveFn = Arc::new(move || {
        if counter.fetch_add(1, Ordering::SeqCst) == 2 {
            desktop.events.inject_gap();
        }
        live.clone()
    });
    let check = FakeCheck::ok();
    let e = fx.list(&*live_now, &*check, Some(ctx())).await.unwrap_err();
    assert_eq!(e.code, "desktop_failed");
    assert!(check.phases().is_empty(), "no authority check after a gap");
    assert_eq!(fx.desktop().listed().len(), 0);
}

#[tokio::test]
async fn the_authority_check_runs_after_the_inspection_and_before_any_handle() {
    let fx = Fx::new().await;
    let desktop = Arc::clone(fx.desktop());
    let root = fx.root();
    let seen: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&seen);
    let check = FakeCheck::with_hook(Ok(()), move || {
        let reads = std::fs::read_to_string(root.join("reads.log")).unwrap_or_default();
        let clients = reads.matches("clients").count();
        *slot.lock().unwrap() = Some((clients, desktop.listed().len()));
    });
    fx.list(&*live_of(all_on()), &*check, Some(ctx()))
        .await
        .unwrap();
    // One `clients` read happened, and no handle existed at the check.
    assert_eq!(*seen.lock().unwrap(), Some((1, 0)));
    assert_eq!(check.phases(), [AuthorityPhase::Read]);
    let seen_read = check.seen.lock().unwrap();
    assert_eq!(
        seen_read[0].1,
        Some(fleetdeck_proto::coordinator_desktop_wire::ProtectedRead::InputWindows)
    );
}

#[tokio::test]
async fn a_refused_authority_check_issues_no_handle() {
    let fx = Fx::new().await;
    let check = FakeCheck::answering(Err(lost()));
    let e = fx
        .list(&*live_of(all_on()), &*check, Some(ctx()))
        .await
        .unwrap_err();
    assert_eq!(e, lost());
    assert_eq!(fx.desktop().listed().len(), 0);
}

#[tokio::test]
async fn a_gate_that_turns_off_during_the_check_issues_no_handle() {
    let fx = Fx::new().await;
    let off = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&off);
    let check = FakeCheck::with_hook(Ok(()), move || flag.store(true, Ordering::SeqCst));
    let read = Arc::clone(&off);
    let live_now: LiveFn = Arc::new(move || {
        let mut live = all_on();
        live.desktop_input_any = !read.load(Ordering::SeqCst);
        live
    });
    let e = fx.list(&*live_now, &*check, Some(ctx())).await.unwrap_err();
    assert_eq!(e.code, "input_any_disabled");
    assert_eq!(fx.desktop().listed().len(), 0);
    // A monitor list that no longer permits a listed monitor refuses too.
    let off = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&off);
    let check = FakeCheck::with_hook(Ok(()), move || flag.store(true, Ordering::SeqCst));
    let read = Arc::clone(&off);
    let live_now: LiveFn = Arc::new(move || {
        let mut live = all_on();
        if read.load(Ordering::SeqCst) {
            live.monitors = vec!["DP-9".to_owned()];
        }
        live
    });
    let e = fx.list(&*live_now, &*check, Some(ctx())).await.unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert_eq!(fx.desktop().listed().len(), 0);
}

#[tokio::test]
async fn a_locked_screen_refuses_with_screen_locked_and_no_handle() {
    let fx = Fx::new().await;
    fx.rig.flag("hyprlock", true);
    let check = FakeCheck::ok();
    let e = fx
        .list(&*live_of(all_on()), &*check, Some(ctx()))
        .await
        .unwrap_err();
    assert_eq!(e.code, "screen_locked");
    assert!(check.phases().is_empty());
    assert_eq!(fx.desktop().listed().len(), 0);
}

#[tokio::test]
async fn each_gate_and_a_missing_context_refuse_before_the_desktop_is_read() {
    let fx = Fx::new().await;
    type TurnOff = fn(&mut Live);
    let cases: [(TurnOff, &str); 5] = [
        (|l| l.fleet_control = false, fleet_off().code),
        (|l| l.desktop = false, "forbidden"),
        (|l| l.desktop_input = false, "forbidden"),
        (|l| l.desktop_input_any = false, "input_any_disabled"),
        (|l| l.capture = false, "forbidden"),
    ];
    for (turn_off, code) in cases {
        let mut live = all_on();
        turn_off(&mut live);
        let check = FakeCheck::ok();
        let e = fx
            .list(&*live_of(live), &*check, Some(ctx()))
            .await
            .unwrap_err();
        assert_eq!(e.code, code);
        assert!(check.phases().is_empty());
    }
    // The click gate is not a gate of the listing.
    let mut live = all_on();
    live.desktop_click = false;
    list(&fx, live_of(live)).await.unwrap();
    // No coordinator context: `desktop_failed`.
    fx.clear_logs();
    let e = fx
        .list(&*live_of(all_on()), &*FakeCheck::ok(), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "desktop_failed");
    assert_eq!(reads(&fx), 0);
}

// ------------------------------------------------------------ the handles

#[tokio::test]
async fn a_second_listing_reuses_the_handles_and_never_extends_their_life() {
    let fx = Fx::new().await;
    let first = fx.listing().await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let second = fx.listing().await;
    for (a, b) in first["windows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(second["windows"].as_array().unwrap())
    {
        assert_eq!(a["window_id"], b["window_id"]);
        assert!(b["expires_in_ms"].as_u64().unwrap() < a["expires_in_ms"].as_u64().unwrap());
    }
    assert_eq!(fx.desktop().listed().len(), 2);
}

#[tokio::test]
async fn a_listed_handle_is_not_accepted_by_window_close() {
    use fleetdeck_proto::desktop::DesktopAction as A;
    let fx = Fx::new().await;
    let handle = fx.other_handle().await;
    fx.clear_logs();
    let action = A::PreviewClose {
        window_id: handle.clone(),
    };
    let h = crate::desktop::authority_tests::Harness::new();
    let admitted = crate::desktop::coordinator_tests::coordinated(
        &h,
        &action,
        7,
        &ctx(),
        FakeCheck::ok(),
        Arc::new(|| Ok(())),
        crate::desktop::coordinator_tests::extras(),
    );
    let e = fx
        .desktop()
        .act(&crate::desktop::tests::on, action, admitted)
        .await
        .unwrap_err();
    assert_eq!(e.code, "desktop_window_not_found");
    assert!(fx.calls_of("hyprctl").is_empty(), "nothing was closed");
    // The handle of the preview stays what it was.
    assert!(fx.desktop().events.preview(&handle).is_none());
}
