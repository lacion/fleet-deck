//! The P5b-1 contract end to end over a real link (R13 P5b task P1): a real
//! deck (real deck crate, a database file), a real station B (real station
//! crate with the fake desktop and a fake `wf-recorder` behind the
//! `shim-exec` seam) and the owner on the board. The crate tests of the
//! tasks F, E1-E3, S1, S2, K1 and K2 are not repeated here; each test names
//! the proof of `docs/v2/design/omarchy-p5b.md` §17 that it shows.
//!
//! Every recording lasts two or three seconds. B runs its lease renewal and
//! watchdog at the design periods (one second), so a discard trigger shows
//! within about two seconds.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod p5b_support;
mod settings_support;

use std::time::Duration;

use fleetdeck_deck::desktop_capture::recording::StopIntent;
use fleetdeck_deck::store::desktop_captures::CaptureState as DeckState;
use fleetdeck_proto::recording::StopReason;
use fleetdeck_station::store::capture_ops::{CaptureState, RecordingRow};
use p5b_support::mp4::{big_clip, clip};
use p5b_support::rig::RecorderRig;
use p5b_support::*;
use serde_json::{Value, json};

/// The committed artifact of a finished start answer: its ID.
fn artifact_id(answer: &Value) -> String {
    answer["capture"]["published"]["artifact"]["artifact_id"]
        .as_str()
        .expect("an artifact ID")
        .to_owned()
}

/// The file that B committed for the artifact `id`.
fn committed_file(w: &World, id: &str) -> Vec<u8> {
    std::fs::read(w.dir_b.join("artifacts").join(id).join("1/capture.mp4")).unwrap()
}

/// The pairs of a request to the raw route: the login and a `Range`.
fn ranged(w: &World, range: Option<&str>) -> wire::Pairs {
    let mut h = w.cookie();
    if let Some(range) = range {
        h.push(("Range".to_owned(), range.to_owned()));
    }
    h
}

async fn raw(w: &World, id: &str, method: &str, range: Option<&str>) -> wire::Reply {
    w.client
        .send(method, &World::raw_path(id), &ranged(w, range), None)
        .await
}

/// Waits until the fake recorder of B is gone: no process of it lives, and
/// the PID that it wrote has no `/proc` entry.
async fn recorder_gone(w: &World) {
    let pid = w.b.rig.recorder_pid().expect("the recorder ran");
    eventually("the recorder is gone", || async {
        w.b.rig.live_recorders() == 0 && !std::path::Path::new(&format!("/proc/{pid}")).exists()
    })
    .await;
}

/// Waits until the station row of `id` is terminal.
async fn station_ends(w: &World, id: &str) -> RecordingRow {
    eventually("a terminal station row", || async {
        w.station_row(id)
            .await
            .is_some_and(|r| r.op.state != CaptureState::Running)
    })
    .await;
    w.station_row(id).await.unwrap()
}

/// A station row that failed with a discard: no artifact, no reservation,
/// no staging folder, no artifact folder, and the reason that was latched.
/// The reservation is zero only after the removal of the files: the check
/// reads the folders after it read the reservation.
async fn assert_station_discarded(w: &World, id: &str, reasons: &[StopReason]) {
    let row = station_ends(w, id).await;
    assert_eq!(row.op.state, CaptureState::Failed, "{row:?}");
    assert_eq!(row.op.code.as_deref(), Some("recording_interrupted"));
    assert!(
        row.stop_reason.is_some_and(|r| reasons.contains(&r)),
        "{:?} not in {reasons:?}",
        row.stop_reason
    );
    eventually("the reservation is released", || async {
        w.station_row(id).await.unwrap().op.reserved_bytes == 0
    })
    .await;
    assert_eq!(w.leftovers(), (Vec::new(), Vec::new()));
    recorder_gone(w).await;
}

// ------------------------------------------------------------------ proof 1

