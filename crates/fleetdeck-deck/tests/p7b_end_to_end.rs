//! The P7b contract end to end over real links (R13 P7b task P): a caller
//! station A (real station crate, real coordinator MCP endpoint), the deck
//! (real deck crate) and a target station B (real station crate with a fake
//! desktop: a fake `hyprctl`, `wtype` and `wlrctl`). The crate tests of the
//! station and the deck are not repeated here; each test names the items of
//! `docs/v2/design/omarchy-p7b.md` §16 that it shows.
//!
//! As in the P7 proofs, B and the board client both come from `127.0.0.1`: an
//! input operation exposes the address of the board client and the deck signs
//! the owner board session out (§12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod p6_support;
mod settings_support;

use fleetdeck_proto::version::feature;
use p6_support::input_rig_p7b::{self as rig7b, SENTINEL_CLASS, SENTINEL_TITLE};
use p6_support::mcp::Tool;
use p6_support::*;
use serde_json::{Value, json};

const CAPS: (u32, u32) = (20, 1000);

/// A world with the input allowance, the click allowance and/or the scope
/// `any_window`, and one preview of the caller on B. Returns the world and
/// the `window_id` of the preview.
async fn world_with(click: bool, any_window: bool) -> (World, String) {
    let w = World::with(Setup {
        input: Some(CAPS),
        click,
        any_window,
        ..Setup::default()
    })
    .await;
    let window = w.open_preview("p1").await;
    (w, window)
}

/// The names of the changing programs on B since the preview opened, with the
/// first argument of each `hyprctl`.
fn order(w: &World) -> Vec<String> {
    w.b.rig
        .calls()
        .into_iter()
        .map(|c| match c.program.as_str() {
            "hyprctl" => format!("hyprctl {}", c.args[..2].join(" ")),
            other => other.to_owned(),
        })
        .collect()
}

/// A call that finished: the answer of a done input is `{}` and the status
/// tool says `done` with an empty result.
async fn done(w: &World, id: &str, out: &Tool) {
    assert_eq!(out.ok(), json!({}), "{}", out.raw);
    let status = w
        .mcp
        .call("desktop_action_status", json!({"request_id": id}))
        .await
        .ok();
    assert_eq!(status["state"], "done", "{status}");
    assert_eq!(status["result"], json!({}), "{status}");
}

/// A call that must be an error with the fixed `code`.
fn refused(out: &Tool, code: &str) {
    assert_eq!(out.code(), code, "{}", out.raw);
}

fn nothing_ran(w: &World) {
    assert!(w.b.rig.calls().is_empty(), "{:?}", w.b.rig.calls());
}

// -------------------------------------------------------------- proof 1

/// §16 Behavior and §10.2: `pointer_click` in the preview of the caller. The
/// notification comes first, then the focus, then one `movecursor` at the
/// preview origin plus the point, then `wlrctl pointer click left`; the click
/// notification text is the fixed one; the result is done; the grant shows one
/// input action, no typed character and one more desktop action (§11.3).
#[tokio::test]
async fn pointer_click_notifies_focuses_moves_then_clicks_once() {
    let (w, window) = world_with(true, false).await;
    let before = w.grant().await;
    let out = w
        .mcp
        .call("pointer_click", w.move_args("c1", &window, 10, 20))
        .await;
    done(&w, "c1", &out).await;
    for text in [window.as_str(), "0x", "110", "120"] {
        assert!(!out.raw.contains(text), "the answer echoes `{text}`");
    }

    assert_eq!(
        order(&w),
        [
            "notify-send",
            "hyprctl dispatch focuswindow",
            "hyprctl dispatch movecursor",
            "wlrctl"
        ],
        "{:?}",
        w.b.rig.calls()
    );
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Fleet Deck",
            "Fleet Deck is clicking for swift-fox in an agent preview."
        ]
    );
    let (ox, oy) = input_rig::PREVIEW_AT;
    assert_eq!(
        input_rig::move_calls(&w.b.rig),
        [[
            "dispatch".to_owned(),
            "movecursor".to_owned(),
            (ox + 10).to_string(),
            (oy + 20).to_string()
        ]]
    );
    assert_eq!(input_rig::focus_calls(&w.b.rig).len(), 1);
    assert_eq!(
        rig7b::wlrctl_calls(&w.b.rig),
        [["pointer", "click", "left"].map(str::to_owned).to_vec()]
    );
    assert!(w.b.rig.calls_of("wtype").is_empty());

    let after = w.grant().await;
    assert_eq!(after.input_used, before.input_used + 1);
    assert_eq!(after.input_character_used, 0);
    assert_eq!(after.desktop_used, before.desktop_used + 1);
}

