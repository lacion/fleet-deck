//! The P7 contract end to end over real links (R13 P7 task P): a caller
//! station A (real station crate, real coordinator MCP endpoint), the deck
//! (real deck crate) and a target station B (real station crate with a fake
//! desktop: a fake `hyprctl` and a fake `wtype`). The crate tests of the
//! station and the deck are not repeated here; each test names the items of
//! `docs/v2/design/omarchy-p7.md` §16 that it shows.
//!
//! In this world B and the board client both come from `127.0.0.1`, so an
//! input operation exposes the address of the board client: the deck signs
//! the owner board session out (§7.6). A test that needs the board after an
//! input therefore signs in before it, and a test that must act as the owner
//! during an input uses the store of the deck (see the revocation test).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod p6_support;
mod settings_support;

use fleetdeck_proto::version::feature;
use p6_support::input_rig::{self, start_input_grant};
use p6_support::mcp::Tool;
use p6_support::*;
use serde_json::json;

const SENTINEL_TEXT: &str = "SENTINEL-TYPED-7f3a-zq";

/// A world with the input allowance `caps` (the input cap and the character
/// cap) and one preview of the caller on B. Returns the world and the
/// `window_id` of the preview.
async fn world_with(caps: (u32, u32)) -> (World, String) {
    let w = World::with(Setup {
        input: Some(caps),
        ..Setup::default()
    })
    .await;
    let window = w.open_preview("p1").await;
    (w, window)
}

/// The usual world: caps that no test of the proofs reaches by accident.
async fn world() -> (World, String) {
    world_with((20, 1000)).await
}

/// The names of the changing programs on B since the preview opened, with
/// the first argument of each `hyprctl`.
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

/// A call that finished: the answer of a done input is `{}` (§8.3: no echo)
/// and the status tool says `done` with an empty result.
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

/// A call that must be an error with the fixed `code`; the text of the
/// failure never repeats what the caller sent.
fn refused(out: &Tool, code: &str) {
    assert_eq!(out.code(), code, "{}", out.raw);
}

// -------------------------------------------------------------- proof 1

/// §16 items 6, 13, 14, 16, 53: `key_press` `Return` in the preview of the
/// caller. The disclosure comes before the focus and before `wtype`; the
/// `wtype` argv is the fixed one of §5.1; the result is done and carries
/// no echo; the grant shows one input, no characters and one more desktop
/// action.
#[tokio::test]
async fn key_press_return_discloses_then_focuses_then_types_one_chord() {
    let (w, window) = world().await;
    let before = w.grant().await;
    let out = w
        .mcp
        .call("key_press", w.key_args("k1", &window, "Return"))
        .await;
    done(&w, "k1", &out).await;
    for text in [window.as_str(), "Return", "0x"] {
        assert!(!out.raw.contains(text), "the answer echoes `{text}`");
    }

    // The order of the three visible steps.
    assert_eq!(
        order(&w),
        ["notify-send", "hyprctl dispatch focuswindow", "wtype"],
        "{:?}",
        w.b.rig.calls()
    );
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(
        notes[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Fleet Deck",
            "Fleet Deck is sending keyboard input for swift-fox to an agent preview."
        ]
    );
    let focus = input_rig::focus_calls(&w.b.rig);
    assert_eq!(focus.len(), 1);
    assert!(focus[0][2].starts_with("address:0x"), "{focus:?}");
    let keys = w.b.rig.calls_of("wtype");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].args, ["-k", "Return"]);
    // A chord has no stdin.
    assert_eq!(input_rig::wtype_stdin(&w.b.rig), None);

    let after = w.grant().await;
    assert_eq!(after.input_used, before.input_used + 1);
    assert_eq!(after.input_character_used, 0);
    assert_eq!(after.desktop_used, before.desktop_used + 1);
}

// -------------------------------------------------------------- proof 2

