//! The owner recording over loopback HTTPS (R13 P5b-1 task K1): the
//! `screen_record` branch of `POST .../desktop/act`, the stop, the active
//! discovery, the status, the continuation lease and the recording provenance,
//! with a fake station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod common;
mod desktop_support;
#[path = "board_desktop_recording/lease.rs"]
mod lease;
mod share_support;
#[path = "board_desktop_recording/stop_active.rs"]
mod stop_active;
#[path = "board_desktop_recording/support.rs"]
mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use capture_support::*;
use common::next_request;
use desktop_support::*;
use fleetdeck_deck::desktop_capture::recording::{OperationKind, RecordOptions, StopIntent};
use fleetdeck_deck::desktop_capture::{self, Reply};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::capture::{CAPTURE_ID_HEADER, CaptureCaller, CaptureKind};
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::recording::StopReason;
use fleetdeck_proto::version::feature;
use serde_json::json;
use share_support::*;
use support::*;

#[tokio::test]
async fn a_recording_is_sent_once_indexed_with_its_recording_provenance_and_audited() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c, h) = rec_setup_file(dir.path()).await;
    let st = f.station.0.clone();
    let post = start_post(&c, &h, &st, Some(ID), rec_body(30), true);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(req.id, rec_ok(ID, "V1", Some(meta(30))))
            .await
            .unwrap();
        req
    };
    let (r, req) = tokio::join!(post, answer);
    assert_eq!(r.status, 200, "{}", r.body);
    // The wire: flat params, and the capture id is the authority action id.
    assert_eq!(req.method, "desktop.act");
    assert_eq!(req.params["action"], "screen_record");
    assert_eq!(req.params["monitor"], "DP-1");
    assert_eq!(req.params["seconds"], 30);
    let a = authority(&req);
    assert_eq!(a.action_id, ID);
    let node = f.station_node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenRecord {
        monitor: "DP-1".to_owned(),
        seconds: 30,
    };
    assert_eq!(a.action_hash, action_hash(&action, &node));
    // The answer: no address, the capture with the deck's station id and the
    // deck's recording provenance.
    assert!(r.body["address"].is_null());
    assert_eq!(r.body["capture"]["capture_id"], ID);
    let published = &r.body["capture"]["published"];
    assert_eq!(published["artifact"]["station_id"], st);
    assert_eq!(published["artifact"]["kind"], "video");
    let p = &published["version"]["capture"];
    assert_eq!(
        (p["kind"].as_str(), p["caller"].as_str()),
        (Some("recording"), Some("owner"))
    );
    assert_eq!(p["station_id"], st);
    assert!(p["caller_station"].is_null());
    assert_eq!(p["recording"]["requested_seconds"], 30);
    assert_eq!(p["recording"]["stop_reason"], "duration");
    assert_eq!(
        r.body["info"]["features"],
        json!([
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::DESKTOP_RECORDING,
            feature::ARTIFACTS_VIDEO
        ])
    );
    // The row, the artifact list and the version.
    let row = rec_row(&f, ID).await.unwrap();
    assert_eq!(row.kind, OperationKind::Recording);
    assert_eq!(row.requested_seconds, Some(30));
    assert!(row.owner_login.as_deref().is_some_and(|l| l.len() == 64));
    assert_eq!(row.link_epoch, station.control.params().local_desktop_epoch);
    assert_eq!((row.stop_intent, row.started_at), (StopIntent::None, None));
    assert_eq!(
        (row.capture.state, row.capture.indexed),
        (CaptureState::Committed, true)
    );
    assert!(row.audited_terminal);
    let list = f.store.artifacts(None, None, None).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0].artifact_id.as_str(), list[0].station_id.as_str()),
        ("V1", st.as_str())
    );
    let versions = f
        .store
        .artifact_versions(f.station.clone(), "V1".to_owned())
        .await
        .unwrap();
    let stored = versions[0].capture.as_ref().unwrap();
    assert_eq!(
        (stored.kind, stored.caller),
        (CaptureKind::Recording, CaptureCaller::Owner)
    );
    assert_eq!(stored.recording, Some(meta(30)));
    // The audit: accepted before the send, one terminal row, no path.
    let rows = c.audited("desktop.capture", AuditResult::Ok).await;
    assert_eq!(
        rows,
        vec![
            Some(format!(
                "capture={ID} kind=recording monitor=DP-1 seconds=30 stage=terminal code=ok \
                 stop_reason=duration artifact=V1"
            )),
            Some(format!(
                "capture={ID} kind=recording monitor=DP-1 seconds=30 stage=accepted code=ok"
            )),
        ]
    );
    assert!(!audit_text(&c).await.contains("capture.mp4"));
    quiet(&mut station).await;
    // The header-less answer is the legacy projection: `other`, no capture.
    let r2 = start_post(&c, &h, &st, Some(ID), rec_body(30), false);
    let info_answer = serve_one(
        &mut station,
        "desktop.info",
        serde_json::to_value(info()).unwrap(),
    );
    let (r2, _) = tokio::join!(r2, info_answer);
    assert_eq!(r2.status, 200, "{}", r2.body);
    assert_eq!(r2.body["capture"]["published"]["artifact"]["kind"], "other");
    assert!(
        r2.body["capture"]["published"]["version"]
            .get("capture")
            .is_none()
    );
    let stored = f
        .store
        .artifact_versions(f.station.clone(), "V1".to_owned())
        .await
        .unwrap();
    assert!(stored[0].capture.is_some());
}

