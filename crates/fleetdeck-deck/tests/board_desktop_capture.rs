//! The owner screenshot over loopback HTTPS (R13 P5a task E): the
//! `screen_capture` branch of `POST .../desktop/act` with a fake station: the
//! authorization, the capture id header, the durable record, the indexing, the
//! error codes and the repeated POST. The lookup, the recovery and the
//! features are in `board_desktop_capture_recovery.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod common;
mod desktop_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use capture_support::*;
use common::next_request;
use desktop_support::*;
use fleetdeck_deck::desktop_capture::{self, CaptureError, CaptureOptions, Reply};
use fleetdeck_deck::desktop_relay::RelayError;
use fleetdeck_deck::link::desktop_authority::Waits;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use serde_json::json;
use share_support::*;

const SENTINEL: &str = "SENTINEL-TEXT-4711";

#[tokio::test]
async fn a_capture_is_sent_once_indexed_with_its_provenance_and_cached_as_a_capture() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c, h) = setup_file(dir.path()).await;
    let st = f.station.0.clone();
    let (r, req) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        capture_ok(ID, "A1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The wire: flat params, and the capture id is the authority action id.
    assert_eq!(req.params["action"], "screen_capture");
    assert_eq!(req.params["monitor"], "DP-1");
    let a = authority(&req);
    assert_eq!(a.action_id, ID);
    let node = f.station_node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenCapture {
        monitor: "DP-1".to_owned(),
    };
    assert_eq!(a.action_hash, action_hash(&action, &node));
    // The answer: no address, the capture with the deck's station id.
    assert!(r.body["address"].is_null());
    assert!(r.body.get("info_fresh").is_none());
    assert_eq!(r.body["capture"]["capture_id"], ID);
    let artifact = &r.body["capture"]["published"]["artifact"];
    assert_eq!(artifact["station_id"], st);
    let provenance = &r.body["capture"]["published"]["version"]["capture"];
    assert_eq!(provenance["station_id"], st);
    assert_eq!(provenance["monitor"], "DP-1");
    assert_eq!(provenance["capture_id"], ID);
    assert_eq!(provenance["kind"], "screenshot");
    assert_eq!(provenance["caller"], "owner");
    assert!(provenance["caller_station"].is_null());
    assert_eq!(provenance["at"], 1_000);
    // The info of the answer carries the negotiated P5a and P5b features.
    assert_eq!(
        r.body["info"]["features"],
        json!([
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::DESKTOP_RECORDING,
            feature::ARTIFACTS_VIDEO
        ])
    );
    // The artifact list shows it, and the version read has the provenance.
    let list = f.store.artifacts(None, None, None).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].artifact_id.as_str(), list[0].station_id.as_str()),
        ("A1", st.as_str())
    );
    let versions = f
        .store
        .artifact_versions(f.station.clone(), "A1".to_owned())
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].capture.as_ref().unwrap().station_id, st);
    let row = row(&f, ID).await.unwrap();
    assert_eq!((row.state, row.indexed), (CaptureState::Committed, true));
    // The file of a capture version is a capture cache row.
    f.store
        .put_artifact_file(
            f.station.clone(),
            "A1".to_owned(),
            1,
            "capture.png".to_owned(),
            vec![1, 2, 3],
        )
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("deck.db")).unwrap();
    let flag: i64 = conn
        .query_row(
            "SELECT capture FROM artifact_cache WHERE artifact_id = 'A1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flag, 1);
    // The audit row names the capture and the result.
    assert_eq!(
        c.audited("desktop.screen_capture", AuditResult::Ok).await,
        vec![Some(format!("capture={ID} code=ok"))]
    );
    // No second action, and nothing is sent for the id again.
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_capture_id_header_is_required_and_must_be_a_canonical_ulid() {
    let (f, mut station, c, h) = setup_default().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let body = cap_body("DP-1");
    let r = capture_with(&c, &h, &st, None, body.clone()).await;
    expect(&r, 400, "capture_id_required");
    assert_eq!(r.body["message"], "A screenshot needs a capture ID.");
    for bad in [
        "",
        "abc",
        "01arz3ndektsv4rrffq69g5fav",
        "81ARZ3NDEKTSV4RRFFQ69G5FAV",
        "01ARZ3NDEKTSV4RRFFQ69G5FAI",
    ] {
        let r = capture_with(&c, &h, &st, Some(bad), body.clone()).await;
        expect(&r, 400, "invalid_capture_id");
        assert_eq!(r.body["message"], "The capture ID is not valid.");
    }
    // The header twice.
    let origin = tailnet_origin(&c);
    let mut headers = refs(&h);
    headers.push(("Origin", origin.as_str()));
    headers.push(("X-FleetDeck-Capture-Id", ID));
    headers.push(("X-FleetDeck-Capture-Id", ID));
    expect(
        &c.post(&act_path(&st), &headers, body).await,
        400,
        "invalid_capture_id",
    );
    quiet(&mut station).await;
    assert!(row(&f, ID).await.is_none());
    let rows = c.audited("desktop.act", AuditResult::Refused).await;
    assert_eq!(rows.len(), 7);
    let of = |code: &str| rows.iter().filter(|r| r.as_deref() == Some(code)).count();
    assert_eq!(of("capture_id_required"), 1);
    assert_eq!(of("invalid_capture_id"), 6);
    // The header is ignored for the other actions.
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "screen_lock" }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn a_token_and_a_member_cannot_take_a_screenshot_and_the_refusal_is_audited() {
    let (f, mut station, c, h) = setup_default().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .post(
            &act_path(&st),
            &[
                ("Authorization", auth.as_str()),
                ("X-FleetDeck-Capture-Id", ID),
            ],
            cap_body("DP-1"),
        )
        .await;
    expect(&r, 403, "owner_only");
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    let mut headers = refs(&member);
    headers.push(("X-FleetDeck-Capture-Id", ID));
    let r = c.post(&act_path(&st), &headers, cap_body("DP-1")).await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
    assert!(row(&f, ID).await.is_none());
    assert!(
        !c.audited("desktop.act", AuditResult::Refused)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_station_without_the_feature_or_the_switch_is_refused_before_a_record() {
    let (f, mut station, c, h) = setup_default().await;
    enable(&c, true);
    let st = f.station.0.clone();
    // Another live link that did not negotiate `desktop_capture.v1`.
    f.store.set_link_features(
        &f.station,
        98,
        vec![
            feature::DESKTOP.to_owned(),
            feature::DESKTOP_AUTHORITY.to_owned(),
        ],
    );
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    expect(&r, 409, "desktop_unsupported");
    f.store.clear_link_features(&f.station, 98);
    quiet(&mut station).await;
    assert!(row(&f, ID).await.is_none());
    // The switch off.
    enable(&c, false);
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    expect(&r, 409, "desktop_control_off");
    enable(&c, true);
    // A monitor outside the grammar stops before the record.
    for monitor in ["DP 1;rm", "", "x/y"] {
        let r = capture_with(&c, &h, &st, Some(ID), cap_body(monitor)).await;
        expect(&r, 400, "invalid_body");
    }
    quiet(&mut station).await;
    assert!(row(&f, ID).await.is_none());
    // A station without the feature at all.
    let (f2, mut station2, c2, h2) = setup_without(&[feature::DESKTOP_CAPTURE]).await;
    let st2 = f2.station.0.clone();
    let r = capture_with(&c2, &h2, &st2, Some(ID), cap_body("DP-1")).await;
    expect(&r, 409, "desktop_unsupported");
    quiet(&mut station2).await;
    assert!(row(&f2, ID).await.is_none());
}

#[tokio::test]
async fn a_repeated_post_sends_nothing_and_a_changed_monitor_is_a_conflict() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        capture_ok(ID, "A1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The same id and body: the station is asked for `desktop.info` only.
    let again = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1"));
    let answer = async {
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "desktop.info");
        assert_eq!(req.params, json!({ "fresh": true }));
        station
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
    };
    let (second, ()) = tokio::join!(again, answer);
    assert_eq!(second.status, 200, "{}", second.body);
    assert_eq!(second.body["capture"], r.body["capture"]);
    assert!(second.body.get("info_fresh").is_none());
    quiet(&mut station).await;
    // Another monitor with the same id.
    let r = capture_with(&c, &h, &st, Some(ID), cap_body("DP-2")).await;
    expect(&r, 409, "capture_id_conflict");
    assert_eq!(r.body["message"], text::CAPTURE_ID_CONFLICT);
    quiet(&mut station).await;
    // The station could not answer the info in time: the stored info and
    // `info_fresh: false`.
    let cached: fleetdeck_proto::link::StationInfo = serde_json::from_value(json!({
        "name": "fake", "incarnation": "inc", "head": 7, "live_sessions": 2
    }))
    .unwrap();
    f.store.set_station_info(&f.station, cached);
    f.store.update_station_desktop(&f.station, info());
    let opts = CaptureOptions {
        waits: Waits {
            long: Duration::from_millis(300),
            short: Duration::from_millis(300),
        },
        ..CaptureOptions::default()
    };
    let ctx = relay_ctx(&f, &c);
    let third = desktop_capture::act(&ctx, f.station.clone(), ID.into(), "DP-1".into(), opts)
        .await
        .unwrap();
    let Reply::Done(done) = third else {
        panic!("a result");
    };
    assert_eq!(done.info_fresh, Some(false));
    assert_eq!(done.capture.unwrap().capture_id, ID);
    // Three audit rows: the send and two answers of the row; one conflict.
    let ok_rows = c.audited("desktop.screen_capture", AuditResult::Ok).await;
    assert_eq!(ok_rows.len(), 3);
    assert_eq!(
        c.audited("desktop.screen_capture", AuditResult::Refused)
            .await,
        vec![Some(format!("capture={ID} code=capture_id_conflict"))]
    );
}

#[tokio::test]
async fn every_station_capture_code_keeps_its_status_and_text_and_is_recorded() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let cases = [
        (code::BUSY, 409, text::BUSY),
        (code::DISK_FULL, 507, text::DISK_FULL),
        (code::CAPTURE_QUOTA, 409, text::CAPTURE_QUOTA),
        (code::CAPTURE_TOO_LARGE, 413, text::CAPTURE_TOO_LARGE),
        (code::CAPTURE_DIMENSIONS, 422, text::CAPTURE_DIMENSIONS),
        (code::CAPTURE_FAILED, 502, text::CAPTURE_FAILED),
        (
            code::CAPTURE_PUBLISH_FAILED,
            502,
            text::CAPTURE_PUBLISH_FAILED,
        ),
        (code::SCREEN_LOCKED, 409, text::SCREEN_LOCKED),
        (code::CAPTURE_ID_CONFLICT, 409, text::CAPTURE_ID_CONFLICT),
        (code::CAPTURE_INTERRUPTED, 502, text::CAPTURE_INTERRUPTED),
    ];
    for (n, (station_code, status, fixed)) in (1u32..).zip(cases) {
        let id = nth_id(n);
        let message = format!("{SENTINEL} /home/owner/secret");
        let (r, _) = capture_exchange(
            &c,
            &h,
            &mut station,
            &st,
            &id,
            cap_body("DP-1"),
            err(station_code, &message),
        )
        .await;
        expect(&r, status, station_code);
        assert_eq!(r.body["message"], fixed, "{station_code}");
        assert!(!r.text.contains(SENTINEL), "{}", r.text);
        let stored = row(&f, &id).await.unwrap();
        assert_eq!(stored.state, CaptureState::Failed, "{station_code}");
        assert_eq!(stored.code.as_deref(), Some(station_code));
        assert_eq!(stored.message.as_deref(), Some(fixed));
        // The same id again: the same failure, nothing sent.
        let again = capture_with(&c, &h, &st, Some(&id), cap_body("DP-1")).await;
        expect(&again, status, station_code);
        // The lookup answers the stored failure.
        let looked = get(&c, &h, &lookup_path(&st, &id)).await;
        assert_eq!(looked.status, 200, "{}", looked.body);
        assert_eq!(looked.body["state"], "failed");
        assert_eq!(looked.body["error"], station_code);
        assert_eq!(looked.body["message"], fixed);
        quiet(&mut station).await;
    }
}

#[tokio::test]
async fn the_missing_grim_is_a_desktop_unsupported_with_its_own_text() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let (r, _) = capture_exchange(
        &c,
        &h,
        &mut station,
        &st,
        ID,
        cap_body("DP-1"),
        err("unsupported", text::NO_GRIM),
    )
    .await;
    expect(&r, 409, "desktop_unsupported");
    assert_eq!(r.body["message"], text::NO_GRIM);
    let stored = row(&f, ID).await.unwrap();
    assert_eq!(stored.state, CaptureState::Failed);
    assert_eq!(stored.code.as_deref(), Some("unsupported"));
    let again = capture_with(&c, &h, &st, Some(ID), cap_body("DP-1")).await;
    expect(&again, 409, "desktop_unsupported");
    assert_eq!(again.body["message"], text::NO_GRIM);
    quiet(&mut station).await;
}