/// §16 items 8, 9, 35, 36, 37: `type_text` `hello`. The `wtype` argv is
/// exactly `-` and its stdin file holds `hello`; the text is in no argv,
/// environment record, log file, database or audit row of the test; the grant
/// shows five characters.
#[tokio::test]
async fn type_text_travels_only_on_stdin_and_is_nowhere_else() {
    let (w, window) = world().await;
    let out = w
        .mcp
        .call("type_text", w.text_args("t1", &window, "hello"))
        .await;
    done(&w, "t1", &out).await;
    let typed = w.b.rig.calls_of("wtype");
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].args, ["-"]);
    assert_eq!(input_rig::wtype_stdin(&w.b.rig).as_deref(), Some("hello"));
    let grant = w.grant().await;
    assert_eq!(grant.input_character_used, 5);
    assert_eq!(grant.input_used, 1);

    // A sentinel text: it reaches the fake `wtype` through stdin and no other
    // place of the whole test. (`hello` is too short for a scan of a binary
    // database.)
    let out = w
        .mcp
        .call("type_text", w.text_args("t2", &window, SENTINEL_TEXT))
        .await;
    done(&w, "t2", &out).await;
    assert_eq!(
        input_rig::wtype_stdin(&w.b.rig).as_deref(),
        Some(SENTINEL_TEXT)
    );
    assert_eq!(w.b.rig.calls_of("wtype")[1].args, ["-"]);
    for needle in ["hello", SENTINEL_TEXT] {
        assert!(!w.b.rig.logs().contains(needle), "a rig log holds the text");
        assert!(!w.audit_text().await.contains(needle), "the audit");
        assert!(!mcp::logged().contains(needle), "a log event");
        let env = std::fs::read_to_string(w.b.rig.dir.path().join("wtype.env")).unwrap();
        assert!(!env.contains(needle), "the environment of wtype");
    }
    assert_eq!(
        w.files_holding(SENTINEL_TEXT),
        Vec::<String>::new(),
        "a file of the test holds the typed text"
    );
    assert_eq!(w.grant().await.input_character_used, 5 + 22);
}

// -------------------------------------------------------------- proof 3

/// §16 items 10 and 11: `pointer_move` to a point inside the preview
/// dispatches `movecursor` at the preview origin plus the point; a point
/// outside the preview is `input_destination_unavailable` and nothing moves.
#[tokio::test]
async fn pointer_move_adds_the_preview_origin_and_refuses_a_point_outside() {
    let (w, window) = world().await;
    let (ox, oy) = input_rig::PREVIEW_AT;
    let (width, height) = input_rig::PREVIEW_SIZE;
    let out = w
        .mcp
        .call("pointer_move", w.move_args("m1", &window, 10, 20))
        .await;
    done(&w, "m1", &out).await;
    assert_eq!(
        input_rig::move_calls(&w.b.rig),
        [[
            "dispatch".to_owned(),
            "movecursor".to_owned(),
            (ox + 10).to_string(),
            (oy + 20).to_string()
        ]]
    );
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(
        notes[0].args[3..],
        [
            "Fleet Deck",
            "Fleet Deck is moving the pointer for swift-fox in an agent preview."
        ]
    );
    assert_eq!(
        order(&w),
        [
            "notify-send",
            "hyprctl dispatch focuswindow",
            "hyprctl dispatch movecursor"
        ]
    );

    // Outside: the right edge, the bottom edge and far away. Nothing runs
    // after the first move.
    let calls = w.b.rig.calls().len();
    for (id, x, y) in [
        ("m2", u32::try_from(width).unwrap(), 0),
        ("m3", 0, u32::try_from(height).unwrap()),
        ("m4", 32767, 32767),
    ] {
        let out = w
            .mcp
            .call("pointer_move", w.move_args(id, &window, x, y))
            .await;
        refused(&out, "input_destination_unavailable");
        assert_eq!(
            out.json()["message"],
            "The pointer destination is not available inside this preview."
        );
    }
    assert_eq!(input_rig::move_calls(&w.b.rig).len(), 1);
    assert_eq!(w.b.rig.calls().len(), calls, "{:?}", w.b.rig.calls());
}

// -------------------------------------------------------------- proof 4

/// §7.6 and §16.3 (board sign-out): the owner board session that came from
/// the machine address of B (here `127.0.0.1`, as the board client does) is
/// deleted at the input dispatch; its next request is refused for good; a
/// sign-in from that address is refused with no cookie during the exposure
/// and the 60 s tail, and works after the tail. The exposure rows move back in
/// time instead of a wait. The next request of the deleted session answers
/// 401 `login_required` (the session lookup), not `signed_out_for_input`: that
/// answer is rule 2 for a session that still exists (crate tests).
#[tokio::test]
async fn an_input_signs_the_board_of_the_target_machine_out_and_blocks_a_sign_in() {
    let (w, window) = world().await;
    let (c, h) = w.deck().board().await;
    assert_eq!(session(&c, &h).await.status, 200);

    let out = w
        .mcp
        .call("key_press", w.key_args("k1", &window, "Return"))
        .await;
    done(&w, "k1", &out).await;

    // The session is gone (rule 1 deleted it at the dispatch): the existing
    // session lookup refuses the next request with 401 `login_required`.
    // `signed_out_for_input` is rule 2, for a session that still exists; the
    // board learns the reason from the 4401 close of its live socket.
    let gone = session(&c, &h).await;
    assert_eq!(
        (gone.status, gone.code()),
        (401, "login_required"),
        "{}",
        gone.body
    );
    // The audit row names the session kind and the operation, no address.
    let audit = w.audit_text().await;
    assert!(audit.contains("board.signed_out_for_input"), "{audit}");
    assert!(!audit.contains("127.0.0.1"), "{audit}");

    // A sign-in from the same address: refused before any cookie.
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
            "An agent is typing on this station. Sign in again in a minute."
        );
        assert!(r.header("set-cookie").is_none());
        assert!(r.body.get("csrf").is_none());
    };
    let token = store.create_login_link().await.unwrap();
    blocked(
        &c.post("/api/auth/link", &[], json!({"token": token.expose()}))
            .await,
    );
    // The tail: the operation finished 10 s ago; the exposure ended 5 s ago.
    move_exposure_back(&w, 10_000).await;
    blocked(
        &c.post("/api/auth/link", &[], json!({"token": token.expose()}))
            .await,
    );
    // After the tail (65 s after the end) the same link signs in.
    move_exposure_back(&w, 70_000).await;
    let ok = c
        .post("/api/auth/link", &[], json!({"token": token.expose()}))
        .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert!(ok.header("set-cookie").is_some());
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