/// Proof 1 (§17 items 4, 9, 11, 12, 13, 14): the owner starts a recording on
/// the board route, the lease runs, the duration ends, B commits, the deck
/// indexes a `video` artifact with recording provenance, the status is
/// `committed`, and the raw route serves `206` for `bytes=0-99` with the
/// exact bytes. HEAD answers the headers while B is gone.
#[tokio::test]
async fn an_owner_recording_commits_is_indexed_as_video_and_serves_the_exact_range() {
    let w = World::start().await;
    let answer = w.start_recording(ID, 3).await.unwrap();
    assert_eq!(answer.status, 200, "{}", answer.text());
    let body = answer.json();
    assert!(body["address"].is_null());
    assert_eq!(body["capture"]["capture_id"], ID);
    let published = &body["capture"]["published"];
    assert_eq!(published["artifact"]["kind"], "video");
    assert_eq!(published["artifact"]["station_id"], w.b_id.as_str());
    assert_eq!(published["artifact"]["author"], "owner");
    assert!(
        published["artifact"]["title"]
            .as_str()
            .is_some_and(|t| t.starts_with("Desktop recording") && t.ends_with("/DP-1")),
        "{published}"
    );
    assert_eq!(published["version"]["files"], json!(["capture.mp4"]));
    let provenance = &published["version"]["capture"];
    assert_eq!(provenance["kind"], "recording");
    assert_eq!(provenance["caller"], "owner");
    assert_eq!(provenance["capture_id"], ID);
    assert_eq!(provenance["monitor"], "DP-1");
    assert_eq!(provenance["station_id"], w.b_id.as_str());
    let meta = &provenance["recording"];
    assert_eq!(meta["requested_seconds"], 3);
    assert_eq!(meta["stop_reason"], "duration");
    assert_eq!(meta["duration_ms"], 3000);
    assert_eq!(
        (meta["width"].as_u64(), meta["height"].as_u64()),
        (Some(3440), Some(1440))
    );
    let id = artifact_id(&body);
    assert_eq!(w.b.rig.recorder_runs(), 1);
    // The station and the deck rows are terminal; the deck indexed it.
    let station = w.station_row(ID).await.unwrap();
    assert_eq!(station.op.state, CaptureState::Committed);
    assert_eq!(station.stop_reason, Some(StopReason::Duration));
    let deck = w.deck_row(ID).await.unwrap();
    assert_eq!(
        (deck.capture.state, deck.capture.indexed),
        (DeckState::Committed, true)
    );
    // The board status and the artifact list.
    let status = w.status(ID).await;
    assert_eq!(status.status, 200, "{}", status.text());
    assert_eq!(status.json()["state"], "committed");
    assert_eq!(status.json()["published"]["artifact"]["artifact_id"], id);
    let list = w.artifacts(true).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(
        (list[0]["artifact_id"].as_str(), list[0]["kind"].as_str()),
        (Some(id.as_str()), Some("video"))
    );
    // The raw route: a closed range, a suffix, an open range, the whole file.
    let file = committed_file(&w, &id);
    let n = file.len();
    assert!(n > 100, "{n}");
    let first = raw(&w, &id, "GET", Some("bytes=0-99")).await;
    assert_eq!(first.status, 206, "{}", first.text());
    assert_eq!(
        first.header("content-range"),
        Some(format!("bytes 0-99/{n}").as_str())
    );
    assert_eq!(first.header("accept-ranges"), Some("bytes"));
    assert_eq!(first.header("content-type"), Some("video/mp4"));
    assert_eq!(first.length(), 100);
    assert!(first.body == file[..100], "the first 100 bytes");
    let tail = raw(&w, &id, "GET", Some("bytes=-50")).await;
    assert_eq!(tail.status, 206);
    assert!(tail.body == file[n - 50..], "the last 50 bytes");
    let mid = raw(&w, &id, "GET", Some("bytes=1000-")).await;
    assert_eq!(mid.status, 206);
    assert_eq!(
        mid.header("content-range"),
        Some(format!("bytes 1000-{}/{n}", n - 1).as_str())
    );
    assert!(mid.body == file[1000..], "from byte 1000");
    let whole = raw(&w, &id, "GET", None).await;
    assert_eq!((whole.status, whole.length()), (200, n));
    assert!(whole.body == file, "the whole file");
    // HEAD: the headers and no body, with B gone. A GET cannot be served.
    w.b.unlink();
    let (store, b) = (w.deck().store.clone(), w.b_id.clone());
    eventually("the station is gone", || async { !store.is_reachable(&b) }).await;
    let head = raw(&w, &id, "HEAD", Some("bytes=0-9")).await;
    assert_eq!(head.status, 200, "{}", head.text());
    assert_eq!(head.length(), n);
    assert_eq!(head.header("accept-ranges"), Some("bytes"));
    assert_eq!(head.header("content-type"), Some("video/mp4"));
    assert!(head.header("etag").is_some());
    assert!(head.body.is_empty());
    let offline = raw(&w, &id, "GET", Some("bytes=0-99")).await;
    assert!(
        offline.status >= 400,
        "{} {}",
        offline.status,
        offline.text()
    );
}