// -------------------------------------------------------------- proof 2

/// §16 Behavior and §4: a world without the click allowance (the P7 grant)
/// lists no `pointer_click`; a call is an error, and nothing runs on B.
#[tokio::test]
async fn pointer_click_without_the_click_allowance_is_not_listed_and_runs_nothing() {
    let (w, window) = world_with(false, false).await;
    let names = w.mcp.names().await;
    assert!(names.iter().any(|n| n == "key_press"), "{names:?}");
    assert!(!names.iter().any(|n| n == "pointer_click"), "{names:?}");
    let out = w
        .mcp
        .call("pointer_click", w.move_args("c1", &window, 10, 20))
        .await;
    assert!(out.is_error, "{}", out.raw);
    nothing_ran(&w);
    assert!(rig7b::wlrctl_calls(&w.b.rig).is_empty());
    assert_eq!(w.grant().await.input_used, 0);
}

// -------------------------------------------------------------- proof 3

/// §16 Behavior and Exposure, §6: `window_list` under `any_window` gives the
/// preview and the two ordinary windows with opaque 32-hex handles, the
/// monitors, `focused` and `expires_in_ms`, and nothing else. The sentinel
/// title and class are in no answer, no deck audit row, no stored row, no log
/// event and no log of the fake desktop; the handles are the only window
/// names of the answer.
#[tokio::test]
async fn window_list_gives_opaque_handles_and_geometry_and_no_title_or_class() {
    let (w, preview) = world_with(false, true).await;
    rig7b::add_ordinary_windows(&w.b.rig);
    let out = w.window_list().await;
    let listing = out.ok();
    assert_eq!(listing["station"], w.to_b(), "{listing}");
    assert_eq!(
        listing["monitors"],
        json!([
            {"name": "DP-1", "x": 0, "y": 0, "width": 3440, "height": 1440},
            {"name": "HDMI-A-1", "x": 3440, "y": 0, "width": 1920, "height": 1080}
        ]),
        "{listing}"
    );
    let windows = listing["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3, "{listing}");
    let (px, py) = input_rig::PREVIEW_AT;
    let (pw, ph) = input_rig::PREVIEW_SIZE;
    let want = [
        ((px, py), (pw, ph), true),
        (rig7b::SENTINEL_AT, rig7b::SENTINEL_SIZE, false),
        (rig7b::OTHER_AT, rig7b::OTHER_SIZE, false),
    ];
    let mut handles = Vec::new();
    for (at, size, focused) in want {
        let found = windows
            .iter()
            .find(|v| v["rect"]["x"] == at.0 && v["rect"]["y"] == at.1)
            .unwrap_or_else(|| panic!("no window at {at:?}: {listing}"));
        let keys: std::collections::BTreeSet<&str> = found
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["expires_in_ms", "focused", "monitors", "rect", "window_id"].into(),
            "{found}"
        );
        assert_eq!(
            found["rect"],
            json!({"x": at.0, "y": at.1, "width": size.0, "height": size.1})
        );
        assert_eq!(found["monitors"], json!(["DP-1"]));
        assert_eq!(found["focused"], json!(focused), "{found}");
        let left = found["expires_in_ms"].as_u64().unwrap();
        assert!((1..=60_000).contains(&left), "{found}");
        let handle = found["window_id"].as_str().unwrap().to_owned();
        assert!(rig7b::is_handle(&handle), "{handle}");
        handles.push(handle);
    }
    handles.sort();
    handles.dedup();
    assert_eq!(handles.len(), 3, "the handles are not distinct");
    // The listing makes no change on B and shows no native address.
    nothing_ran(&w);
    assert!(!out.raw.contains("0x7f00a00"), "a native address");
    assert!(!out.raw.contains(&preview), "the preview handle of P6");

    // The sentinel title and class are nowhere.
    let reads = w.b.rig.logs();
    for needle in [SENTINEL_TITLE, SENTINEL_CLASS] {
        assert!(!out.raw.contains(needle), "the answer holds `{needle}`");
        assert!(!w.audit_text().await.contains(needle), "the audit");
        assert!(!mcp::logged().contains(needle), "a log event");
        assert!(!reads.contains(needle), "a log of the fake desktop");
        assert_eq!(
            w.files_holding_outside_rig(needle),
            Vec::<String>::new(),
            "a file of the test holds `{needle}`"
        );
    }
}

