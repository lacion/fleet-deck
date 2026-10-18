//! The owner stop, the active discovery and the recording status of
//! `board_desktop_recording` (R13 P5b-1 task K1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::capture_support::*;
use crate::common::next_request;
use crate::desktop_support::*;
use crate::share_support::*;
use crate::support::*;
use fleetdeck_deck::desktop_capture::recording::{NewRecording, RecordingRow, StopIntent};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::recording::{LeaseDecision, LeasePhase, StopReason};
use serde_json::json;
use std::collections::BTreeSet;

#[tokio::test]
async fn an_owner_stop_is_committed_before_the_send_is_idempotent_and_carried_by_the_next_lease() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        rec_ok(ID, "V1", Some(owner_stop_meta())),
        async |station, act| {
            for _ in 0..2 {
                let post = stop_post(&c, &h, &st, ID, true);
                let serve = async {
                    let req = next_request(station).await;
                    assert_eq!(req.method, "desktop.capture_stop");
                    assert_eq!(
                        req.params,
                        json!({ "capture_id": ID, "reason": "owner_stop" })
                    );
                    // The intent is in the row before the station gets the stop.
                    let row = rec_row(&f, ID).await.unwrap();
                    assert_eq!(
                        (row.stop_intent, row.stop_reason),
                        (StopIntent::Save, Some(StopReason::OwnerStop))
                    );
                    station
                        .control
                        .respond(req.id, Outcome::Ok(pending_status(ID, "stopping")))
                        .await
                        .unwrap();
                };
                let (r, ()) = tokio::join!(post, serve);
                assert_eq!(r.status, 202, "{}", r.body);
                assert_eq!(r.body["state"], "pending");
                assert_eq!(r.body["recording"]["phase"], "stopping");
                assert_eq!(r.body["stop_reason"], "owner_stop");
            }
            // The next lease carries the save.
            let out = lease(station, act, LeasePhase::Record, Some("ok")).await;
            assert_eq!(
                answer_of(out),
                (LeaseDecision::Save, Some(StopReason::OwnerStop))
            );
            let out = lease(station, act, LeasePhase::Import, Some("ok")).await;
            assert_eq!(
                answer_of(out),
                (LeaseDecision::Save, Some(StopReason::OwnerStop))
            );
        },
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Terminal now: 200, and the station is not asked again.
    let r = stop_post(&c, &h, &st, ID, false).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "committed");
    assert_eq!(r.body["stop_reason"], "owner_stop");
    // No header: the legacy projection.
    assert_eq!(r.body["published"]["artifact"]["kind"], "other");
    let r = stop_post(&c, &h, &st, ID, true).await;
    assert_eq!(r.body["published"]["artifact"]["kind"], "video");
    quiet(&mut station).await;
    // Two rows for each stop that was sent: the intent and the outcome.
    let rows = c.audited("desktop.capture_stop", AuditResult::Ok).await;
    assert_eq!(rows.len(), 8);
    assert!(
        rows.iter()
            .flatten()
            .all(|d| d.starts_with(&format!("capture={ID} stage=")))
    );
    assert!(rows.contains(&Some(format!(
        "capture={ID} stage=intent intent=save reason=owner_stop"
    ))));
}

#[tokio::test]
async fn a_stored_discard_is_never_replaced_and_a_stop_works_with_the_switch_off() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let failed = err("recording_interrupted", SENTINEL);
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        failed,
        async |station, act| {
            let row = f
                .store
                .recording_stop_intent(
                    f.station.clone(),
                    ID.to_owned(),
                    false,
                    StopReason::Deadline,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (row.stop_intent, row.stop_reason),
                (StopIntent::Discard, Some(StopReason::Deadline))
            );
            // Turning the switch off never disables the stop.
            enable(&c, false);
            let post = stop_post(&c, &h, &st, ID, true);
            let serve = async {
                let req = next_request(station).await;
                assert_eq!(req.method, "desktop.capture_stop");
                station
                    .control
                    .respond(req.id, err("not_found", "x"))
                    .await
                    .unwrap();
            };
            let (r, ()) = tokio::join!(post, serve);
            // The send failed: the answer is the durable status, not an error.
            assert_eq!(r.status, 202, "{}", r.body);
            assert_eq!(r.body["state"], "pending");
            let row = rec_row(&f, ID).await.unwrap();
            assert_eq!(
                (row.stop_intent, row.stop_reason),
                (StopIntent::Discard, Some(StopReason::Deadline))
            );
            enable(&c, true);
            let out = lease(station, act, LeasePhase::Record, Some("ok")).await;
            assert_eq!(
                answer_of(out),
                (LeaseDecision::Discard, Some(StopReason::Deadline))
            );
            // A discard is the only way out of a save: not the other way round.
            let out = lease(station, act, LeasePhase::Import, None).await;
            assert_eq!(wire_code(&out), "forbidden");
        },
    )
    .await;
    expect(&r, 409, "recording_interrupted");
    assert!(!r.text.contains(SENTINEL));
    let row = rec_row(&f, ID).await.unwrap();
    assert_eq!(row.capture.state, CaptureState::Failed);
    assert_eq!(row.stop_reason, Some(StopReason::Deadline));
}