// ------------------------------------------------------------------ proof 2

/// Proof 2 (§17 items 4, 6): the owner stops a recording in the middle; the
/// next lease carries `save`; B commits the clip that the recorder wrote
/// (one second of ten); a repeated stop is idempotent and changes nothing.
#[tokio::test]
async fn an_owner_stop_saves_a_shorter_clip_and_a_repeated_stop_changes_nothing() {
    let w = World::start().await;
    // The recorder of this test ends its file at one second.
    w.b.rig.fixture(10, &clip(WIDTH, HEIGHT, 1));
    let start = w.start_recording(ID, 10);
    w.until_recording(ID).await;
    let first = w.stop(ID).await;
    assert!(
        matches!(first.status, 200 | 202),
        "{} {}",
        first.status,
        first.text()
    );
    let answer = start.await.unwrap();
    assert_eq!(answer.status, 200, "{}", answer.text());
    let body = answer.json();
    let meta = &body["capture"]["published"]["version"]["capture"]["recording"];
    assert_eq!(meta["stop_reason"], "owner_stop");
    assert_eq!(meta["requested_seconds"], 10);
    assert_eq!(meta["duration_ms"], 1000);
    let id = artifact_id(&body);
    let station = w.station_row(ID).await.unwrap();
    assert_eq!(station.op.state, CaptureState::Committed);
    assert_eq!(station.stop_reason, Some(StopReason::OwnerStop));
    let deck = w.deck_row(ID).await.unwrap();
    assert_eq!(
        (deck.stop_intent, deck.stop_reason),
        (StopIntent::Save, Some(StopReason::OwnerStop))
    );
    // A repeated stop is the terminal status: the same artifact, no new
    // recorder, no second artifact.
    for _ in 0..2 {
        let again = w.stop(ID).await;
        assert_eq!(again.status, 200, "{}", again.text());
        assert_eq!(again.json()["state"], "committed");
        assert_eq!(again.json()["published"]["artifact"]["artifact_id"], id);
        assert_eq!(again.json()["stop_reason"], "owner_stop");
    }
    assert_eq!(w.b.rig.recorder_runs(), 1);
    assert_eq!(w.artifacts(true).await.len(), 1);
    let (staging, artifacts) = w.leftovers();
    assert_eq!((staging, artifacts), (Vec::new(), vec![id.clone()]));
    recorder_gone(&w).await;
    // The file is the shorter clip.
    assert_eq!(committed_file(&w, &id), clip(WIDTH, HEIGHT, 1));
}

// ------------------------------------------------------------------ proof 3

/// The owner start in a task, until the recorder runs.
async fn recording(w: &World, seconds: u32) -> tokio::task::JoinHandle<wire::Reply> {
    let start = w.start_recording(ID, seconds);
    w.until_recording(ID).await;
    start
}

/// The answer of a discarded start: `409 recording_interrupted`, and a deck
/// row that is `failed` with that code.
async fn assert_deck_interrupted(w: &World, answer: &wire::Reply) {
    assert_eq!(answer.status, 409, "{}", answer.text());
    assert_eq!(answer.code(), "recording_interrupted");
    let deck = w.deck_row(ID).await.unwrap();
    assert_eq!(deck.capture.state, DeckState::Failed);
    assert_eq!(deck.capture.code.as_deref(), Some("recording_interrupted"));
    assert!(!deck.capture.indexed);
}

