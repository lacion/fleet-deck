//! The total fetch limit of a capture file (R13 P5a contract 7.4): the limit
//! of 60 s is scaled down to a few hundred milliseconds, and a fake station
//! answers each `artifacts.file` request slowly. An ordinary artifact with
//! the same slowness is not cut.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::Inbound;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_testkit::Det;
use futures_util::StreamExt;

use super::*;
use crate::store::stations::NewStation;

/// The bytes of the fake file, and the bytes of one answer.
const SIZE: usize = 60;
const STEP: usize = 6;
/// The wait of the fake station before each answer.
const SLOW: Duration = Duration::from_millis(100);
/// The scaled total limit: about three answers fit, ten do not.
const TOTAL: Duration = Duration::from_millis(350);
const T: Duration = Duration::from_secs(10);

/// A station that answers `artifacts.file` after [`SLOW`], [`STEP`] bytes at
/// a time, and the deck side of its link.
struct Rig {
    control: Control,
    store: DeckStore,
    rec: StationRecord,
    _station: tokio::task::JoinHandle<LiveLink>,
    _deck: tokio::task::JoinHandle<LiveLink>,
}

fn node(role: Role) -> (NodeConfig, Arc<Grants>, Arc<Identity>) {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    let grants = Grants::new();
    let config = NodeConfig::new(
        Arc::clone(&identity),
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&grants));
    (config, grants, identity)
}