#[tokio::test]
async fn a_start_needs_the_header_the_seconds_the_features_and_the_switch() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let r = start_post(&c, &h, &st, None, rec_body(30), true).await;
    expect(&r, 400, "capture_id_required");
    assert_eq!(r.body["message"], "A recording needs a capture ID.");
    let r = start_post(&c, &h, &st, Some("abc"), rec_body(30), true).await;
    expect(&r, 400, "invalid_capture_id");
    for bad in [
        json!(0),
        json!(3601),
        json!(1.5),
        json!(-1),
        json!(30.0),
        json!(1e3),
    ] {
        let body = json!({ "action": "screen_record", "monitor": "DP-1", "seconds": bad });
        let r = start_post(&c, &h, &st, Some(ID), body, true).await;
        expect(&r, 400, "recording_duration");
        assert_eq!(
            r.body["message"],
            "The recording length must be from 1 to 3600 seconds."
        );
    }
    for body in [
        json!({ "action": "screen_record", "monitor": "DP-1", "seconds": "30" }),
        json!({ "action": "screen_record", "monitor": "DP-1" }),
        json!({ "action": "screen_record", "monitor": "DP-1", "seconds": 5, "x": 1 }),
    ] {
        expect(
            &start_post(&c, &h, &st, Some(ID), body, true).await,
            400,
            "invalid_body",
        );
    }
    let bad_monitor = json!({ "action": "screen_record", "monitor": "x/y", "seconds": 5 });
    expect(
        &start_post(&c, &h, &st, Some(ID), bad_monitor, true).await,
        400,
        "invalid_body",
    );
    // The switch off, and a token.
    enable(&c, false);
    let r = start_post(&c, &h, &st, Some(ID), rec_body(30), true).await;
    expect(&r, 409, "desktop_control_off");
    enable(&c, true);
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
    let r = c
        .post(
            &act_path(&st),
            &[("Authorization", auth.as_str()), (CAPTURE_ID_HEADER, ID)],
            rec_body(30),
        )
        .await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
    assert!(rec_row(&f, ID).await.is_none());
    assert!(
        !c.audited("desktop.act", AuditResult::Refused)
            .await
            .is_empty()
    );
    // The switch off and the monitor outside the grammar stop before the row.
    assert_eq!(
        c.audited("desktop.capture", AuditResult::Refused)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn a_station_without_a_feature_of_the_owner_set_gets_a_501_and_no_row() {
    for missing in [
        feature::DESKTOP_RECORDING,
        feature::ARTIFACTS_VIDEO,
        feature::ARTIFACTS_DELETE,
        feature::DESKTOP_CAPTURE,
    ] {
        let (f, mut station, c, h) = setup_without(&[missing]).await;
        let st = f.station.0.clone();
        let r = start_post(&c, &h, &st, Some(ID), rec_body(30), true).await;
        expect(&r, 501, "desktop_unsupported");
        assert_eq!(
            r.body["message"],
            "This station does not support monitor recordings."
        );
        quiet(&mut station).await;
        assert!(rec_row(&f, ID).await.is_none(), "{missing}");
    }
}

#[tokio::test]
async fn a_deck_timeout_answers_unknown_and_never_sends_again() {
    let (f, mut station, c, h) = rec_setup().await;
    let ctx = relay_ctx(&f, &c);
    let opts = RecordOptions {
        second: Duration::from_millis(1),
        wait_extra: Duration::from_millis(200),
        ..RecordOptions::default()
    };
    let ask = desktop_capture::recording::start(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        1,
        "login".into(),
        opts,
    );
    let seen = async {
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "desktop.act");
    };
    let (result, ()) = tokio::join!(ask, seen);
    assert!(matches!(
        result.unwrap(),
        Reply::State(s, b) if s.as_u16() == 202 && format!("{b:?}").contains("Unknown")
    ));
    assert_eq!(
        rec_row(&f, ID).await.unwrap().capture.state,
        CaptureState::Attempted
    );
    let again = desktop_capture::recording::start(
        &ctx,
        f.station.clone(),
        ID.into(),
        "DP-1".into(),
        1,
        "login".into(),
        opts,
    )
    .await
    .unwrap();
    assert!(matches!(again, Reply::State(s, _) if s.as_u16() == 202));
    quiet(&mut station).await;
    // The recovery learns the station failure: its fixed text, one terminal row.
    let row = rec_row(&f, ID).await.unwrap();
    let failed = fleetdeck_proto::capture::CaptureStatus::Failed {
        capture_id: ID.into(),
        error: "recording_too_large".into(),
        message: SENTINEL.into(),
    };
    for _ in 0..2 {
        desktop_capture::reconcile(&f.store, &row.capture, failed.clone())
            .await
            .unwrap();
    }
    let r = get_with(&c, &h, &lookup_path(&f.station.0, ID), true).await;
    assert_eq!(r.body["state"], "failed");
    assert_eq!(r.body["error"], "recording_too_large");
    assert_eq!(r.body["message"], "The recording exceeds 256 MiB.");
    assert_eq!(
        c.audited("desktop.capture", AuditResult::Failed)
            .await
            .len(),
        1
    );
    assert!(!audit_text(&c).await.contains(SENTINEL));
}

