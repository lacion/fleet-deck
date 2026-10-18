//! The screenshot and image helpers of the P6 deck tests (R13 P6 task T3): a
//! fake station that answers a screenshot and the reads of its image, and the
//! queries of the capture tools.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use fleetdeck_deck::store::coordinator_desktop::{DesktopOp, request_key};
use fleetdeck_deck::store::desktop_captures::CaptureRow;
use fleetdeck_proto::StationId;

use super::*;

/// The artifact ID that the fake station answers.
pub const ART: &str = "01ARZ3NDEKTSV4RRFFQ69G5ART";

/// A PNG-like file of `size` bytes: the signature and filler.
pub fn cap_png(size: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(size, 7);
    bytes
}

pub fn cap_sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The `screenshot` call of `monitor` DP-1.
pub fn shot_call(request_id: &str, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "monitor": "DP-1"});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "screenshot", "arguments": args})
}

pub fn capture_status_query(request_id: &str) -> Value {
    json!({"tool": "capture_status", "arguments": {"request_id": request_id}})
}

pub fn image_query(station: &str, artifact_id: &str, version: u32) -> Value {
    json!({"tool": "read_capture_image", "arguments": {"artifact": {
        "station_id": station, "artifact_id": artifact_id, "version": version}}})
}

/// The provenance that a station reports.
#[derive(Clone)]
pub enum Prov {
    Right,
    Grant(&'static str),
    Missing,
}

/// What the fake station answers for a screenshot.
#[derive(Clone)]
pub struct ShotSpec {
    pub size: u64,
    pub sha: String,
    pub prov: Prov,
    pub grant: String,
    pub station: String,
}

impl ShotSpec {
    pub fn of(w: &World, bytes: &[u8]) -> Self {
        Self {
            size: bytes.len() as u64,
            sha: cap_sha(bytes),
            prov: Prov::Right,
            grant: w.b.grant_id.clone(),
            station: w.station().to_string(),
        }
    }

    pub fn published(&self, id: &str) -> Value {
        let prov = match &self.prov {
            Prov::Missing => Value::Null,
            p => json!({
                "capture_id": id, "station_id": "station-side", "monitor": "DP-1",
                "kind": "screenshot", "caller": "coordinator",
                "caller_station": self.station, "caller_session": "coord-1",
                "caller_grant_id": match p {
                    Prov::Grant(g) => (*g).to_owned(),
                    _ => self.grant.clone(),
                },
                "at": 1000}),
        };
        let mut version = json!({"artifact_id": ART, "version": 1, "size": self.size,
            "sha256": self.sha, "files": ["capture.png"], "created_at": 1000});
        if !prov.is_null() {
            version["capture"] = prov;
        }
        json!({"artifact": {"artifact_id": ART, "station_id": "station-side",
            "session_id": null, "title": "Desktop capture", "kind": "image",
            "latest_version": 1, "created_at": 1000, "updated_at": 1000,
            "author": "coordinator:x/coord-1"}, "version": version, "created": true})
    }
}

/// A one-chunk answer of `desktop.coordinator_read`.
pub fn chunk_of(bytes: &[u8]) -> Value {
    json!({"path": "capture.png", "size": bytes.len(), "offset": 0, "length": bytes.len(),
        "data_base64": STANDARD.encode(bytes), "eof": true})
}

/// The answer of the fake station to a `desktop.coordinator_read`.
pub type ReadFn = Arc<dyn Fn(&Req) -> Outcome + Send + Sync>;

/// A read that always answers the whole of `bytes`.
pub fn reads_of(bytes: Vec<u8>) -> ReadFn {
    Arc::new(move |_| ok(chunk_of(&bytes)))
}

/// The fake station: the screenshot of `spec`, the reads of `read`.
pub fn shot_station(spec: ShotSpec, read: ReadFn) -> Handler {
    handler(move |req| {
        let out = match req.method.as_str() {
            "desktop.act" => {
                let id = req.params["authority"]["action_id"].as_str().unwrap_or("");
                ok(json!({"address": null, "info": info(),
                    "capture": {"capture_id": id, "published": spec.published(id)}}))
            }
            "desktop.coordinator_read" => read(&req),
            _ => standard_answer(&req),
        };
        async move { Some(out) }
    })
}

pub async fn cap_op(w: &World, rid: &str) -> DesktopOp {
    w.f.store
        .desktop_op_by_key(&w.b.grant_id, &request_key(rid))
        .await
        .unwrap()
        .expect("an operation row")
}

pub async fn cap_row(w: &World, id: &str, st: &StationId) -> CaptureRow {
    w.f.store
        .capture_get(st.clone(), id.to_owned())
        .await
        .unwrap()
        .expect("a capture row")
}

pub async fn capture_world() -> World {
    world_with(&BOTH, |_| {}).await
}

/// A world with a committed screenshot of `bytes`; the reply and the ID.
pub async fn committed_shot(bytes: &[u8]) -> (World, Value, String) {
    let w = capture_world().await;
    w.fake.set(shot_station(
        ShotSpec::of(&w, bytes),
        reads_of(bytes.to_vec()),
    ));
    let reply = w.call("c1", shot_call("req-1", None)).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    let id = reply["result"]["capture_id"].as_str().unwrap().to_owned();
    (w, reply, id)
}

pub async fn charged(w: &World) -> u64 {
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    g.image_byte_used
}

pub async fn count(w: &World, sql: &'static str) -> i64 {
    w.f.store
        .db()
        .call(move |conn| Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap()
}

pub fn published_of(r: &CaptureRow) -> fleetdeck_proto::artifacts::ArtifactPublished {
    r.published.clone().expect("a result")
}