async fn rig() -> Rig {
    let (deck, _deck_grants, deck_id) = node(Role::Deck);
    let (station, station_grants, station_id) = node(Role::Station);
    deck.pins.pin(station_id.public_key(), Role::Station);
    station.pins.pin(deck_id.public_key(), Role::Deck);
    station_grants.grant(deck_id.fingerprint(), [Scope::Read, Scope::Drive]);
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut listener = node::listen(deck.clone(), addr).await.unwrap();
    let (dialed, accepted) = tokio::time::timeout(T, async {
        tokio::join!(
            node::dial(&station, listener.local_addr, deck_id.public_key()),
            listener.next_link()
        )
    })
    .await
    .expect("link setup");
    let at_deck = fleetdeck_link::start(&deck, accepted.expect("listener"));
    let mut at_station = fleetdeck_link::start(&station, dialed.unwrap());
    let control = at_deck.control.clone();
    // The deck link stays open for the whole test.
    let deck_task = tokio::spawn(async move {
        let mut at_deck = at_deck;
        while at_deck.inbound.recv().await.is_some() {}
        at_deck
    });
    let serving = tokio::spawn(async move {
        while let Some(inbound) = at_station.inbound.recv().await {
            let Inbound::Request(req) = inbound else {
                continue;
            };
            if req.method != "artifacts.file" {
                continue;
            }
            tokio::time::sleep(SLOW).await;
            let offset = usize::try_from(req.params["offset"].as_u64().unwrap()).unwrap();
            let end = (offset + STEP).min(SIZE);
            let out = Outcome::Ok(serde_json::json!({
                "path": "capture.png",
                "size": SIZE,
                "offset": offset,
                "length": end - offset,
                "data_base64": STANDARD.encode(vec![b'x'; end - offset]),
                "eof": end == SIZE,
            }));
            if at_station.control.respond(req.id, out).await.is_err() {
                break;
            }
        }
        at_station
    });
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let rec = store
        .pin_station(NewStation {
            key: station_id.public_key(),
            name: "fake".to_owned(),
            scopes: BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap();
    Rig {
        control,
        store,
        rec,
        _station: serving,
        _deck: deck_task,
    }
}

fn version(capture: bool) -> ArtifactVersion {
    ArtifactVersion {
        artifact_id: "a1".to_owned(),
        version: 1,
        size: SIZE as u64,
        sha256: String::new(),
        files: vec!["capture.png".to_owned()],
        created_at: 1,
        capture: capture.then(|| CaptureProvenance {
            capture_id: "01JCAPTURE0000000000000000".to_owned(),
            station_id: "st1".to_owned(),
            monitor: "DP-1".to_owned(),
            kind: CaptureKind::Screenshot,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 1,
        }),
    }
}

/// Stores the artifact and its version, so that a cache write has a row.
async fn store_artifact(r: &Rig, capture: bool) {
    let artifact = fleetdeck_proto::artifacts::Artifact {
        artifact_id: "a1".to_owned(),
        station_id: String::new(),
        session_id: None,
        title: "Shot".to_owned(),
        kind: fleetdeck_proto::artifacts::ArtifactKind::Image,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "owner".to_owned(),
    };
    r.store
        .note_artifact(r.rec.station_id.clone(), &artifact)
        .await
        .unwrap();
    r.store
        .put_artifact_versions(r.rec.station_id.clone(), vec![version(capture)])
        .await
        .unwrap();
}

/// The answer that `stream_with` builds after its first chunk.
async fn streamed_body(r: &Rig, deadline: Option<Instant>) -> Response {
    let (part, last) = read_chunk(&r.control, "fake", "a1", 1, "capture.png", 0, deadline)
        .await
        .unwrap();
    chunked(
        Chunks {
            store: r.store.clone(),
            station: r.rec.station_id.clone(),
            artifact_id: "a1".to_owned(),
            remote: Some(Remote {
                control: r.control.clone(),
                name: "fake".to_owned(),
                version: 1,
                path: "capture.png".to_owned(),
                deadline,
            }),
            fetched: part.len() as u64,
            eof: last,
            buf: Bytes::from(part),
            done: false,
            gate: None,
        },
        HeaderMap::new(),
    )
}

#[test]
fn the_total_limit_is_60_seconds_and_only_a_capture_version_has_a_deadline() {
    assert_eq!(CAPTURE_FETCH_TOTAL, Duration::from_secs(60));
    let started = Instant::now();
    assert_eq!(
        fetch_deadline(&version(true), started, CAPTURE_FETCH_TOTAL),
        Some(started + Duration::from_secs(60))
    );
    assert_eq!(
        fetch_deadline(&version(false), started, CAPTURE_FETCH_TOTAL),
        None
    );
}

#[tokio::test]
async fn a_capture_body_that_passes_the_total_limit_ends_with_an_error() {
    let r = rig().await;
    store_artifact(&r, true).await;
    let deadline = fetch_deadline(&version(true), Instant::now(), TOTAL);
    let response = streamed_body(&r, deadline).await;
    let mut body = response.into_body().into_data_stream();
    let mut received = 0;
    let error = loop {
        match body.next().await.expect("the body ends with an error") {
            Ok(chunk) => received += chunk.len(),
            Err(e) => break e.to_string(),
        }
    };
    assert!(received > 0 && received < SIZE, "{received} bytes");
    assert!(error.contains("timeout"), "{error}");
    // The body is over: no byte follows the error.
    assert!(body.next().await.is_none());
}

#[tokio::test]
async fn an_ordinary_body_with_the_same_slowness_is_not_cut() {
    let r = rig().await;
    store_artifact(&r, false).await;
    let deadline = fetch_deadline(&version(false), Instant::now(), TOTAL);
    assert_eq!(deadline, None);
    let started = Instant::now();
    let response = streamed_body(&r, deadline).await;
    let mut body = response.into_body().into_data_stream();
    let mut received = 0;
    while let Some(chunk) = body.next().await {
        received += chunk.unwrap().len();
    }
    assert_eq!(received, SIZE);
    // The fetch took longer than the scaled limit that cuts a capture.
    assert!(started.elapsed() > TOTAL);
}

#[tokio::test]
async fn a_capture_chunk_request_past_the_deadline_gives_the_timeout_answer() {
    let r = rig().await;
    let deadline = Some(Instant::now() + SLOW / 2);
    let e = read_chunk(&r.control, "fake", "a1", 1, "capture.png", 0, deadline)
        .await
        .unwrap_err();
    assert_eq!(e.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(e.code, "timeout");
    assert_eq!(e.message, "the station did not answer in time");
    // A deadline that has passed does not send a request.
    let past = Some(Instant::now());
    let e = read_chunk(&r.control, "fake", "a1", 1, "capture.png", 0, past)
        .await
        .unwrap_err();
    assert_eq!(e.code, "timeout");
}

#[tokio::test]
async fn a_capture_cache_fill_that_passes_the_total_limit_caches_nothing() {
    let r = rig().await;
    store_artifact(&r, true).await;
    let deadline = fetch_deadline(&version(true), Instant::now(), TOTAL);
    let files = vec!["capture.png".to_owned()];
    let e = fill_cache(&r.store, &r.control, &r.rec, "a1", 1, &files, deadline)
        .await
        .unwrap_err();
    assert_eq!((e.status, e.code), (StatusCode::GATEWAY_TIMEOUT, "timeout"));
    let cached = r
        .store
        .cached_versions(r.rec.station_id.clone(), "a1".to_owned())
        .await
        .unwrap();
    assert!(cached.is_empty());
}

#[tokio::test]
async fn an_ordinary_cache_fill_with_the_same_slowness_is_not_cut() {
    let r = rig().await;
    store_artifact(&r, false).await;
    let deadline = fetch_deadline(&version(false), Instant::now(), TOTAL);
    let files = vec!["capture.png".to_owned()];
    fill_cache(&r.store, &r.control, &r.rec, "a1", 1, &files, deadline)
        .await
        .unwrap();
    let cached = r
        .store
        .cached_versions(r.rec.station_id.clone(), "a1".to_owned())
        .await
        .unwrap();
    assert_eq!(cached, [1]);
}