/// Proof 3 (§17 items 18, 21): the deck switch `desktop_control` turns off
/// during the recording; the next lease answers `discard` with `deck_policy`.
#[tokio::test]
async fn a_deck_switch_that_turns_off_discards_at_the_next_lease() {
    let w = World::start().await;
    let start = recording(&w, 30).await;
    w.desktop_control(false);
    let answer = start.await.unwrap();
    assert_deck_interrupted(&w, &answer).await;
    assert_station_discarded(&w, ID, &[StopReason::DeckPolicy]).await;
    assert!(w.artifacts(true).await.is_empty());
    assert_eq!(w.b.rig.recorder_runs(), 1);
}

/// Proof 3 (§17 items 18, 21): the owner login ends during the recording
/// (the board logout); the next lease answers `discard` with
/// `owner_login_ended`.
#[tokio::test]
async fn an_owner_login_that_ends_discards_at_the_next_lease() {
    let mut w = World::start().await;
    let start = recording(&w, 30).await;
    w.logout().await;
    let answer = start.await.unwrap();
    assert_deck_interrupted(&w, &answer).await;
    assert_station_discarded(&w, ID, &[StopReason::OwnerLoginEnded]).await;
    w.relogin().await;
    assert!(w.artifacts(true).await.is_empty());
    assert_eq!(w.b.rig.recorder_runs(), 1);
}

/// Proof 3 (§17 items 18, 21): `allow_desktop_capture` turns off on B. B
/// discards within three seconds, with no artifact. Two checks of B see the
/// change: its own watchdog (`policy_off`) and the live policy answer for the
/// next lease, which the deck refuses (`lease_denied`). Both tick once a
/// second and no seam isolates one of them, so the first one that latches
/// wins; the test accepts either reason.
#[tokio::test]
async fn a_target_that_turns_capture_off_discards_within_the_bound() {
    let w = World::start().await;
    let start = recording(&w, 30).await;
    let flipped = std::time::Instant::now();
    w.b.policy(true, true, false);
    let answer = start.await.unwrap();
    assert!(
        flipped.elapsed() < Duration::from_secs(3),
        "{:?}",
        flipped.elapsed()
    );
    assert_deck_interrupted(&w, &answer).await;
    assert_station_discarded(&w, ID, &[StopReason::PolicyOff, StopReason::LeaseDenied]).await;
    assert!(w.artifacts(true).await.is_empty());
    assert_eq!(w.b.rig.recorder_runs(), 1);
}

// ------------------------------------------------------------------ proof 4

/// Proof 4 (§17 items 19, 23, 30): the link of B closes during the
/// recording. B discards at once (`link_closed`, or `lease_denied` when the
/// renewal in flight fails first): no artifact, the recorder
/// is gone, the reservation is released only after the durable removal of
/// the files. When B links again, the deck learns the failure and never
/// starts a second recording.
#[tokio::test]
async fn a_closed_link_discards_the_recording_and_releases_the_reservation_after_the_cleanup() {
    let w = World::start().await;
    let start = recording(&w, 30).await;
    assert!(w.station_row(ID).await.unwrap().op.reserved_bytes > 0);
    w.b.unlink();
    // At every look: a released reservation means that the files are gone.
    eventually("the discard", || async {
        let row = w.station_row(ID).await.unwrap();
        if row.op.reserved_bytes == 0 && row.op.state != CaptureState::Running {
            assert_eq!(w.leftovers(), (Vec::new(), Vec::new()));
            true
        } else {
            false
        }
    })
    .await;
    // The renewal in flight fails with the link, or the link watcher sees the
    // end first: the first discard that is latched wins.
    assert_station_discarded(&w, ID, &[StopReason::LinkClosed, StopReason::LeaseDenied]).await;
    // The deck request did not get a recording.
    let answer = start.await.unwrap();
    assert_ne!(answer.status, 200, "{}", answer.text());
    // B links again: the lookup shows the failure; nothing is recorded again.
    w.b.link(w.deck().addr, w.identity.public_key(), &[]).await;
    w.wait_reachable().await;
    let status = w.status(ID).await;
    assert_eq!(status.json()["state"], "failed", "{}", status.text());
    assert_eq!(status.json()["error"], "recording_interrupted");
    let deck = w.deck_row(ID).await.unwrap();
    assert_eq!(deck.capture.state, DeckState::Failed);
    assert!(w.artifacts(true).await.is_empty());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(w.b.rig.recorder_runs(), 1);
}

