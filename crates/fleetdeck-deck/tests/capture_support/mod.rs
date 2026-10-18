//! The helpers of the owner screenshot tests (R13 P5a task E): a deck with a
//! fake station that answers `desktop.act`, `desktop.info` and
//! `desktop.capture_status`, the capture requests of the owner, and a
//! background responder that logs every request it gets.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::common::board::{BoardClient, Http, board};
use crate::common::*;
use crate::desktop_support::*;
use crate::share_support::*;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::CAPTURE_ID_HEADER;
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

pub const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// The `n`-th valid capture id of a test.
pub fn nth_id(n: u32) -> String {
    format!("01ARZ3NDEKTSV4RRFFQ69G5F{n:02}")
}

pub fn cap_body(monitor: &str) -> Value {
    json!({ "action": "screen_capture", "monitor": monitor })
}

/// The `published` of a screenshot as the station sends it: the station side
/// name in `station_id`, and no provenance.
pub fn published(artifact_id: &str) -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: artifact_id.to_owned(),
            station_id: "station-side-name".to_owned(),
            session_id: None,
            title: "Desktop capture · fake/DP-1".to_owned(),
            kind: ArtifactKind::Image,
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
            files: vec!["capture.png".to_owned()],
            created_at: 1_000,
            capture: None,
        },
        created: true,
    }
}

/// The station answer of a successful screenshot.
pub fn capture_ok(capture_id: &str, artifact_id: &str) -> Outcome {
    Outcome::Ok(json!({
        "address": null,
        "info": info(),
        "capture": { "capture_id": capture_id, "published": published(artifact_id) },
    }))
}

/// The `committed` status of a capture.
pub fn committed(capture_id: &str, artifact_id: &str) -> Value {
    json!({ "state": "committed", "capture_id": capture_id,
            "published": published(artifact_id) })
}

pub fn state(name: &str, capture_id: &str) -> Value {
    json!({ "state": name, "capture_id": capture_id })
}

/// `POST .../desktop/act` as the owner with `capture_id` in the header.
pub async fn capture_with(
    c: &BoardClient,
    h: &Headers,
    st: &str,
    capture_id: Option<&str>,
    body: Value,
) -> Http {
    let origin = tailnet_origin(c);
    let mut headers = refs(h);
    headers.push(("Origin", origin.as_str()));
    if let Some(id) = capture_id {
        headers.push((CAPTURE_ID_HEADER, id));
    }
    c.post(&act_path(st), &headers, body).await
}

/// One screenshot request and the answer of the fake station to its
/// `desktop.act`.
pub async fn capture_exchange(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    st: &str,
    capture_id: &str,
    body: Value,
    reply: Outcome,
) -> (Http, InboundRequest) {
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, reply).await.unwrap();
        req
    };
    tokio::join!(capture_with(c, h, st, Some(capture_id), body), answer)
}

pub fn lookup_path(st: &str, capture_id: &str) -> String {
    format!("/api/stations/{st}/desktop/captures/{capture_id}")
}

/// The deck, a linked fake station and the owner.
pub async fn setup_with(f: Fixture) -> (Fixture, LiveLink, BoardClient, Headers) {
    let mut f = f;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![card("s1"), card("s2")],
        )
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    enable(&c, true);
    (f, station, c, h)
}

/// As [`setup_with`], with the deck store in the file `dir/deck.db`.
pub async fn setup_file(dir: &Path) -> (Fixture, LiveLink, BoardClient, Headers) {
    setup_with(fixture_file(dir).await).await
}

/// As [`setup_with`], and the station leaves `skip` out of its offer.
pub async fn setup_without(skip: &[&str]) -> (Fixture, LiveLink, BoardClient, Headers) {
    let mut f = fixture().await;
    f.station_node
        .local
        .features
        .retain(|x| !skip.contains(&x.as_str()));
    setup_with(f).await
}

/// What a background fake station got.
#[derive(Default)]
pub struct Log(Mutex<Vec<(String, Value)>>);

impl Log {
    pub fn count(&self, method: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(m, _)| m == method)
            .count()
    }
}

/// The status that a background station answers for a capture id.
pub type Status = Arc<dyn Fn(&str) -> Value + Send + Sync>;

/// Answers every `desktop.capture_status` with `status`, every
/// `desktop.info` with the info, and every other request with a failure; it
/// logs them all.
pub fn serve_status(mut station: LiveLink, status: Status) -> Arc<Log> {
    let log = Arc::new(Log::default());
    let me = Arc::clone(&log);
    tokio::spawn(async move {
        loop {
            let req = next_request(&mut station).await;
            me.0.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((req.method.clone(), req.params.clone()));
            let out = match req.method.as_str() {
                "desktop.capture_status" => {
                    let id = req.params["capture_id"].as_str().unwrap_or_default();
                    Outcome::Ok(status(id))
                }
                "desktop.info" => Outcome::Ok(serde_json::to_value(info()).unwrap()),
                _ => err("failed", "not served"),
            };
            station.control.respond(req.id, out).await.unwrap();
        }
    });
    log
}

/// The capture row, as the store has it.
pub async fn row(
    f: &Fixture,
    capture_id: &str,
) -> Option<fleetdeck_deck::store::desktop_captures::CaptureRow> {
    f.store
        .capture_get(f.station.clone(), capture_id.to_owned())
        .await
        .unwrap()
}

/// The deck, a linked fake station and the owner, with `desktop_control` on.
pub async fn setup_default() -> (Fixture, LiveLink, BoardClient, Headers) {
    setup_with(fixture().await).await
}
