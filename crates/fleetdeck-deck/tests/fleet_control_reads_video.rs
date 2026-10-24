//! The agent read of an artifact (ADR-008 `artifact` query, R13 P5b task K2,
//! decision O12-10 item 29): a `video` artifact is refused with the same
//! answer as an image and a PDF, and nothing goes to the station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::fleet::*;
use common::*;
use fleetdeck_proto::RejectCode;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind};
use fleetdeck_proto::mutation::CoordinatorQuery;

fn note(id: &str, kind: ArtifactKind) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: Some(s("w1")),
        title: format!("Title of {id}"),
        kind,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

#[tokio::test]
async fn an_artifact_read_of_a_video_is_refused_like_an_image_and_a_pdf() {
    let mut w = world().await;
    let b = bound(&w.r.f, "coord-1", &CLASSES, true).await;
    let store = w.r.f.store.clone();
    let st = w.r.f.station.clone();
    for (id, kind) in [
        ("vid", ArtifactKind::Video),
        ("pic", ArtifactKind::Image),
        ("pdf", ArtifactKind::Pdf),
    ] {
        store
            .note_artifact(st.clone(), &note(id, kind))
            .await
            .unwrap();
    }
    let read = |id: &str| CoordinatorQuery::Artifact {
        artifact_id: id.to_owned(),
        version: None,
    };
    for id in ["vid", "pic", "pdf"] {
        let e = refusal(&w.r.f, &b, read(id)).await;
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                RejectCode::InvalidCommand,
                "only text artifacts can be read"
            ),
            "{id}"
        );
    }
    // Nothing went to the station.
    no_request(&mut w.r.station, std::time::Duration::from_millis(100)).await;
}
