//! The chunked delivery of a cached artifact body (R13 P5a task C): chunks of
//! at most 700 KiB, a tombstone check and the display gate right before each
//! chunk, and a body that ends with an error when a check fails.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::HeaderMap;
use axum::response::Response;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, MAX_CHUNK_BYTES};
use fleetdeck_testkit::Det;
use futures_util::StreamExt;

use super::{Gate, serve_cached};
use crate::store::DeckStore;

const MIB: usize = 1024 * 1024;

async fn store_with_artifact(id: &str) -> (DeckStore, StationId) {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let station = StationId::from("st1".to_owned());
    let artifact = Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: None,
        title: "Big".to_owned(),
        kind: ArtifactKind::Other,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    };
    store
        .note_artifact(station.clone(), &artifact)
        .await
        .unwrap();
    (store, station)
}

fn body_of(
    r: Response,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> {
    r.into_body().into_data_stream()
}

#[tokio::test]
async fn a_cached_artifact_body_comes_in_chunks_of_at_most_700_kib() {
    let (store, station) = store_with_artifact("a1").await;
    let response = serve_cached(
        &store,
        &station,
        "a1",
        HeaderMap::new(),
        vec![b'a'; 2 * MIB],
        None,
    );
    let mut body = body_of(response);
    let mut sizes = Vec::new();
    while let Some(chunk) = body.next().await {
        sizes.push(chunk.unwrap().len());
    }
    let max = usize::try_from(MAX_CHUNK_BYTES).unwrap();
    assert_eq!(sizes, [max, max, 2 * MIB - 2 * max]);
    assert_eq!(sizes.len(), 3);
}

#[tokio::test]
async fn a_deletion_between_two_chunks_of_a_cached_artifact_ends_the_body_with_an_error() {
    let (store, station) = store_with_artifact("a1").await;
    let response = serve_cached(
        &store,
        &station,
        "a1",
        HeaderMap::new(),
        vec![b'a'; 2 * MIB],
        None,
    );
    let mut body = body_of(response);
    let first = body.next().await.unwrap().unwrap();
    assert_eq!(first.len(), usize::try_from(MAX_CHUNK_BYTES).unwrap());
    store
        .artifact_delete(station.clone(), "a1".to_owned())
        .await
        .unwrap();
    assert!(body.next().await.unwrap().is_err());
    // The body is over: no byte follows the error.
    assert!(body.next().await.is_none());
}

#[tokio::test]
async fn a_tombstoned_artifact_sends_no_byte_of_a_cached_body() {
    let (store, station) = store_with_artifact("a1").await;
    store
        .artifact_delete(station.clone(), "a1".to_owned())
        .await
        .unwrap();
    let response = serve_cached(
        &store,
        &station,
        "a1",
        HeaderMap::new(),
        vec![b'a'; 10],
        None,
    );
    let mut body = body_of(response);
    assert!(body.next().await.unwrap().is_err());
}

#[tokio::test]
async fn the_gate_runs_before_each_chunk_of_a_cached_artifact_body() {
    let (store, station) = store_with_artifact("a1").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let gate: Gate = {
        let calls = Arc::clone(&calls);
        Arc::new(move || {
            // The gate passes twice, then refuses.
            let n = calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { n < 2 })
        })
    };
    let response = serve_cached(
        &store,
        &station,
        "a1",
        HeaderMap::new(),
        vec![b'a'; 2 * MIB],
        Some(gate),
    );
    let mut body = body_of(response);
    assert!(body.next().await.unwrap().is_ok());
    assert!(body.next().await.unwrap().is_ok());
    assert!(body.next().await.unwrap().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
