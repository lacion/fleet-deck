//! The P6 contract end to end over real links (R13 P6 task T7a): a caller
//! station A (real station crate, real coordinator MCP endpoint), the deck
//! (real deck crate) and a target station B (real station crate with a fake
//! desktop). The unit and crate tests of T1a to T6 are not repeated here; each
//! test names the proofs of `docs/v2/design/omarchy-p6.md` §15 that it shows.
//!
//! The target waits at a gate: the fake `hyprctl instances -j` of the first
//! step of an action holds until the test opens it. Every check of the
//! authority comes after that step, so a test changes the world while the
//! target waits and sees which check refuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod p6_support;
mod settings_support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_deck::store::LinkFeature;
use fleetdeck_proto::version::feature;
use p6_support::board::{raw_get, revoke, start_grant};
use p6_support::fake_deck::FakeDeck;
use p6_support::fake_desktop::Rig;
use p6_support::mcp::png_of;
use p6_support::*;
use serde_json::{Value, json};

fn is_handle(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn notify_args(w: &World, id: &str) -> Value {
    json!({"request_id": id, "station": w.to_b(), "title": "Build done", "body": "all green"})
}

/// The `notify` call of A that waits at the gate of B, and the gate.
async fn held_notify(w: &World, id: &str) -> tokio::task::JoinHandle<mcp::Tool> {
    w.b.rig.close_gate();
    let (client, args) = (w.mcp.clone(), notify_args(w, id));
    let call = tokio::spawn(async move { client.call("notify", args).await });
    w.b.rig.entered().await;
    call
}

// ------------------------------------------------------------ proofs 3 and 5

/// Proofs 3 and 5: `notify` and `preview_open` of A run on B; the title has
/// the callsign prefix; the opaque handle closes its own window only.
#[tokio::test]
async fn a_remote_notification_and_preview_run_on_the_target_and_a_handle_closes_only_its_window() {
    let w = World::start().await;
    let b = w.to_b();
    let sent = w.mcp.call("notify", notify_args(&w, "n1")).await;
    assert_eq!(sent.ok(), json!({}));
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(notes.len(), 1, "{:?}", w.b.rig.calls());
    // The fixed argv: `--` ends the options; the title has the callsign.
    assert_eq!(
        notes[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "swift-fox: Build done",
            "all green"
        ]
    );
    let url = json!({"request_id": "p1", "station": b, "url": "http://localhost:3000/"});
    let window = w.mcp.call("preview_open", url).await.ok()["window_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(is_handle(&window), "{window}");
    assert_eq!(w.b.rig.calls_of("chromium").len(), 1);
    // A handle that no preview of this caller has gives the answer of an
    // unknown one, and no window closes.
    let closes = |w: &World| {
        w.b.rig
            .calls_of("hyprctl")
            .into_iter()
            .filter(|c| c.args.iter().any(|a| a == "closewindow"))
            .count()
    };
    let foreign = json!({"request_id": "c0", "station": b, "window_id": "0".repeat(32)});
    let refused = w.mcp.call("window_close", foreign).await;
    assert_eq!(refused.code(), "desktop_window_not_found");
    assert_eq!(closes(&w), 0);
    let own = json!({"request_id": "c1", "station": b, "window_id": window});
    assert_eq!(w.mcp.call("window_close", own).await.ok(), json!({}));
    assert_eq!(closes(&w), 1);
}

/// R13 P6 follow-up (§6.4): the owner sees the preview that an agent opened
/// in the answer of an owner screenshot (the capture path stamps the
/// previews too), closes it on the board, and an unknown handle is the owner
/// not-found answer, not a failure of the deck.
#[tokio::test]
async fn the_owner_sees_the_preview_of_an_agent_and_closes_it_on_the_board() {
    let w = World::start().await;
    let url = json!({"request_id": "p1", "station": w.to_b(), "url": "http://localhost:3000/"});
    let window = w.mcp.call("preview_open", url).await.ok()["window_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (c, h) = w.deck().board().await;
    p6_support::board::enable_desktop(&c);
    w.b.grim(&png_of(4096, b"owner pixels "));
    let shot = p6_support::board::owner_capture(&c, &h, "beta", "DP-1").await;
    assert_eq!(shot.status, 200, "{}", shot.body);
    assert_eq!(
        shot.body["info"]["previews"],
        json!([{"window_id": window, "monitor": null, "caller_label": "swift-fox"}]),
        "{}",
        shot.body
    );
    assert!(!shot.body.to_string().contains("0x"), "no address leaves");

    let origin = format!("https://deck.tail1234.ts.net:{}", c.addr.port());
    let mut headers = settings_support::refs(&h);
    headers.push(("Origin", origin.as_str()));
    let act = "/api/stations/beta/desktop/act";
    let unknown = c
        .post(
            act,
            &headers,
            json!({"action": "preview_close", "window_id": "0".repeat(32)}),
        )
        .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert_eq!(unknown.code(), "desktop_not_found");
    let closed = c
        .post(
            act,
            &headers,
            json!({"action": "preview_close", "window_id": window}),
        )
        .await;
    assert_eq!(closed.status, 200, "{}", closed.body);
    let sent = w.b.rig.calls_of("hyprctl");
    let closes = sent
        .iter()
        .filter(|c| c.args.iter().any(|a| a == "closewindow"))
        .count();
    assert_eq!(closes, 1);
    // The record goes with the window: the next answer lists nothing.
    let again = c
        .post(
            act,
            &headers,
            json!({"action": "preview_close", "window_id": window}),
        )
        .await;
    assert_eq!(again.status, 404, "{}", again.body);
}

/// The owner recording answer lists the previews too (the recording path
/// stamps them like every owner answer).
#[tokio::test]
async fn the_owner_recording_answer_lists_the_previews_of_the_agents() {
    let w = World::start().await;
    let url = json!({"request_id": "p1", "station": w.to_b(), "url": "http://localhost:3000/"});
    let window = w.mcp.call("preview_open", url).await.ok()["window_id"]
        .as_str()
        .unwrap()
        .to_owned();
    p6_support::recorder::add_recorder(w.b.rig.dir.path());
    let (c, h) = w.deck().board().await;
    p6_support::board::enable_desktop(&c);
    let origin = format!("https://deck.tail1234.ts.net:{}", c.addr.port());
    let mut headers = settings_support::refs(&h);
    headers.push(("Origin", origin.as_str()));
    headers.push((
        fleetdeck_proto::capture::CAPTURE_ID_HEADER,
        p6_support::board::OWNER_CAPTURE,
    ));
    headers.push(("X-FleetDeck-Features", "artifacts_video.v1"));
    let started = c
        .post(
            "/api/stations/beta/desktop/act",
            &headers,
            json!({"action": "screen_record", "monitor": "DP-1", "seconds": 1}),
        )
        .await;
    assert_eq!(started.status, 200, "{}", started.body);
    assert_eq!(
        started.body["info"]["previews"],
        json!([{"window_id": window, "monitor": null, "caller_label": "swift-fox"}]),
        "{}",
        started.body
    );
}

// ------------------------------------------------------------ proofs 8 and 9

/// The committed screenshot of `monitor` of B.
async fn shot(w: &World, id: &str) -> Value {
    let args = json!({"request_id": id, "station": w.to_b(), "monitor": "DP-1"});
    w.mcp.call("screenshot", args).await.ok()
}

/// Proofs 8 and 9: a screenshot of B through A, the disclosure before `grim`,
/// an image of exactly the cap through both hops, and one byte more refused
/// while the owner still reads the artifact.
#[tokio::test]
async fn a_screenshot_of_the_cap_passes_both_hops_and_one_byte_more_is_refused() {
    let w = World::start().await;
    let exact = png_of(716_800, b"exact cap pixels ");
    w.b.grim(&exact);
    let first = shot(&w, "s1").await;
    assert_eq!(first["state"], "committed", "{first}");
    assert_eq!(first["monitor"], "DP-1");
    assert_eq!(first["size"], 716_800);
    let artifact = first["artifact"].clone();
    assert_eq!(artifact["station_id"], w.b_id.as_str());
    assert_eq!(artifact["version"], 1);
    // The fixed disclosure came before `grim`, with the callsign.
    let order = w.b.rig.order();
    let grim = order.iter().position(|p| p == "grim").expect("grim ran");
    let note = order.iter().position(|p| p == "notify-send").unwrap();
    assert!(note < grim, "{order:?}");
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(
        notes[0].args[3..],
        [
            "Fleet Deck",
            "Fleet Deck is taking a screenshot of DP-1 for swift-fox."
        ]
    );
    let image = w
        .mcp
        .call("read_capture_image", json!({"artifact": artifact}))
        .await;
    assert!(
        !image.is_error,
        "{}",
        image.raw.chars().take(400).collect::<String>()
    );
    assert_eq!(image.content.len(), 2);
    assert_eq!(image.content[0]["type"], "text");
    let meta = image.json();
    assert_eq!(meta["size"], 716_800);
    assert!(meta["notice"].as_str().unwrap().contains("untrusted"));
    assert_eq!(image.content[1]["type"], "image");
    assert_eq!(image.content[1]["mimeType"], "image/png");
    let bytes = STANDARD
        .decode(image.content[1]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes, exact);
    assert!(image.bytes <= 1_048_576, "{}", image.bytes);

    // One byte more: the capture commits; the image read is refused; the
    // owner reads the artifact on the board.
    w.b.grim(&png_of(716_801, b"cap and one "));
    let second = shot(&w, "s2").await;
    assert_eq!(second["state"], "committed", "{second}");
    let id = second["artifact"]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let big = w
        .mcp
        .call(
            "read_capture_image",
            json!({"artifact": second["artifact"]}),
        )
        .await;
    assert_eq!(big.code(), "image_too_large");
    assert_eq!(big.content.len(), 1);
    let (c, h) = w.deck().board().await;
    let path = format!("/api/artifacts/beta/{id}/1/raw/capture.png");
    let (status, body) = raw_get(&c, &path, &settings_support::refs(&h)).await;
    assert_eq!((status, body.len()), (200, 716_801));
}

// --------------------------------------------------------- proofs 11, 12, 13

/// Proof 11: the owner revokes the grant on the board between the deck
/// admission and the effect: nothing happens on B.
#[tokio::test]
async fn a_grant_revoked_before_the_effect_stops_it_with_authority_lost() {
    let w = World::start().await;
    let (c, h) = w.deck().board().await;
    let call = held_notify(&w, "n1").await;
    revoke(&c, &h, &w.grant_id).await;
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert_eq!(out.code(), "authority_lost");
    assert!(w.b.rig.calls_of("notify-send").is_empty());
}

/// Proofs 11 and 12: A turns `allow_fleet_control` off while the target
/// waits. The deck projections do not change; the live round trip to A at the
/// final check refuses.
#[tokio::test]
async fn a_caller_that_turns_fleet_control_off_is_refused_by_the_live_round_trip() {
    let w = World::start().await;
    let call = held_notify(&w, "n1").await;
    w.set_a_fleet(false);
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert_eq!(out.code(), "authority_lost");
    assert!(w.b.rig.calls_of("notify-send").is_empty());
}

/// Proof 11: the deck switch `desktop_control` turns off while the target
/// waits.
#[tokio::test]
async fn a_deck_switch_that_turns_off_before_the_effect_stops_it() {
    let w = World::start().await;
    let call = held_notify(&w, "n1").await;
    w.deck().desktop_control(false);
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert_eq!(out.code(), "authority_lost");
    assert!(w.b.rig.calls_of("notify-send").is_empty());
}

/// Proof 13: B turns `allow_desktop` off after the deck decided and before
/// the effect; B's own last read refuses. The same for the capture setting of
/// a screenshot.
#[tokio::test]
async fn a_target_setting_that_turns_off_before_the_effect_refuses_the_work_on_the_target() {
    let w = World::start().await;
    let call = held_notify(&w, "n1").await;
    w.set_b(true, false, true);
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert!(out.is_error, "{}", out.raw);
    assert!(out.code().contains("forbidden"), "{}", out.raw);
    assert!(w.b.rig.calls_of("notify-send").is_empty());

    w.set_b(true, true, true);
    w.b.grim(&png_of(2_000, b"pixels"));
    w.b.rig.close_gate();
    let (client, args) = (
        w.mcp.clone(),
        json!({"request_id": "s1", "station": w.to_b(), "monitor": "DP-1"}),
    );
    let call = tokio::spawn(async move { client.call("screenshot", args).await });
    w.b.rig.entered().await;
    w.set_b(true, true, false);
    w.b.rig.open_gate();
    let out = call.await.unwrap();
    assert!(out.is_error, "{}", out.raw);
    assert!(w.b.rig.calls_of("grim").is_empty(), "{:?}", w.b.rig.calls());
}

// ----------------------------------------------------------------- proof 17

/// Proof 17: the deck restarts after `attempted`, while B holds the action.
/// No second notification reaches B; the status never says that a second run
/// happened.
#[tokio::test]
async fn a_deck_restart_after_attempted_never_runs_the_action_a_second_time() {
    let mut w = World::start().await;
    let first = held_notify(&w, "n1").await;
    w.restart_deck().await;
    w.b.rig.open_gate();
    let ended = first.await.unwrap();
    assert!(ended.is_error, "{}", ended.raw);
    // The same request again: the recorded operation, no new send. The
    // links of the new deck may need a moment: an answer before the
    // admission (a link that is not up) carries no record and starts nothing.
    let deadline = tokio::time::Instant::now() + T;
    let again = loop {
        let call = w.mcp.call("notify", notify_args(&w, "n1")).await;
        if !call.is_error || tokio::time::Instant::now() > deadline {
            break call;
        }
        let code = call.code();
        assert!(
            code == "station_unreachable" || code == "authority_unknown",
            "{}",
            call.raw
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let kept = ["unknown", "pending", "done"];
    let state = again.ok()["state"].as_str().unwrap().to_owned();
    assert!(kept.contains(&state.as_str()), "{}", again.raw);
    // The status says `pending` while the record is open (§4.8): neither a
    // second run nor a new record.
    // The link of the station to the new deck may still settle: the same
    // tolerance as above, until the deadline.
    let deadline = tokio::time::Instant::now() + T;
    let status = loop {
        let call = w
            .mcp
            .call("desktop_action_status", json!({"request_id": "n1"}))
            .await;
        if !call.is_error || tokio::time::Instant::now() > deadline {
            break call.ok();
        }
        let code = call.code();
        assert!(
            code == "station_unreachable" || code == "authority_unknown",
            "{}",
            call.raw
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let state = status["state"].as_str().unwrap().to_owned();
    assert!(kept.contains(&state.as_str()), "{status}");
    assert_eq!(status["operation_id"], again.json()["operation_id"]);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(w.b.rig.calls_of("notify-send").is_empty());
}

// ------------------------------------------------------------------ proof 20

/// Proofs 20 and 28: the owner deletes the screenshot artifact on the board;
/// the image read then answers `unknown_artifact` and returns no bytes.
#[tokio::test]
async fn a_deleted_screenshot_artifact_is_not_read_again() {
    let w = World::start().await;
    w.b.grim(&png_of(2_000, b"deleted soon"));
    let committed = shot(&w, "s1").await;
    let artifact = committed["artifact"].clone();
    let read = w
        .mcp
        .call("read_capture_image", json!({"artifact": artifact}))
        .await;
    assert_eq!(read.content.len(), 2);
    let (c, h, elevation) = w.deck().board_elevated().await;
    let id = artifact["artifact_id"].as_str().unwrap();
    let mut headers = settings_support::refs(&h);
    headers.push(("X-FleetDeck-Elevation", elevation.as_str()));
    let gone = c
        .req(
            "DELETE",
            &format!("/api/artifacts/beta/{id}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(gone.status, 200, "{}", gone.text);
    let after = w
        .mcp
        .call("read_capture_image", json!({"artifact": artifact}))
        .await;
    assert_eq!(after.code(), "unknown_artifact");
    assert!(after.content.iter().all(|b| b["type"] == "text"));
    assert!(!after.raw.contains("image/png"));
}

// ------------------------------------------------------------------ proof 33

/// Proof 33: a target station whose hello lacks `desktop_mcp.v1` cannot be a
/// station of a grant; one that has it can (the refusal is not the one of an
/// unrelated check).
#[tokio::test]
async fn a_target_without_desktop_mcp_cannot_be_a_grant_station() {
    let w = World::with(Setup {
        b_without: vec![feature::DESKTOP_MCP],
        grant: false,
        ..Setup::default()
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let refused = start_grant(
        &c,
        &h,
        &elevation,
        "alpha",
        (&["desktop"], &[w.b_id.as_str()]),
    )
    .await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, "unsupported"),
        "{}",
        refused.body
    );
    assert!(
        refused
            .body
            .to_string()
            .contains("The selected peers do not support this desktop grant."),
        "{}",
        refused.body
    );
    assert!(c.store.grants(10).await.unwrap().is_empty());

    let w = World::with(Setup {
        b_without: Vec::new(),
        grant: false,
        ..Setup::default()
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let passed = start_grant(
        &c,
        &h,
        &elevation,
        "alpha",
        (&["desktop"], &[w.b_id.as_str()]),
    )
    .await;
    assert_ne!(passed.code(), "unsupported", "{}", passed.body);
}

/// Proofs 33 and 36: B reconnects without `capture_mcp.v1` after the grant
/// exists. A screenshot answers `desktop_unsupported`; the catalog of A is
/// the same as before; a desktop tool still works.
#[tokio::test]
async fn a_downgrade_after_the_grant_refuses_the_screenshot_and_keeps_the_catalog() {
    let w = World::start().await;
    let before = w.mcp.names().await;
    for name in ["notify", "screenshot", "read_capture_image", "wall_show"] {
        assert!(before.iter().any(|n| n == name), "{name} in {before:?}");
    }
    w.b.link(
        w.deck().addr,
        w.identity.public_key(),
        &[feature::CAPTURE_MCP],
    )
    .await;
    let (store, b) = (w.deck().store.clone(), w.b_id.clone());
    eventually("B without capture_mcp", || {
        let (store, b) = (store.clone(), b.clone());
        async move { store.link_feature(&b, feature::CAPTURE_MCP) == LinkFeature::Without }
    })
    .await;
    eventually("B with desktop_mcp", || {
        let (store, b) = (store.clone(), b.clone());
        async move { store.link_feature(&b, feature::DESKTOP_MCP) == LinkFeature::Negotiated }
    })
    .await;
    let args = json!({"request_id": "s1", "station": w.to_b(), "monitor": "DP-1"});
    assert_eq!(
        w.mcp.call("screenshot", args).await.code(),
        "desktop_unsupported"
    );
    assert_eq!(w.mcp.names().await, before);
    assert!(w.b.rig.calls_of("grim").is_empty());
    let sent = w.mcp.call("notify", notify_args(&w, "n1")).await;
    assert_eq!(sent.ok(), json!({}));
}

// ------------------------------------------------------------------ proof 9

/// The open item of O12-11: the real `LinkCheck` of the target against a real
/// control of a deck that never answers `desktop.authority_check` fails the
/// effect with `authority_unknown` within the deadline of 5 seconds. A deck
/// that answers `valid` lets the same kind of action run once.
#[tokio::test]
async fn a_deck_that_never_answers_the_authority_check_stops_the_effect_in_time() {
    mcp::record_logs();
    let tmp = tempfile::tempdir().unwrap();
    let deck = std::sync::Arc::new(fleetdeck_core::identity::Identity::generate().unwrap());
    let rig = Rig::new();
    fleetdeck_station::desktop::install(std::sync::Arc::clone(&rig.desktop));
    let b = StationB::new(&tmp.path().join("b"), rig, &deck).await;
    let fake = FakeDeck::start(&b, &deck).await;

    fake.answer_valid(true);
    let ran = fake.notify("01ARZ3NDEKTSV4RRFFQ69G5FAV").await;
    assert!(
        matches!(ran, fleetdeck_proto::link::Outcome::Ok(_)),
        "{ran:?}"
    );
    assert_eq!(b.rig.calls_of("notify-send").len(), 1);
    assert_eq!(fake.checks.lock().unwrap().len(), 1);

    fake.answer_valid(false);
    let started = std::time::Instant::now();
    let held = fake.notify("01ARZ3NDEKTSV4RRFFQ69G5FAW").await;
    let took = started.elapsed();
    let fleetdeck_proto::link::Outcome::Err(e) = held else {
        panic!("the effect ran without an answer of the deck");
    };
    assert_eq!(e.code, "authority_unknown");
    assert!(took >= std::time::Duration::from_millis(4_500), "{took:?}");
    assert!(took <= std::time::Duration::from_secs(6), "{took:?}");
    assert_eq!(fake.checks.lock().unwrap().len(), 2);
    assert_eq!(b.rig.calls_of("notify-send").len(), 1);
}