// -------------------------------------------------------------- proof 4

/// §16 Behavior and §8: `key_press` in a listed window. The focus targets the
/// address of the listed window (and no other), the disclosure names an
/// application window, `wtype` runs once, and the result is done.
#[tokio::test]
async fn key_press_in_a_listed_window_focuses_the_address_of_that_window() {
    let (w, _preview) = world_with(false, true).await;
    rig7b::add_ordinary_windows(&w.b.rig);
    let listing = w.window_list().await.ok();
    assert_eq!(listing["station"], w.to_b(), "{listing}");
    let handle = World::handle_at(&listing, rig7b::OTHER_AT);
    rig7b::focus_goes_to(
        &w.b.rig,
        &rig7b::active_of(rig7b::OTHER_ADDRESS, rig7b::OTHER_CLASS, rig7b::OTHER_TITLE),
    );
    let before = w.grant().await;
    let out = w
        .mcp
        .call("key_press", w.key_args("k1", &handle, "Return"))
        .await;
    done(&w, "k1", &out).await;
    assert!(!out.raw.contains(&handle), "the answer echoes the handle");

    assert_eq!(
        order(&w),
        ["notify-send", "hyprctl dispatch focuswindow", "wtype"],
        "{:?}",
        w.b.rig.calls()
    );
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(
        notes[0].args[3..],
        [
            "Fleet Deck",
            "Fleet Deck is sending keyboard input for swift-fox to an application window."
        ]
    );
    assert_eq!(
        input_rig::focus_calls(&w.b.rig),
        [[
            "dispatch".to_owned(),
            "focuswindow".to_owned(),
            format!("address:{}", rig7b::OTHER_ADDRESS)
        ]]
    );
    assert_eq!(w.b.rig.calls_of("wtype")[0].args, ["-k", "Return"]);
    assert_eq!(w.grant().await.input_used, before.input_used + 1);
}

// -------------------------------------------------------------- proof 5

/// §16 Exposure and §7: a handle that `window_list` issued to one coordinator
/// grant is `desktop_window_not_found` for another coordinator grant on the
/// same station and the same target, and nothing runs.
#[tokio::test]
async fn a_listed_handle_of_another_coordinator_grant_is_not_found_and_runs_nothing() {
    let (w, _preview) = world_with(false, true).await;
    rig7b::add_ordinary_windows(&w.b.rig);
    let listing = w.window_list().await.ok();
    assert_eq!(listing["station"], w.to_b(), "{listing}");
    let handle = World::handle_at(&listing, rig7b::OTHER_AT);
    let second = w.second_caller().await;
    let out = second
        .call("key_press", w.key_args("k1", &handle, "Return"))
        .await;
    refused(&out, "desktop_window_not_found");
    assert_eq!(
        out.json()["message"],
        "No input window available to this grant matches that handle."
    );
    nothing_ran(&w);
    assert_eq!(w.grant().await.input_used, 0);
    // The handle still works for the grant that listed it.
    rig7b::focus_goes_to(
        &w.b.rig,
        &rig7b::active_of(rig7b::OTHER_ADDRESS, rig7b::OTHER_CLASS, rig7b::OTHER_TITLE),
    );
    let own = w
        .mcp
        .call("key_press", w.key_args("k2", &handle, "Return"))
        .await;
    done(&w, "k2", &own).await;
}