#[tokio::test]
async fn another_station_error_keeps_the_existing_mapping_and_the_row_attempted() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    let cases = [
        ("forbidden", "x", 403, "desktop_forbidden"),
        ("not_found", "x", 404, "desktop_not_found"),
        ("unsupported", "another text", 409, "desktop_unsupported"),
        ("too_many_requests", "x", 503, "desktop_overloaded"),
        ("something_new", "x", 502, "desktop_failed"),
        (
            "failed",
            "The desktop action did not finish in time.",
            504,
            "desktop_timeout",
        ),
    ];
    for (n, (station_code, message, status, want)) in (1u32..).zip(cases) {
        let id = nth_id(n);
        let (r, _) = capture_exchange(
            &c,
            &h,
            &mut station,
            &st,
            &id,
            cap_body("DP-1"),
            err(station_code, message),
        )
        .await;
        expect(&r, status, want);
        assert_eq!(row(&f, &id).await.unwrap().state, CaptureState::Attempted);
        // The same id answers `pending`: the deck never sends it again.
        let again = capture_with(&c, &h, &st, Some(&id), cap_body("DP-1")).await;
        assert_eq!(again.status, 202, "{}", again.body);
        assert_eq!(again.body, json!({ "capture_id": id, "state": "pending" }));
        quiet(&mut station).await;
    }
}

