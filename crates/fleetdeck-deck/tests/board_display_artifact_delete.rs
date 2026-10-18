//! The artifact route of a wall display after a deletion (R13 P5a task C): a
//! deleted artifact is `404 unknown_artifact`, and a deletion between two
//! chunks of a streamed version ends the body with an error.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::board::Http;
use common::next_request;
use displays_support::*;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{
    Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion, MAX_CHUNK_BYTES,
};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody};
use fleetdeck_proto::link::Outcome;
use serde_json::json;
use share_support::*;

const PAGE: &str = "<h1>a wall page</h1>";
/// A reported size above the 64 MiB cache limit.
const BIG: u64 = 70 * 1024 * 1024;
const MIB: usize = 1024 * 1024;

fn artifact(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: None,
        title: format!("Title of {id}"),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

fn version(id: &str, size: u64) -> ArtifactVersion {
    ArtifactVersion {
        artifact_id: id.to_owned(),
        version: 1,
        size,
        sha256: String::new(),
        files: vec!["page.html".to_owned()],
        created_at: 1,
        capture: None,
    }
}

async fn direct(store: &DeckStore, display: &str, station: &StationId, id: &str) {
    let body = DirectiveBody::Artifact {
        artifact: ArtifactRef {
            station_id: station.clone(),
            artifact_id: id.to_owned(),
            version: 1,
        },
        pin: false,
        filter: None,
    };
    store
        .display_directive_set(display.to_owned(), DirectiveSender::Owner, body)
        .await
        .unwrap();
}

/// The chunk sizes of a chunked body and whether it ended with its last
/// chunk.
fn chunks_of(r: &Http) -> (Vec<usize>, bool) {
    let mut rest = r.text.as_str();
    let mut sizes = Vec::new();
    loop {
        let Some((size, tail)) = rest.split_once("\r\n") else {
            return (sizes, false);
        };
        let size = usize::from_str_radix(size, 16).expect("a hex chunk size");
        if size == 0 {
            return (sizes, true);
        }
        if tail.len() < size + 2 {
            return (sizes, false);
        }
        sizes.push(size);
        rest = &tail[size + 2..];
    }
}

#[tokio::test]
async fn a_deleted_artifact_is_not_found_on_the_wall_and_a_kept_one_still_shows() {
    let (f, station, c, _) = setup().await;
    // The link stays up for the whole test.
    std::mem::forget(station);
    let st = f.station.clone();
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str()]).await;
    for id in ["doc", "keep"] {
        c.store
            .note_artifact(st.clone(), &artifact(id))
            .await
            .unwrap();
        c.store
            .put_artifact_versions(st.clone(), vec![version(id, PAGE.len() as u64)])
            .await
            .unwrap();
        c.store
            .put_artifact_file(
                st.clone(),
                id.to_owned(),
                1,
                "page.html".to_owned(),
                PAGE.as_bytes().to_vec(),
            )
            .await
            .unwrap();
    }
    let url = |id: &str| format!("/api/displays/self/artifact/{st}/{id}/1/_");
    direct(&c.store, &display, &st, "doc").await;
    let before = get_with(&c, &url("doc"), &cookie).await;
    assert_eq!((before.status, before.text.as_str()), (200, PAGE));

    c.store
        .artifact_delete(st.clone(), "doc".to_owned())
        .await
        .unwrap();
    let after = get_with(&c, &url("doc"), &cookie).await;
    assert_eq!(
        (after.status, after.code(), after.body["message"].as_str()),
        (404, "unknown_artifact", Some("No such artifact."))
    );
    // The directive of the kept artifact is not touched.
    direct(&c.store, &display, &st, "keep").await;
    let kept = get_with(&c, &url("keep"), &cookie).await;
    assert_eq!((kept.status, kept.text.as_str()), (200, PAGE));
}

#[tokio::test]
async fn a_deletion_between_two_chunks_of_a_streamed_wall_artifact_cuts_the_body() {
    let (f, mut station, c, _) = setup().await;
    let st = f.station.clone();
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str()]).await;
    c.store
        .note_artifact(st.clone(), &artifact("doc"))
        .await
        .unwrap();
    direct(&c.store, &display, &st, "doc").await;
    let content = vec![b'a'; 2 * MIB];
    // The fake deletes the artifact when it serves the second file request.
    let (store, hook_station) = (c.store.clone(), st.clone());
    let fake = tokio::spawn(async move {
        let mut offsets = Vec::new();
        loop {
            serve_one(&mut station, &content, &store, &hook_station, &mut offsets).await;
        }
    });
    let url = format!("/api/displays/self/artifact/{st}/doc/1/_");
    let got = get_with(&c, &url, &cookie).await;
    assert_eq!(got.status, 200, "{}", got.text.len());
    let (sizes, complete) = chunks_of(&got);
    assert!(!complete, "{sizes:?}");
    assert!(sizes.iter().sum::<usize>() < 2 * MIB, "{sizes:?}");
    let after = get_with(&c, &url, &cookie).await;
    assert_eq!((after.status, after.code()), (404, "unknown_artifact"));
    fake.abort();
}

/// Answers one request of the display route: `artifacts.get` (a version of
/// `BIG` bytes) or `artifacts.file`. The second file request deletes the
/// artifact first.
async fn serve_one(
    station: &mut LiveLink,
    content: &[u8],
    store: &DeckStore,
    st: &StationId,
    offsets: &mut Vec<u64>,
) {
    let req = next_request(station).await;
    let id = req.params["artifact_id"].as_str().unwrap().to_owned();
    let out = if req.method == "artifacts.get" {
        let v = version(&id, BIG);
        Outcome::Ok(json!(ArtifactDetail {
            artifact: artifact(&id),
            version: v.clone(),
            versions: vec![v],
        }))
    } else {
        assert_eq!(req.method, "artifacts.file");
        let offset = req.params["offset"].as_u64().unwrap();
        offsets.push(offset);
        if offsets.len() == 2 {
            store.artifact_delete(st.clone(), id.clone()).await.unwrap();
        }
        let start = usize::try_from(offset).unwrap();
        let end = (start + usize::try_from(MAX_CHUNK_BYTES).unwrap()).min(content.len());
        Outcome::Ok(
            json!({"path": "page.html", "size": content.len(), "offset": offset,
            "length": end - start, "data_base64": STANDARD.encode(&content[start..end]),
            "eof": end == content.len()}),
        )
    };
    station.control.respond(req.id, out).await.unwrap();
}
