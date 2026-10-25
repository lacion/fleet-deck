//! The version projection of the `video` kind on the board (R13 P5b task K2,
//! design `omarchy-p5b.md` §11.1, §14): the artifact list, get and publish
//! answers and the board WebSocket events show kind `other` and no recording
//! provenance unless the request or the socket declares `artifacts_video.v1`.
//! The stored rows and events never change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::board::{BoardClient, Http, Ws, board};
use common::*;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{RecordingMeta, StopReason};
use fleetdeck_proto::{EventEnvelope, SessionSummary, StationId};
use serde_json::{Value, json};

const FEATURE: (&str, &str) = ("X-FleetDeck-Features", "artifacts_video.v1");

fn artifact(id: &str, kind: ArtifactKind) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: Some("s1".into()),
        title: format!("Title of {id}"),
        kind,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

fn version(id: &str, kind: CaptureKind) -> ArtifactVersion {
    let recording = kind == CaptureKind::Recording;
    ArtifactVersion {
        artifact_id: id.to_owned(),
        version: 1,
        size: 3,
        sha256: "ab".repeat(32),
        files: vec![
            if recording {
                "capture.mp4"
            } else {
                "capture.png"
            }
            .to_owned(),
        ],
        created_at: 1,
        capture: Some(CaptureProvenance {
            capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            station_id: "fake".to_owned(),
            monitor: "DP-1".to_owned(),
            kind,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 1,
            recording: recording.then_some(RecordingMeta {
                requested_seconds: 3,
                duration_ms: 3000,
                width: 2,
                height: 2,
                stop_reason: StopReason::Duration,
            }),
        }),
    }
}

/// A deck with one recording ("vid") and one screenshot ("pic") in its index,
/// and a logged-in owner. No station is linked.
async fn seeded() -> (Fixture, BoardClient, String, String) {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    for (id, kind, capture) in [
        ("vid", ArtifactKind::Video, CaptureKind::Recording),
        ("pic", ArtifactKind::Image, CaptureKind::Screenshot),
    ] {
        c.store
            .note_artifact(f.station.clone(), &artifact(id, kind))
            .await
            .unwrap();
        let v = version(id, capture);
        c.store
            .put_artifact_versions(f.station.clone(), vec![v.clone()])
            .await
            .unwrap();
        // Only the capture path writes the provenance column.
        let provenance = serde_json::to_string(&v.capture).unwrap();
        let id = id.to_owned();
        let set = c
            .store
            .db()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE artifact_versions SET capture = ?1 WHERE artifact_id = ?2",
                    [provenance, id],
                )?)
            })
            .await;
        assert_eq!(set.unwrap(), 1);
    }
    (f, c, cookie, csrf)
}

fn kinds(list: &Http) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["artifact_id"].as_str().unwrap().to_owned(),
                a["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn the_artifact_list_shows_other_for_a_video_unless_the_request_declares_the_feature() {
    let (_f, c, cookie, _) = seeded().await;
    let plain = [("Cookie", cookie.as_str())];
    let old = c.get("/api/artifacts", &plain).await;
    assert_eq!(old.status, 200, "{}", old.text);
    assert_eq!(
        kinds(&old),
        [
            ("pic".to_owned(), "image".to_owned()),
            ("vid".to_owned(), "other".to_owned())
        ]
    );
    // A header with other tokens only is still a legacy request.
    let other = [
        ("Cookie", cookie.as_str()),
        (FEATURE.0, "desktop_capture.v1"),
    ];
    let r = c.get("/api/artifacts", &other).await;
    assert_eq!(kinds(&r)[1].1, "other");
    // Comma and space lists with the token declare the feature.
    for list in [
        "artifacts_video.v1",
        "a.v1, artifacts_video.v1",
        "a.v1 artifacts_video.v1",
    ] {
        let h = [("Cookie", cookie.as_str()), (FEATURE.0, list)];
        let r = c.get("/api/artifacts", &h).await;
        assert_eq!(
            kinds(&r),
            [
                ("pic".to_owned(), "image".to_owned()),
                ("vid".to_owned(), "video".to_owned())
            ],
            "{list}"
        );
    }
}

#[tokio::test]
async fn the_artifact_get_drops_the_recording_provenance_unless_the_request_declares_the_feature() {
    let (_f, c, cookie, _) = seeded().await;
    let plain = [("Cookie", cookie.as_str())];
    let aware = [("Cookie", cookie.as_str()), FEATURE];

    let old = c.get("/api/artifacts/fake/vid", &plain).await;
    assert_eq!(old.status, 200, "{}", old.text);
    assert_eq!(old.body["artifact"]["kind"], "other");
    assert_eq!(old.body["versions"][0]["capture"], Value::Null);
    assert_eq!(old.body["versions"][0]["files"], json!(["capture.mp4"]));

    let new = c.get("/api/artifacts/fake/vid", &aware).await;
    assert_eq!(new.body["artifact"]["kind"], "video");
    assert_eq!(new.body["versions"][0]["capture"]["kind"], "recording");
    assert_eq!(
        new.body["versions"][0]["capture"]["recording"]["duration_ms"],
        3000
    );

    // A screenshot is the same for both.
    for h in [&plain[..], &aware[..]] {
        let r = c.get("/api/artifacts/fake/pic", h).await;
        assert_eq!(r.body["artifact"]["kind"], "image");
        assert_eq!(r.body["versions"][0]["capture"]["kind"], "screenshot");
    }

    // The stored row never changed: the aware request still sees the video.
    let again = c.get("/api/artifacts/fake/vid", &aware).await;
    assert_eq!(again.body["artifact"]["kind"], "video");
}