// -------------------------------------------------------------- proof 6

/// §16 Failure and §3.2, §9.2: `allow_desktop_input_any` off on B is
/// `input_any_disabled` for `window_list` and for a key in the own preview;
/// `allow_desktop_click` off is `input_click_disabled` for a click. Nothing
/// runs on B. With the gate on again the same read works.
#[tokio::test]
async fn a_target_with_the_p7b_gates_off_refuses_before_any_effect() {
    let (w, preview) = world_with(true, true).await;
    rig7b::add_ordinary_windows(&w.b.rig);

    w.set_b_gates(true, false);
    let out = w.window_list().await;
    refused(&out, "input_any_disabled");
    assert_eq!(
        out.json()["message"],
        "Agent input in other application windows is disabled on the target station."
    );
    let out = w
        .mcp
        .call("key_press", w.key_args("k1", &preview, "Return"))
        .await;
    refused(&out, "input_any_disabled");
    nothing_ran(&w);

    w.set_b_gates(false, true);
    let out = w
        .mcp
        .call("pointer_click", w.move_args("c1", &preview, 10, 20))
        .await;
    refused(&out, "input_click_disabled");
    assert_eq!(
        out.json()["message"],
        "Agent pointer clicks are disabled on the target station."
    );
    nothing_ran(&w);
    assert!(rig7b::wlrctl_calls(&w.b.rig).is_empty());
    assert_eq!(w.grant().await.input_used, 0);

    w.set_b_gates(true, true);
    let listing = w.window_list().await.ok();
    assert_eq!(listing["station"], w.to_b(), "{listing}");
    assert_eq!(listing["windows"].as_array().unwrap().len(), 3, "{listing}");
}

// -------------------------------------------------------------- proof 7

/// §12 and §16 Exposure (board sign-out on a click): the owner board session
/// that came from the machine address of B (`127.0.0.1`, as the board client
/// does) is deleted at the click dispatch. As in the P7 proof, its next request
/// answers 401 `login_required` (the session lookup); the audit row
/// `board.signed_out_for_input` exists and names no address; a sign-in from
/// that address during the exposure and during the 60 s tail answers 403
/// `sign_in_blocked_for_input` with the generalized text and no cookie; the
/// operation row of the click has `attempted_at` and `exposure_addrs`.
#[tokio::test]
async fn a_click_signs_the_board_of_the_target_machine_out_and_blocks_a_sign_in() {
    let (w, preview) = world_with(true, false).await;
    let (c, h) = w.deck().board().await;
    assert_eq!(session(&c, &h).await.status, 200);

    let out = w
        .mcp
        .call("pointer_click", w.move_args("c1", &preview, 10, 20))
        .await;
    done(&w, "c1", &out).await;

    let gone = session(&c, &h).await;
    assert_eq!(
        (gone.status, gone.code()),
        (401, "login_required"),
        "{}",
        gone.body
    );
    let audit = w.audit_text().await;
    assert!(audit.contains("board.signed_out_for_input"), "{audit}");
    assert!(!audit.contains("127.0.0.1"), "{audit}");

    let (attempted_at, exposure) = w.click_operation().await;
    assert!(attempted_at.is_some(), "the click has no attempted_at");
    assert!(
        exposure.as_deref().is_some_and(|text| !text.is_empty()),
        "the click has no exposure_addrs"
    );

    // A sign-in from the same address: refused before any cookie, during the
    // exposure and during the 60 s tail.
    let store = w.deck().store.clone();
    let blocked = |r: &common::board::Http| {
        assert_eq!(
            (r.status, r.code()),
            (403, "sign_in_blocked_for_input"),
            "{}",
            r.body
        );
        assert_eq!(
            r.body["message"],
            "An agent is using desktop input on this station. Sign in again in a minute."
        );
        assert!(r.header("set-cookie").is_none());
        assert!(r.body.get("csrf").is_none());
    };
    let token = store.create_login_link().await.unwrap();
    blocked(
        &c.post("/api/auth/link", &[], json!({"token": token.expose()}))
            .await,
    );
    move_exposure_back(&w, 10_000).await;
    blocked(
        &c.post("/api/auth/link", &[], json!({"token": token.expose()}))
            .await,
    );
}