#[tokio::test]
async fn a_station_failure_is_stored_with_its_fixed_text_and_a_station_text_never_leaks() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let cases = [
        ("busy", 409),
        ("recording_too_large", 413),
        ("recording_dimensions", 422),
        ("recording_failed", 502),
        ("recording_cancelled", 409),
        ("desktop_timeout", 504),
        ("screen_locked", 409),
        ("disk_full", 507),
    ];
    for (n, (code, status)) in cases.into_iter().enumerate() {
        let id = nth_id(u32::try_from(n).unwrap() + 20);
        let post = start_post(&c, &h, &st, Some(&id), rec_body(5), true);
        let answer = async {
            let req = next_request(&mut station).await;
            station
                .control
                .respond(req.id, err(code, SENTINEL))
                .await
                .unwrap();
        };
        let (r, ()) = tokio::join!(post, answer);
        expect(&r, status, code);
        assert!(!r.text.contains(SENTINEL));
        let row = rec_row(&f, &id).await.unwrap();
        assert_eq!(row.capture.state, CaptureState::Failed);
        assert_eq!(row.capture.code.as_deref(), Some(code));
        assert!(!row.capture.message.unwrap().contains(SENTINEL));
        assert!(row.audited_terminal);
    }
    // The missing tool and any other unsupported answer.
    for (n, (msg, want)) in [
        (
            "The recording tool wf-recorder is not installed.",
            "The recording tool wf-recorder is not installed.",
        ),
        (
            SENTINEL,
            "This station does not support monitor recordings.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = nth_id(u32::try_from(n).unwrap() + 40);
        let post = start_post(&c, &h, &st, Some(&id), rec_body(5), true);
        let answer = async {
            let req = next_request(&mut station).await;
            station
                .control
                .respond(req.id, err("unsupported", msg))
                .await
                .unwrap();
        };
        let (r, ()) = tokio::join!(post, answer);
        expect(&r, 501, "desktop_unsupported");
        assert_eq!(r.body["message"], want);
    }
    let text = audit_text(&c).await;
    assert!(!text.contains(SENTINEL) && !text.contains("capture.mp4"));
    assert_eq!(
        c.audited("desktop.capture", AuditResult::Failed)
            .await
            .len(),
        10
    );
}