// -------------------------------------------------------------- proof 5

/// §16 items 39 and 40: a preview whose title the fake reports as a Fleet
/// Deck board is `input_board_forbidden` for all three tools; no input
/// notification, no focus and no `wtype` follow.
#[tokio::test]
async fn a_preview_that_looks_like_the_board_gets_no_input() {
    let (w, window) = world().await;
    input_rig::set_title(&w.b.rig, "Fleet Deck - Sessions");
    let calls = [
        ("key_press", w.key_args("k1", &window, "Return")),
        ("type_text", w.text_args("t1", &window, "hello")),
        ("pointer_move", w.move_args("m1", &window, 5, 5)),
    ];
    for (tool, args) in calls {
        let out = w.mcp.call(tool, args).await;
        refused(&out, "input_board_forbidden");
        assert_eq!(
            out.json()["message"],
            "Fleet Deck board and wall windows cannot receive agent input.",
            "{tool}"
        );
    }
    assert!(w.b.rig.calls().is_empty(), "{:?}", w.b.rig.calls());
    assert_eq!(input_rig::wtype_stdin(&w.b.rig), None);
}

// -------------------------------------------------------------- proof 6

/// §16 items 18 and 34: the owner revokes the grant while `wtype` runs (the
/// rig holds it). The result is refused with `authority_lost` or
/// `input_interrupted`, and no second input is sent: a new call after the
/// revocation runs nothing.
///
/// The owner does not use the board here: the input exposes the address of
/// the board client, so its session is signed out (the owner lock-out of
/// §7.6). The test makes the two store steps of the revoke route.
#[tokio::test]
async fn a_revoke_while_wtype_runs_gives_no_success_and_no_second_input() {
    let (w, window) = world().await;
    input_rig::hold_wtype(&w.b.rig, true);
    let (client, args) = (w.mcp.clone(), w.key_args("k1", &window, "Return"));
    let call = tokio::spawn(async move { client.call("key_press", args).await });
    input_rig::wtype_started(&w.b.rig).await;
    assert_eq!(w.b.rig.calls_of("wtype").len(), 1);

    let store = w.deck().store.clone();
    assert!(
        store
            .revoke_grant(w.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    w.deck().node.link().peers.refresh_changed().await.unwrap();
    input_rig::hold_wtype(&w.b.rig, false);

    let out = call.await.unwrap();
    assert!(out.is_error, "{}", out.raw);
    let code = out.code();
    assert!(
        ["authority_lost", "input_interrupted"].contains(&code.as_str()),
        "{code}: {}",
        out.raw
    );
    // A second input, with another request ID, is refused and sends nothing.
    let again = w
        .mcp
        .call("key_press", w.key_args("k2", &window, "Return"))
        .await;
    assert!(again.is_error, "{}", again.raw);
    assert_eq!(w.b.rig.calls_of("wtype").len(), 1, "{:?}", w.b.rig.calls());
    assert_eq!(input_rig::focus_calls(&w.b.rig).len(), 1);
}

// -------------------------------------------------------------- proof 7

/// §3.3 item 10 and §16 item 19: `allow_desktop_input` off on B is refused
/// before any notification, focus or `wtype`.
#[tokio::test]
async fn a_target_with_desktop_input_off_is_refused_before_any_effect() {
    let (w, window) = world().await;
    w.set_b_input(true, true, true, false);
    let out = w
        .mcp
        .call("key_press", w.key_args("k1", &window, "Return"))
        .await;
    assert!(out.is_error, "{}", out.raw);
    assert_eq!(out.code(), "not_granted", "{}", out.raw);
    assert!(w.b.rig.calls().is_empty(), "{:?}", w.b.rig.calls());
    // Turned on again, the same preview takes input.
    w.set_b_input(true, true, true, true);
    let out = w
        .mcp
        .call("key_press", w.key_args("k2", &window, "Return"))
        .await;
    done(&w, "k2", &out).await;
}

/// §3.3 item 10 and §16 item 19: B turns `allow_desktop_input` off after the
/// deck decided and while the target waits at the gate of its first step; the
/// target reads its own setting again and refuses before the notification,
/// the focus and `wtype`.
#[tokio::test]
async fn a_target_that_turns_desktop_input_off_after_the_deck_decided_runs_nothing() {
    let (w, window) = world().await;
    w.b.rig.close_gate();
    let (client, args) = (w.mcp.clone(), w.key_args("k1", &window, "Return"));
    let call = tokio::spawn(async move { client.call("key_press", args).await });
    w.b.rig.entered().await;
    w.set_b_input(true, true, true, false);
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert!(out.is_error, "{}", out.raw);
    assert!(out.code().contains("forbidden"), "{}", out.raw);
    assert!(w.b.rig.calls().is_empty(), "{:?}", w.b.rig.calls());
}

// -------------------------------------------------------------- proof 8

/// §9.1 and §16 item 14: with an input cap of 1, the second input is
/// `input_cap_reached` and nothing runs.
#[tokio::test]
async fn a_second_input_over_a_cap_of_one_is_refused_and_runs_nothing() {
    let (w, window) = world_with((1, 100)).await;
    let first = w
        .mcp
        .call("key_press", w.key_args("k1", &window, "Return"))
        .await;
    done(&w, "k1", &first).await;
    let calls = w.b.rig.calls().len();
    let second = w
        .mcp
        .call("key_press", w.key_args("k2", &window, "Return"))
        .await;
    refused(&second, "input_cap_reached");
    assert_eq!(
        second.json()["message"],
        "This grant has used its input action limit."
    );
    assert_eq!(w.b.rig.calls().len(), calls, "{:?}", w.b.rig.calls());
    assert_eq!(w.grant().await.input_used, 1);
}

/// §9.1 and §16 item 14: with a character cap of 3, a text of 4 characters is
/// `input_characters_reached` and nothing runs.
#[tokio::test]
async fn a_text_over_the_character_cap_is_refused_and_runs_nothing() {
    let (w, window) = world_with((5, 3)).await;
    let out = w
        .mcp
        .call("type_text", w.text_args("t1", &window, "abcd"))
        .await;
    refused(&out, "input_characters_reached");
    assert_eq!(
        out.json()["message"],
        "This grant has used its typed-character limit."
    );
    assert!(w.b.rig.calls().is_empty(), "{:?}", w.b.rig.calls());
    assert_eq!(input_rig::wtype_stdin(&w.b.rig), None);
    assert_eq!(w.grant().await.input_character_used, 0);
}

// -------------------------------------------------------------- proof 9

/// §16 items 42 and 43, §14 item 1: a default P6 world (no input allowance)
/// lists no input tool, a call of one is an error, and nothing runs on B.
#[tokio::test]
async fn a_world_without_the_input_allowance_has_no_input_tool_and_runs_nothing() {
    let w = World::start().await;
    let url = json!({"request_id": "p1", "station": w.to_b(), "url": "http://localhost:3000/"});
    let window = w.mcp.call("preview_open", url).await.ok()["window_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let names = w.mcp.names().await;
    assert!(names.iter().any(|n| n == "preview_open"), "{names:?}");
    for tool in ["key_press", "type_text", "pointer_move"] {
        assert!(!names.iter().any(|n| n == tool), "{tool} is listed");
    }
    let before = w.b.rig.calls().len();
    let calls = [
        ("key_press", w.key_args("k1", &window, "Return")),
        ("type_text", w.text_args("t1", &window, "hello")),
        ("pointer_move", w.move_args("m1", &window, 1, 1)),
    ];
    for (tool, args) in calls {
        let out = w.mcp.call(tool, args).await;
        assert!(out.is_error, "{tool}: {}", out.raw);
    }
    assert_eq!(w.b.rig.calls().len(), before, "{:?}", w.b.rig.calls());
    assert!(w.b.rig.calls_of("wtype").is_empty());
}

// ------------------------------------------------------------- proof 10

/// §14 items 4 and 5: B leaves `desktop_input_mcp.v1` out of its hello; the
/// owner start of an input grant with B as a grant station is 409
/// `unsupported`, and nothing is stored.
#[tokio::test]
async fn an_input_grant_with_a_target_without_the_feature_is_unsupported() {
    let w = World::with(Setup {
        b_without: vec![feature::DESKTOP_INPUT_MCP],
        grant: false,
        ..Setup::default()
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let refused = start_input_grant(&c, &h, &elevation, "alpha", &[w.b_id.as_str()]).await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, "unsupported"),
        "{}",
        refused.body
    );
    assert!(c.store.grants(10).await.unwrap().is_empty());
}