// ------------------------------------------------------------------ proof 5

/// Proof 5 (§17 items 19, 28, 31): the deck restarts during the recording.
/// B loses its lease peer and discards. After the restart the status is
/// terminal and nothing records a second time.
#[tokio::test]
async fn a_deck_restart_discards_the_recording_and_never_records_twice() {
    let mut w = World::start().await;
    let _start = recording(&w, 30).await;
    w.restart_deck().await;
    assert_station_discarded(
        &w,
        ID,
        &[
            StopReason::LinkClosed,
            StopReason::LeaseExpired,
            StopReason::LeaseDenied,
        ],
    )
    .await;
    // The new deck has the row (`attempted` before the crash) and learns the
    // outcome from the station.
    let status = w.status(ID).await;
    assert_eq!(status.json()["state"], "failed", "{}", status.text());
    assert_eq!(status.json()["error"], "recording_interrupted");
    let deck = w.deck_row(ID).await.unwrap();
    assert_eq!(deck.capture.state, DeckState::Failed);
    assert!(w.artifacts(true).await.is_empty());
    // The same capture ID sent again answers the stored failure; it does not
    // record.
    let again = w.start_recording(ID, 30).await.unwrap();
    assert_eq!(again.status, 409, "{}", again.text());
    assert_eq!(again.code(), "recording_interrupted");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(w.b.rig.recorder_runs(), 1);
    assert_eq!(w.leftovers(), (Vec::new(), Vec::new()));
}

// ------------------------------------------------------------------ proof 6

/// Proof 6 (§17 items 32, 40): the owner deletes the artifact while a range
/// read is in progress. The body ends before its `Content-Length`; a new
/// range read answers 404.
#[tokio::test]
async fn a_deletion_during_a_range_read_cuts_the_body_and_a_new_read_is_404() {
    let w = World::with(Setup { password: true }).await;
    // One file of about 27 MB: far more than the socket buffers hold.
    w.b.rig.fixture(2, &big_clip(WIDTH, HEIGHT, 2, 450_000));
    let answer = w.start_recording(ID, 2).await.unwrap();
    assert_eq!(answer.status, 200, "{}", answer.text());
    let id = artifact_id(&answer.json());
    let size = committed_file(&w, &id).len();
    assert!(size > 20_000_000, "{size}");
    let mut read = w
        .client
        .open(
            "GET",
            &World::raw_path(&id),
            &ranged(&w, Some("bytes=100-")),
            None,
            true,
        )
        .await;
    assert_eq!(read.status, 206);
    let wanted: usize = read
        .headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .unwrap()
        .1
        .parse()
        .unwrap();
    assert_eq!(wanted, size - 100);
    let got = read.read_at_least(200_000).await;
    assert!(got >= 200_000 && got < wanted, "{got} of {wanted}");
    // Delete while the body is in flight.
    let mut headers = w.headers(None, true);
    headers.push(("X-FleetDeck-Elevation".to_owned(), w.elevation().await));
    let deleted = w
        .client
        .send(
            "DELETE",
            &format!("/api/artifacts/beta/{id}"),
            &headers,
            None,
        )
        .await;
    assert!(
        (200..300).contains(&deleted.status),
        "{} {}",
        deleted.status,
        deleted.text()
    );
    let rest = read.finish().await;
    assert!(rest.body.len() < wanted, "{} of {wanted}", rest.body.len());
    let after = raw(&w, &id, "GET", Some("bytes=0-99")).await;
    assert_eq!(
        (after.status, after.code().as_str()),
        (404, "unknown_artifact")
    );
    assert!(w.artifacts(true).await.is_empty());
}

