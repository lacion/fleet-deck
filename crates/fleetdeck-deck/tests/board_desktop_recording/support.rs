//! The helpers of the owner recording tests (R13 P5b-1 task K1): the station
//! answers of a recording, the owner requests, a start that waits for the
//! station while a scenario runs, and the lease requests of the station.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use crate::capture_support::*;
use crate::common::board::{BoardClient, Http};
use crate::common::{Fixture, fixture, fixture_file, next_request};
use crate::desktop_support::*;
use crate::share_support::*;
use fleetdeck_deck::desktop_capture::recording::RecordingRow;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::{CAPTURE_ID_HEADER, CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::recording::{
    CaptureLeaseAnswer, CaptureLeaseRequest, LeaseDecision, LeasePhase, RecordingMeta, StopReason,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const SENTINEL: &str = "SENTINEL-TEXT-4711";

pub fn meta(seconds: u32) -> RecordingMeta {
    RecordingMeta {
        requested_seconds: seconds,
        duration_ms: u64::from(seconds) * 1_000,
        width: 1920,
        height: 1080,
        stop_reason: StopReason::Duration,
    }
}

/// The `published` of a recording as the station sends it.
pub fn rec_published(
    artifact_id: &str,
    capture_id: &str,
    m: Option<RecordingMeta>,
) -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: artifact_id.to_owned(),
            station_id: "station-side-name".to_owned(),
            session_id: None,
            title: "Desktop recording · fake/DP-1".to_owned(),
            kind: ArtifactKind::Video,
            latest_version: 1,
            created_at: 1_000,
            updated_at: 1_000,
            author: "owner".to_owned(),
        },
        version: ArtifactVersion {
            artifact_id: artifact_id.to_owned(),
            version: 1,
            size: 3,
            sha256: "ab".repeat(32),
            files: vec!["capture.mp4".to_owned()],
            created_at: 1_000,
            capture: Some(CaptureProvenance {
                capture_id: capture_id.to_owned(),
                station_id: "station-side-name".to_owned(),
                monitor: "DP-1".to_owned(),
                kind: CaptureKind::Recording,
                caller: CaptureCaller::Owner,
                caller_station: None,
                caller_session: None,
                caller_grant_id: None,
                at: 1_000,
                recording: m,
            }),
        },
        created: true,
    }
}

pub fn rec_ok(capture_id: &str, artifact_id: &str, m: Option<RecordingMeta>) -> Outcome {
    let published = rec_published(artifact_id, capture_id, m);
    Outcome::Ok(json!({
        "address": null,
        "info": info(),
        "capture": { "capture_id": capture_id, "published": published },
    }))
}

pub fn rec_body(seconds: u32) -> Value {
    json!({ "action": "screen_record", "monitor": "DP-1", "seconds": seconds })
}

pub fn headers(h: &Headers, c: &BoardClient, id: Option<&str>, video: bool) -> Headers {
    let mut v = h.clone();
    v.push(("Origin".to_owned(), tailnet_origin(c)));
    if let Some(id) = id {
        v.push((CAPTURE_ID_HEADER.to_owned(), id.to_owned()));
    }
    if video {
        v.push((
            "X-FleetDeck-Features".to_owned(),
            "artifacts_video.v1".to_owned(),
        ));
    }
    v
}

pub async fn start_post(
    c: &BoardClient,
    h: &Headers,
    st: &str,
    id: Option<&str>,
    body: Value,
    video: bool,
) -> Http {
    c.post(&act_path(st), &refs(&headers(h, c, id, video)), body)
        .await
}

/// `POST .../stop` as the owner with the body `{}`.
pub async fn stop_post(c: &BoardClient, h: &Headers, st: &str, id: &str, video: bool) -> Http {
    let hs = headers(h, c, None, video);
    c.post(&stop_path(st, id), &refs(&hs), json!({})).await
}

/// `GET path` as the owner.
pub async fn get_with(c: &BoardClient, h: &Headers, path: &str, video: bool) -> Http {
    let hs = headers(h, c, None, video);
    c.get(path, &refs(&hs)).await
}

pub fn stop_path(st: &str, id: &str) -> String {
    format!("/api/stations/{st}/desktop/captures/{id}/stop")
}

pub fn active_path(st: &str) -> String {
    format!("/api/stations/{st}/desktop/recording")
}