#[tokio::test]
async fn a_screenshot_an_unknown_capture_a_bad_id_and_a_bad_body_are_refused_and_an_offline_stop_is_kept()
 {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    f.store
        .capture_prepare(f.station.clone(), nth_id(2), "shot".into(), "DP-1".into())
        .await
        .unwrap();
    let hb = headers(&h, &c, None, true);
    let hs = refs(&hb);
    let r = c.post(&stop_path(&st, &nth_id(2)), &hs, json!({})).await;
    expect(&r, 409, "not_recording");
    assert_eq!(r.body["message"], "This capture is not a recording.");
    expect(
        &c.post(&stop_path(&st, &nth_id(3)), &hs, json!({})).await,
        404,
        "unknown_capture",
    );
    expect(
        &c.post(&stop_path(&st, "abc"), &hs, json!({})).await,
        400,
        "invalid_capture_id",
    );
    expect(
        &c.post(&stop_path("nope", ID), &hs, json!({})).await,
        404,
        "unknown_station",
    );
    expect(
        &c.post(&stop_path(&st, ID), &hs, json!({ "reason": "deck_policy" }))
            .await,
        400,
        "invalid_body",
    );
    let (_, token) = f
        .store
        .create_token(
            "t".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    expect(
        &c.post(
            &stop_path(&st, ID),
            &[("Authorization", auth.as_str())],
            json!({}),
        )
        .await,
        403,
        "owner_only",
    );
    quiet(&mut station).await;
    // A recording of an offline station: the intent is kept, nothing is sent.
    f.store
        .recording_prepare(fleetdeck_deck::desktop_capture::recording::NewRecording {
            station: f.station.clone(),
            capture_id: nth_id(4),
            action_hash: "h".into(),
            monitor: "DP-1".into(),
            seconds: 5,
            owner_login: "login".into(),
            link_epoch: "e".into(),
        })
        .await
        .unwrap();
    f.store
        .capture_attempted(f.station.clone(), nth_id(4))
        .await
        .unwrap();
    f.drop_link(station).await;
    let r = c.post(&stop_path(&st, &nth_id(4)), &hs, json!({})).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(
        rec_row(&f, &nth_id(4)).await.unwrap().stop_intent,
        StopIntent::Save
    );
}

#[tokio::test]
async fn the_active_recording_is_idle_active_or_unreachable_and_is_the_deck_row_with_the_station_progress()
 {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let active = active_path(&st);
    let get_active = || get_with(&c, &h, &active, true);
    let (r, req) = tokio::join!(
        get_active(),
        serve_one(
            &mut station,
            "desktop.capture_active",
            json!({ "state": "idle" })
        )
    );
    assert_eq!((r.status, &r.body), (200, &json!({ "state": "idle" })));
    assert_eq!(req.params, json!({}));
    // An active recording: the station text never reaches the board.
    let reply = json!({ "state": "active", "capture_id": ID, "status": {
        "state": "failed", "capture_id": ID, "error": "recording_failed", "message": SENTINEL,
        "recording": { "phase": "recording", "requested_seconds": 30, "started_at": 1_000, "elapsed_ms": 4_000 },
        "stop_reason": "owner_stop" } });
    let (r, ()) = during((&c, &h, &st), &mut station, ID, err("recording_failed", "x"), async |station, _| {
        let (r, _) = tokio::join!(get_active(), serve_one(station, "desktop.capture_active", reply.clone()));
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["state"], "active");
        assert_eq!(r.body["capture_id"], ID);
        assert_eq!(r.body["status"]["state"], "pending");
        assert_eq!(r.body["status"]["recording"]["phase"], "recording");
        assert_eq!(r.body["status"]["recording"]["elapsed_ms"], 4_000);
        assert!(!r.text.contains(SENTINEL));
        // An answer for another capture is invalid.
        let other = json!({ "state": "active", "capture_id": nth_id(9), "status": pending_status(ID, "recording") });
        let (r, _) = tokio::join!(get_active(), serve_one(station, "desktop.capture_active", other));
        expect(&r, 502, "desktop_failed");
    })
    .await;
    expect(&r, 502, "recording_failed");
    // The owner only, and an offline target is never `idle`.
    let (_, token) = f
        .store
        .create_token(
            "t".into(),
            BTreeSet::from([Scope::Read, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    expect(
        &c.get(&active_path(&st), &[("Authorization", auth.as_str())])
            .await,
        403,
        "owner_only",
    );
    f.drop_link(station).await;
    let r = get_active().await;
    expect(&r, 409, "station_unreachable");
}

#[tokio::test]
async fn the_status_of_a_recording_shows_progress_only_from_a_current_answer() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let status_path = lookup_path(&st, ID);
    let get_status = |video: bool| get_with(&c, &h, &status_path, video);
    let committed = rec_ok(ID, "V1", Some(meta(30)));
    let (r, ()) = during(
        (&c, &h, &st),
        &mut station,
        ID,
        committed,
        async |station, _| {
            let (r, req) = tokio::join!(
                get_status(true),
                serve_one(
                    station,
                    "desktop.capture_status",
                    pending_status(ID, "recording")
                )
            );
            assert_eq!(req.params, json!({ "capture_id": ID }));
            assert_eq!(r.status, 200, "{}", r.body);
            assert_eq!(r.body["state"], "pending");
            assert_eq!(r.body["recording"]["phase"], "recording");
            // The station does not answer and the deck wait is not over: pending,
            // and no progress.
            let (r, _) = tokio::join!(get_status(true), async {
                let req = next_request(station).await;
                station
                    .control
                    .respond(req.id, err("failed", "x"))
                    .await
                    .unwrap();
            });
            assert_eq!(
                (r.body["state"].as_str(), r.body.get("recording").is_none()),
                (Some("pending"), true)
            );
        },
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The station committed: the status shows the video, or its projection.
    let r = get_status(true).await;
    assert_eq!(
        (
            r.body["state"].as_str(),
            r.body["published"]["artifact"]["kind"].as_str()
        ),
        (Some("committed"), Some("video"))
    );
    assert_eq!(
        r.body["published"]["version"]["capture"]["kind"],
        "recording"
    );
    assert!(r.body.get("recording").is_none());
    let r = get_status(false).await;
    assert_eq!(r.body["published"]["artifact"]["kind"], "other");
    assert!(r.body["published"]["version"].get("capture").is_none());
    quiet(&mut station).await;
    // A screenshot keeps the P5a answer.
    f.store
        .capture_prepare(f.station.clone(), nth_id(2), "shot".into(), "DP-1".into())
        .await
        .unwrap();
    let r = get_with(&c, &h, &lookup_path(&st, &nth_id(2)), true).await;
    assert_eq!(r.body, state("pending", &nth_id(2)));
}

#[tokio::test]
async fn the_stop_latch_goes_none_save_discard_and_a_finished_row_never_changes() {
    let (f, _station, _c, _h) = rec_setup().await;
    let new = |n: u32| NewRecording {
        station: f.station.clone(),
        capture_id: nth_id(n),
        action_hash: format!("h{n}"),
        monitor: "DP-1".into(),
        seconds: 5,
        owner_login: "login".into(),
        link_epoch: "e".into(),
    };
    let latch = |n: u32, save: bool, reason: StopReason| {
        f.store
            .recording_stop_intent(f.station.clone(), nth_id(n), save, reason)
    };
    let both = |row: Option<RecordingRow>| {
        let row = row.unwrap();
        (row.stop_intent, row.stop_reason)
    };
    for n in 1..=4 {
        f.store.recording_prepare(new(n)).await.unwrap();
        f.store
            .capture_attempted(f.station.clone(), nth_id(n))
            .await
            .unwrap();
    }
    // none -> save -> discard; a second save and a second discard change nothing.
    assert_eq!(
        both(latch(1, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::Save, Some(StopReason::OwnerStop))
    );
    assert_eq!(
        both(latch(1, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::Save, Some(StopReason::OwnerStop))
    );
    assert_eq!(
        both(latch(1, false, StopReason::DeckPolicy).await.unwrap()),
        (StopIntent::Discard, Some(StopReason::DeckPolicy))
    );
    assert_eq!(
        both(latch(1, false, StopReason::Deadline).await.unwrap()),
        (StopIntent::Discard, Some(StopReason::DeckPolicy))
    );
    assert_eq!(
        both(latch(1, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::Discard, Some(StopReason::DeckPolicy))
    );
    // none -> discard, and a discard is never replaced by a save.
    assert_eq!(
        both(latch(2, false, StopReason::OwnerLoginEnded).await.unwrap()),
        (StopIntent::Discard, Some(StopReason::OwnerLoginEnded))
    );
    assert_eq!(
        both(latch(2, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::Discard, Some(StopReason::OwnerLoginEnded))
    );
    // A finished row and a screenshot never change; an unknown row is `None`.
    f.store
        .capture_failed(
            f.station.clone(),
            nth_id(3),
            "recording_failed".into(),
            "x".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        both(latch(3, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::None, None)
    );
    f.store
        .capture_prepare(f.station.clone(), nth_id(5), "shot".into(), "DP-1".into())
        .await
        .unwrap();
    assert_eq!(
        both(latch(5, true, StopReason::OwnerStop).await.unwrap()),
        (StopIntent::None, None)
    );
    assert!(
        latch(9, true, StopReason::OwnerStop)
            .await
            .unwrap()
            .is_none()
    );
    // Neither write moves `updated_at`, the time of the `attempted` state.
    let before = rec_row(&f, &nth_id(4)).await.unwrap().capture.updated_at;
    latch(4, true, StopReason::OwnerStop).await.unwrap();
    assert_eq!(
        rec_row(&f, &nth_id(4)).await.unwrap().capture.updated_at,
        before
    );
}