// ------------------------------------------------------------------ proof 7

/// Proof 7 (§17 items 44, 47; §14): a board request without the feature
/// header lists the recording as `other` and shows no recording provenance;
/// with the header it is `video`. The start answer is projected the same way.
#[tokio::test]
async fn a_board_without_the_feature_header_sees_the_recording_as_other() {
    let w = World::start().await;
    w.b.rig.fixture(2, &clip(WIDTH, HEIGHT, 2));
    let with = w.start_recording(ID, 2).await.unwrap();
    assert_eq!(with.status, 200, "{}", with.text());
    let id = artifact_id(&with.json());
    assert_eq!(
        with.json()["capture"]["published"]["artifact"]["kind"],
        "video"
    );
    let kind_of = |list: &[Value]| {
        list.iter()
            .map(|a| a["kind"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(kind_of(&w.artifacts(false).await), ["other"]);
    assert_eq!(kind_of(&w.artifacts(true).await), ["video"]);
    let old = w.get(&format!("/api/artifacts/beta/{id}"), false).await;
    assert_eq!(old.status, 200, "{}", old.text());
    assert_eq!(old.json()["artifact"]["kind"], "other");
    assert!(old.json()["versions"][0]["capture"].is_null());
    let new = w.get(&format!("/api/artifacts/beta/{id}"), true).await;
    assert_eq!(new.json()["artifact"]["kind"], "video");
    assert_eq!(new.json()["versions"][0]["capture"]["kind"], "recording");
    // The status of the same capture, and a repeated start, are projected too.
    let status = w
        .get(&format!("/api/stations/beta/desktop/captures/{ID}"), false)
        .await;
    assert_eq!(status.json()["state"], "committed");
    assert_eq!(status.json()["published"]["artifact"]["kind"], "other");
    let mut headers = w.headers(Some(ID), false);
    headers.retain(|(k, _)| k != VIDEO.0);
    let body = json!({"action": "screen_record", "monitor": "DP-1", "seconds": 2});
    let plain = w.client.post(World::act_path(), &headers, body).await;
    assert_eq!(plain.status, 200, "{}", plain.text());
    assert_eq!(
        plain.json()["capture"]["published"]["artifact"]["kind"],
        "other"
    );
    assert!(plain.json()["capture"]["published"]["version"]["capture"].is_null());
    // The repeat did not record again, and the stored kind never changed.
    assert_eq!(w.b.rig.recorder_runs(), 1);
    assert_eq!(kind_of(&w.artifacts(true).await), ["video"]);
}

// ------------------------------------------------------------------ proof 8

/// Proof 8 (§17 item 7): the active discovery shows the running recording
/// with its progress from a fresh request, and `idle` after the end.
#[tokio::test]
async fn the_active_discovery_shows_the_recording_with_progress_and_then_idle() {
    let w = World::start().await;
    let idle = w.active().await;
    assert_eq!(idle.status, 200, "{}", idle.text());
    assert_eq!(idle.json(), json!({"state": "idle"}));
    let start = recording(&w, 3).await;
    let active = w.active().await;
    assert_eq!(active.status, 200, "{}", active.text());
    let body = active.json();
    assert_eq!(body["state"], "active", "{body}");
    assert_eq!(body["capture_id"], ID);
    assert_eq!(body["status"]["state"], "pending");
    assert_eq!(body["status"]["capture_id"], ID);
    let progress = &body["status"]["recording"];
    assert_eq!(progress["phase"], "recording", "{body}");
    assert_eq!(progress["requested_seconds"], 3);
    assert!(
        progress["started_at"].as_i64().is_some_and(|t| t > 0),
        "{body}"
    );
    assert!(progress["elapsed_ms"].as_u64().is_some(), "{body}");
    let done = start.await.unwrap();
    assert_eq!(done.status, 200, "{}", done.text());
    let after = w.active().await;
    assert_eq!(after.status, 200, "{}", after.text());
    assert_eq!(after.json(), json!({"state": "idle"}));
}