/// `GET /api/auth/session` with the owner login `h`.
async fn session(
    c: &common::board::BoardClient,
    h: &settings_support::Headers,
) -> common::board::Http {
    c.get("/api/auth/session", &settings_support::refs(h)).await
}

/// Moves the times of every input operation of the deck back by `ms` (the
/// store clock is the system clock) and clears the cache of the exposure.
async fn move_exposure_back(w: &World, ms: i64) {
    let store = w.deck().store.clone();
    store
        .db()
        .call(move |c| {
            c.execute(
                "UPDATE coordinator_desktop_ops
                 SET attempted_at = attempted_at - ?1, finished_at = finished_at - ?1
                 WHERE attempted_at IS NOT NULL",
                [ms],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store.invalidate_exposure();
}

// -------------------------------------------------------------- proof 8

/// §16 Version and §4: B leaves `desktop_click_mcp.v1` out of its hello; the
/// owner start with `allow_click` and B as a grant station is 409
/// `unsupported`, and nothing is stored.
#[tokio::test]
async fn a_click_grant_with_a_target_without_the_click_feature_is_unsupported() {
    let w = World::with(Setup {
        b_without: vec![feature::DESKTOP_CLICK_MCP],
        grant: false,
        input: Some(CAPS),
        click: true,
        ..Setup::default()
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let refused = rig7b::start_p7b_grant(
        &c,
        &h,
        &elevation,
        "alpha",
        &[w.b_id.as_str()],
        (true, None),
    )
    .await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, "unsupported"),
        "{}",
        refused.body
    );
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

/// §16 Version and §4: B leaves `desktop_input_any.v1` out of its hello; the
/// owner start with the scope `any_window` is 409 `unsupported`, and nothing is
/// stored.
#[tokio::test]
async fn an_any_window_grant_with_a_target_without_the_feature_is_unsupported() {
    let w = World::with(Setup {
        b_without: vec![feature::DESKTOP_INPUT_ANY],
        grant: false,
        input: Some(CAPS),
        any_window: true,
        ..Setup::default()
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let refused = rig7b::start_p7b_grant(
        &c,
        &h,
        &elevation,
        "alpha",
        &[w.b_id.as_str()],
        (false, Some("any_window")),
    )
    .await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, "unsupported"),
        "{}",
        refused.body
    );
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

// -------------------------------------------------------------- proof 9

/// §16 Version and §4: a default P7 world (no click, own scope) lists the
/// three P7 input tools and neither `pointer_click` nor `window_list`; a call
/// of either is an error and nothing runs on B. The P7 proofs stay green in
/// `p7_end_to_end`.
#[tokio::test]
async fn a_default_p7_world_lists_the_three_p7_input_tools_only() {
    let (w, window) = world_with(false, false).await;
    let names = w.mcp.names().await;
    for tool in ["key_press", "type_text", "pointer_move"] {
        assert!(names.iter().any(|n| n == tool), "{tool} is not listed");
    }
    for tool in ["pointer_click", "window_list"] {
        assert!(!names.iter().any(|n| n == tool), "{tool} is listed");
    }
    let calls: [(&str, Value); 2] = [
        ("pointer_click", w.move_args("c1", &window, 1, 1)),
        ("window_list", json!({"station": w.to_b()})),
    ];
    for (tool, args) in calls {
        let out = w.mcp.call(tool, args).await;
        assert!(out.is_error, "{tool}: {}", out.raw);
    }
    nothing_ran(&w);
}