#[tokio::test]
async fn a_station_commit_with_unacceptable_metadata_is_a_202_pending_never_a_failure() {
    let (f, mut station, c, h) = rec_setup().await;
    let st = f.station.0.clone();
    let mut other_seconds = meta(30);
    other_seconds.requested_seconds = 29;
    let mut too_long = meta(30);
    too_long.duration_ms = 31_001;
    let mut bad_reason = meta(30);
    bad_reason.stop_reason = StopReason::PolicyOff;
    let mut bad_size = meta(30);
    bad_size.width = 1921;
    for (n, bad) in [
        Some(other_seconds),
        Some(too_long),
        Some(bad_reason),
        Some(bad_size),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let id = nth_id(u32::try_from(n).unwrap() + 60);
        let artifact = format!("B{n}");
        let post = start_post(&c, &h, &st, Some(&id), rec_body(30), true);
        let answer = async {
            let req = next_request(&mut station).await;
            station
                .control
                .respond(req.id, rec_ok(&id, &artifact, bad))
                .await
                .unwrap();
        };
        let (r, ()) = tokio::join!(post, answer);
        assert_eq!(r.status, 202, "{n} {}", r.body);
        assert_eq!(r.body, state("pending", &id));
        let row = rec_row(&f, &id).await.unwrap();
        assert_eq!(
            (row.capture.state, row.capture.indexed),
            (CaptureState::Committed, false)
        );
        assert!(row.capture.published.is_some());
    }
    // Nothing is indexed, and nothing is an owner screenshot.
    assert!(
        f.store
            .artifacts(None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        c.audited("desktop.capture", AuditResult::Failed)
            .await
            .len(),
        5
    );
    // The recovery: a station status with good metadata indexes it.
    let id = nth_id(60);
    let row = rec_row(&f, &id).await.unwrap();
    let good = rec_published("B0", &id, Some(meta(30)));
    let status = fleetdeck_proto::capture::CaptureStatus::Committed {
        capture_id: id.clone(),
        published: good,
    };
    desktop_capture::reconcile(&f.store, &row.capture, status.clone())
        .await
        .unwrap();
    desktop_capture::reconcile(&f.store, &row.capture, status)
        .await
        .unwrap();
    let row = rec_row(&f, &id).await.unwrap();
    assert!(row.capture.indexed && row.audited_terminal);
    assert_eq!(
        c.audited("desktop.capture", AuditResult::Ok)
            .await
            .iter()
            .filter(|d| { d.as_deref().is_some_and(|d| d.contains("stage=terminal")) })
            .count(),
        1
    );
}