/// A fake station that answers `artifacts.publish` with a recording.
fn serve_publish(mut station: LiveLink) {
    tokio::spawn(async move {
        loop {
            let req = next_request(&mut station).await;
            assert_eq!(req.method, "artifacts.publish");
            let published = ArtifactPublished {
                artifact: artifact("vid", ArtifactKind::Video),
                version: version("vid", CaptureKind::Recording),
                created: true,
            };
            station
                .control
                .respond(req.id, Outcome::Ok(json!(published)))
                .await
                .unwrap();
        }
    });
}

#[tokio::test]
async fn the_artifact_publish_answer_is_projected_and_the_index_keeps_the_video() {
    let mut f = fixture().await;
    let link = f.connect().await;
    serve_publish(link);
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    let body = json!({"station": "fake", "path": "capture.mp4"});
    let old_h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let new_h = [
        ("Cookie", cookie.as_str()),
        ("X-CSRF-Token", csrf.as_str()),
        FEATURE,
    ];

    let old = c.post("/api/artifacts/publish", &old_h, body.clone()).await;
    assert_eq!(old.status, 200, "{}", old.text);
    assert_eq!(old.body["artifact"]["kind"], "other");
    assert_eq!(old.body["version"]["capture"], Value::Null);

    let new = c.post("/api/artifacts/publish", &new_h, body).await;
    assert_eq!(new.status, 200, "{}", new.text);
    assert_eq!(new.body["artifact"]["kind"], "video");
    assert_eq!(new.body["version"]["capture"]["kind"], "recording");

    // The index holds the real kind after the legacy answer.
    let plain = [("Cookie", cookie.as_str())];
    let aware = [("Cookie", cookie.as_str()), FEATURE];
    let list = c.get("/api/artifacts", &plain).await;
    assert_eq!(kinds(&list), [("vid".to_owned(), "other".to_owned())]);
    let list = c.get("/api/artifacts", &aware).await;
    assert_eq!(kinds(&list), [("vid".to_owned(), "video".to_owned())]);
}

fn published_event(seq: u64, kind: ArtifactKind) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: "s1".into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::ArtifactPublished {
            artifact_id: format!("A{seq}"),
            version: 1,
            title: "t".to_owned(),
            artifact_kind: kind,
        }),
    }
}

fn artifact_kinds(m: &Value) -> Vec<String> {
    m["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"]["artifact_kind"].as_str().unwrap().to_owned())
        .collect()
}

async fn socket(c: &BoardClient, station: &StationId, path: &str, feature_header: bool) -> Ws {
    let (cookie, _) = c.login().await;
    let origin = c.origin();
    let mut h = vec![("Cookie", cookie.as_str()), ("Origin", origin.as_str())];
    if feature_header {
        h.push(FEATURE);
    }
    let mut ws = c.ws(path, &h).await.expect("the socket opens");
    ws.next_of("snapshot").await;
    ws.send(json!({ "type": "subscribe", "station_id": station, "session_id": "s1" }))
        .await;
    ws.next_of("timeline_start").await;
    ws
}

#[tokio::test]
async fn the_board_socket_projects_artifact_events_for_an_old_session_only() {
    let f = fixture().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![SessionSummary::new("s1".into())],
        )
        .await
        .unwrap();
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![published_event(1, ArtifactKind::Video)],
        )
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    // Without the declaration, the stored timeline and the live events show
    // `other`.
    let mut old = socket(&c, &f.station, "/api/ws", false).await;
    assert_eq!(
        artifact_kinds(&old.next_of("timeline_events").await),
        ["other"]
    );
    // With the query parameter or with the header they show `video`.
    let mut by_query = socket(
        &c,
        &f.station,
        "/api/ws?features=a.v1,artifacts_video.v1",
        false,
    )
    .await;
    assert_eq!(
        artifact_kinds(&by_query.next_of("timeline_events").await),
        ["video"]
    );
    let mut by_header = socket(&c, &f.station, "/api/ws", true).await;
    assert_eq!(
        artifact_kinds(&by_header.next_of("timeline_events").await),
        ["video"]
    );
    // A live event: the same split, and a non-video event is unchanged.
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![
                published_event(2, ArtifactKind::Video),
                published_event(3, ArtifactKind::Html),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        artifact_kinds(&old.next_of("timeline_events").await),
        ["other", "html"]
    );
    assert_eq!(
        artifact_kinds(&by_query.next_of("timeline_events").await),
        ["video", "html"]
    );
    assert_eq!(
        artifact_kinds(&by_header.next_of("timeline_events").await),
        ["video", "html"]
    );
}