#[tokio::test]
async fn an_answer_without_the_capture_is_a_capture_failure() {
    let (f, mut station, c, h) = setup_default().await;
    let st = f.station.0.clone();
    for (n, reply) in [(1, ok(None)), (2, capture_ok(&nth_id(99), "A1"))] {
        let id = nth_id(n);
        let (r, _) =
            capture_exchange(&c, &h, &mut station, &st, &id, cap_body("DP-1"), reply).await;
        expect(&r, 502, "capture_failed");
        assert_eq!(r.body["message"], text::CAPTURE_FAILED);
        let stored = row(&f, &id).await.unwrap();
        assert_eq!(stored.state, CaptureState::Failed);
        assert_eq!(stored.code.as_deref(), Some(code::CAPTURE_FAILED));
    }
    assert!(
        f.store
            .artifacts(None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_station_timeout_is_a_504_and_the_row_stays_attempted() {
    let (f, mut station, c, _h) = setup_default().await;
    let opts = CaptureOptions {
        waits: Waits {
            long: Duration::from_millis(200),
            short: Duration::from_millis(200),
        },
        ..CaptureOptions::default()
    };
    let ctx = relay_ctx(&f, &c);
    let ask = desktop_capture::act(&ctx, f.station.clone(), ID.into(), "DP-1".into(), opts);
    let seen = async {
        // The station gets the request and never answers.
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "desktop.act");
        req
    };
    let (result, req) = tokio::join!(ask, seen);
    assert_eq!(authority(&req).action_id, ID);
    let e = result.unwrap_err();
    assert_eq!(e, CaptureError::Relay(RelayError::Timeout));
    assert_eq!(e.http().1, "desktop_timeout");
    assert_eq!(row(&f, ID).await.unwrap().state, CaptureState::Attempted);
    // Another call of the same id sends nothing.
    let again = desktop_capture::act(&ctx, f.station.clone(), ID.into(), "DP-1".into(), opts)
        .await
        .unwrap();
    assert!(matches!(again, Reply::State(s, _) if s.as_u16() == 202));
    quiet(&mut station).await;
    assert_eq!(
        c.audited("desktop.screen_capture", AuditResult::Failed)
            .await,
        vec![Some(format!("capture={ID} code=desktop_timeout"))]
    );
}