/// The deck, a linked fake station and the owner. The pairing of a real
/// station grants `admin` only, and the lease row needs `read`: the fake
/// station gets both, so that the lease can be tested.
pub async fn rec_setup_with(f: Fixture) -> (Fixture, LiveLink, BoardClient, Headers) {
    f.store
        .pin_station(NewStation {
            key: f.station_node.identity.public_key(),
            name: "fake".to_owned(),
            scopes: BTreeSet::from([Scope::Admin, Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    f.ctx.peers.refresh(f.station.clone()).await.unwrap();
    let (f, station, c, h) = setup_with(f).await;
    assert!(f.ctx.desktop_live.set(c.state.live.clone()).is_ok());
    (f, station, c, h)
}

pub async fn rec_setup() -> (Fixture, LiveLink, BoardClient, Headers) {
    rec_setup_with(fixture().await).await
}

pub async fn rec_setup_file(dir: &std::path::Path) -> (Fixture, LiveLink, BoardClient, Headers) {
    rec_setup_with(fixture_file(dir).await).await
}

pub async fn rec_row(f: &Fixture, id: &str) -> Option<RecordingRow> {
    f.store
        .recording_get(f.station.clone(), id.to_owned())
        .await
        .unwrap()
}

/// Answers the next request of the station side with `reply`.
pub async fn serve_one(station: &mut LiveLink, method: &str, reply: Value) -> InboundRequest {
    let req = next_request(station).await;
    assert_eq!(req.method, method);
    station
        .control
        .respond(req.id, Outcome::Ok(reply))
        .await
        .unwrap();
    req
}

/// Runs `scenario` while the start of `id` (30 seconds) waits for the answer
/// `reply` of the station, which the scenario can give leases and stops
/// before. Returns the answer of the start.
pub async fn during<R>(
    (c, h, st): (&BoardClient, &Headers, &str),
    station: &mut LiveLink,
    id: &str,
    reply: Outcome,
    scenario: impl AsyncFnOnce(&mut LiveLink, &InboundRequest) -> R,
) -> (Http, R) {
    let post = start_post(c, h, st, Some(id), rec_body(30), true);
    let run = async {
        let act = next_request(station).await;
        assert_eq!(act.method, "desktop.act");
        let out = scenario(station, &act).await;
        station.control.respond(act.id, reply).await.unwrap();
        out
    };
    tokio::join!(post, run)
}

/// The answer of the station to `desktop.capture_policy_now`: `ok` allows
/// everything; a flag name turns that flag off (`desktop_epoch` is another
/// epoch); `error` is a wire error; `junk` is not a policy answer.
pub fn policy_reply(station: &LiveLink, how: &str) -> Outcome {
    let mut p = json!({
        "valid": true,
        "desktop_epoch": station.control.params().local_desktop_epoch.clone().unwrap(),
        "allow_desktop": true, "allow_desktop_capture": true,
        "monitor_allowed": true, "monitor_present": true, "unlocked": true,
    });
    match how {
        "ok" => {}
        "error" => return err("failed", "x"),
        "junk" => return Outcome::Ok(json!({})),
        "desktop_epoch" => p["desktop_epoch"] = json!("other"),
        flag => p[flag] = json!(false),
    }
    Outcome::Ok(p)
}

pub fn pending_status(id: &str, phase: &str) -> Value {
    json!({ "state": "pending", "capture_id": id,
            "recording": { "phase": phase, "requested_seconds": 30,
                           "started_at": 1_000, "elapsed_ms": 2_000 } })
}

/// One `desktop.capture_lease` of the station for the start `act`. `asked` is
/// how the station answers the `desktop.capture_policy_now` that the deck asks
/// (see [`policy_reply`]); `None`: none is asked.
pub async fn lease(
    station: &mut LiveLink,
    act: &InboundRequest,
    phase: LeasePhase,
    asked: Option<&str>,
) -> Outcome {
    let a = authority(act);
    let req = CaptureLeaseRequest {
        capture_id: a.action_id,
        action_hash: a.action_hash,
        sequence: 7,
        phase,
    };
    lease_req(station, &req, asked).await
}

pub async fn lease_req(
    station: &mut LiveLink,
    req: &CaptureLeaseRequest,
    asked: Option<&str>,
) -> Outcome {
    let control = station.control.clone();
    let ask = async {
        control
            .request("desktop.capture_lease", None, json!(req))
            .await
            .unwrap()
    };
    let serve = async {
        if let Some(how) = asked {
            let got = next_request(station).await;
            assert_eq!(got.method, "desktop.capture_policy_now");
            assert_eq!(got.params["capture_id"], req.capture_id);
            assert_eq!(got.params["action_hash"], req.action_hash);
            let reply = policy_reply(station, how);
            station.control.respond(got.id, reply).await.unwrap();
        }
    };
    tokio::join!(ask, serve).0
}

pub fn answer_of(o: Outcome) -> (LeaseDecision, Option<StopReason>) {
    let Outcome::Ok(v) = o else {
        panic!("a lease answer, got {o:?}");
    };
    let a: CaptureLeaseAnswer = serde_json::from_value(v).unwrap();
    assert_eq!(a.sequence, 7);
    (a.decision, a.reason)
}

pub fn wire_code(o: &Outcome) -> &str {
    let Outcome::Err(e) = o else {
        panic!("a wire error, got {o:?}");
    };
    &e.code
}

pub fn owner_stop_meta() -> RecordingMeta {
    let mut m = meta(30);
    m.stop_reason = StopReason::OwnerStop;
    m.duration_ms = 12_000;
    m
}
