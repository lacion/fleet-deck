//! The recording helpers of the P6 deck tests (R13 P5b-2 task Q-K): the
//! `record_monitor` call, a fake station that answers a recording with a
//! coordinator recording provenance (and that can run a script of lease
//! requests while the deck waits for the `desktop.act`), and the readers of a
//! recording row.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{CaptureLeaseRequest, LeasePhase};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use fleetdeck_deck::desktop_capture::recording::RecordingRow;
use fleetdeck_proto::StationId;

use super::*;

/// The artifact ID that the recording station answers.
pub const REC_ART: &str = "01ARZ3NDEKTSV4RRFFQ69G5REC";

/// The `record_monitor` call of `monitor` DP-1.
pub fn record_call(request_id: &str, seconds: u32, station: Option<&str>) -> Value {
    let mut args = json!({"request_id": request_id, "monitor": "DP-1", "seconds": seconds});
    if let Some(s) = station {
        args["station"] = json!(s);
    }
    json!({"tool": "record_monitor", "arguments": args})
}

/// A change of the `published` JSON that the station answers.
pub type Tweak = Arc<dyn Fn(&mut Value) + Send + Sync>;

/// What the fake station answers for a recording.
#[derive(Clone)]
pub struct RecSpec {
    pub seconds: u32,
    pub size: u64,
    pub sha: String,
    pub grant: String,
    pub station: String,
    /// The artifact ID of the station result.
    pub artifact: String,
    pub tweak: Option<Tweak>,
}

impl RecSpec {
    pub fn of(w: &World, seconds: u32) -> Self {
        Self {
            seconds,
            size: 4096,
            sha: cap_sha(b"video"),
            grant: w.b.grant_id.clone(),
            station: w.station().to_string(),
            artifact: REC_ART.to_owned(),
            tweak: None,
        }
    }

    pub fn tweaked(mut self, f: impl Fn(&mut Value) + Send + Sync + 'static) -> Self {
        self.tweak = Some(Arc::new(f));
        self
    }

    pub fn published(&self, id: &str) -> Value {
        let seconds = self.seconds;
        let capture = json!({
            "capture_id": id, "station_id": "station-side", "monitor": "DP-1",
            "kind": "recording", "caller": "coordinator",
            "caller_station": self.station, "caller_session": "coord-1",
            "caller_grant_id": self.grant, "at": 1000,
            "recording": {"requested_seconds": seconds,
                "duration_ms": u64::from(seconds) * 1000, "width": 1920, "height": 1080,
                "stop_reason": "duration"}});
        let mut published = json!({
            "artifact": {"artifact_id": self.artifact, "station_id": "station-side",
                "session_id": null, "title": "Desktop recording", "kind": "video",
                "latest_version": 1, "created_at": 1000, "updated_at": 1000,
                "author": "coordinator:x/coord-1"},
            "version": {"artifact_id": self.artifact, "version": 1, "size": self.size,
                "sha256": self.sha, "files": ["capture.mp4"], "created_at": 1000,
                "capture": capture},
            "created": true});
        if let Some(tweak) = &self.tweak {
            tweak(&mut published);
        }
        published
    }

    pub fn answer(&self, id: &str) -> Value {
        json!({"address": null, "info": info(),
            "capture": {"capture_id": id, "published": self.published(id)}})
    }
}

/// One step of the scripted station while the deck waits for the recording.
#[derive(Clone)]
pub enum Step {
    /// A `desktop.capture_lease` request of this phase.
    Lease(LeasePhase),
    /// The answer of `coordinator.policy_now` from now on: `ok`, `false`
    /// (the binding does not match) or `uncertain` (no `binding_matches`).
    Caller(&'static str),
    /// The answer of `desktop.capture_policy_now` from now on: `ok`, or the
    /// name of a flag that turns off.
    Target(&'static str),
    /// A side effect on the deck (a revoke, a switch).
    Do(Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>),
}

/// A [`Step::Do`] of an async closure.
pub fn hook<F, Fut>(f: F) -> Step
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Step::Do(Arc::new(move || f().boxed()))
}

/// How the scripted station ends the `desktop.act`.
#[derive(Clone)]
pub enum Final {
    Ok(RecSpec),
    Err(&'static str, &'static str),
}

/// The lease answers that the script collected, in order.
pub type Answers = Arc<Mutex<Vec<Outcome>>>;

fn caller_answer(mode: &str) -> Outcome {
    match mode {
        "false" => ok(json!({"fleet_control": true, "bound": true, "binding_matches": false})),
        "uncertain" => ok(json!({"fleet_control": true, "bound": true})),
        _ => ok(json!({"fleet_control": true, "bound": true, "binding_matches": true})),
    }
}

fn target_answer(mode: &str, epoch: &str) -> Outcome {
    let mut p = json!({
        "valid": true, "desktop_epoch": epoch, "allow_desktop": true,
        "allow_desktop_capture": true, "monitor_allowed": true,
        "monitor_present": true, "unlocked": true,
    });
    if mode != "ok" {
        p[mode] = json!(false);
    }
    ok(p)
}

/// The scripted station: the policies of the call stage are `ok`; the
/// `desktop.act` runs `steps` (the lease answers go to `answers`) and then
/// ends with `end`.
pub fn script_station(steps: Vec<Step>, end: Final, answers: Answers) -> Handler {
    let modes = Arc::new(Mutex::new(("ok", "ok")));
    handler(move |req| {
        let (steps, end, answers, modes) = (
            steps.clone(),
            end.clone(),
            Arc::clone(&answers),
            Arc::clone(&modes),
        );
        async move {
            match req.method.as_str() {
                "coordinator.policy_now" => {
                    let mode = modes.lock().unwrap().0;
                    Some(caller_answer(mode))
                }
                "desktop.capture_policy_now" => {
                    let mode = modes.lock().unwrap().1;
                    let epoch = req.control.params().local_desktop_epoch.clone().unwrap();
                    Some(target_answer(mode, &epoch))
                }
                "desktop.act" => {
                    let id = req.params["authority"]["action_id"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    let hash = req.params["authority"]["action_hash"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    for step in steps {
                        match step {
                            Step::Lease(phase) => {
                                let lease = CaptureLeaseRequest {
                                    capture_id: id.clone(),
                                    action_hash: hash.clone(),
                                    sequence: 7,
                                    phase,
                                };
                                let out = req
                                    .control
                                    .request("desktop.capture_lease", None, json!(lease))
                                    .await
                                    .unwrap();
                                answers.lock().unwrap().push(out);
                            }
                            Step::Caller(mode) => modes.lock().unwrap().0 = mode,
                            Step::Target(mode) => modes.lock().unwrap().1 = mode,
                            Step::Do(f) => f().await,
                        }
                    }
                    Some(match end {
                        Final::Ok(spec) => ok(spec.answer(&id)),
                        Final::Err(code, message) => fail(code, message),
                    })
                }
                _ => Some(standard_answer(&req)),
            }
        }
    })
}

/// The station that answers a recording and no lease.
pub fn rec_station(spec: RecSpec) -> Handler {
    script_station(Vec::new(), Final::Ok(spec), Answers::default())
}

/// A world of `BOTH` classes whose grant allows recordings.
pub async fn record_world(cap: u32, seconds: u32) -> World {
    world_with(&BOTH, |g| {
        g.recording_cap = cap;
        g.recording_second_cap = seconds;
    })
    .await
}

/// A world whose grant allows 50 recordings of 6000 seconds.
pub async fn rich_world() -> World {
    record_world(50, 6000).await
}

pub async fn rec_row(w: &World, id: &str, st: &StationId) -> RecordingRow {
    w.f.store
        .recording_get(st.clone(), id.to_owned())
        .await
        .unwrap()
        .expect("a recording row")
}

/// The grant counters `(desktop, capture, recording, recording seconds)`.
pub async fn counters(w: &World) -> (u32, u32, u32, u32) {
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    (
        g.desktop_used,
        g.capture_used,
        g.recording_used,
        g.recording_second_used,
    )
}

/// The stored `charged` JSON of an operation.
pub async fn charged_json(w: &World, id: &str) -> Value {
    let id = id.to_owned();
    let text: String =
        w.f.store
            .db()
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT charged FROM coordinator_desktop_ops WHERE operation_id = ?1",
                    [id],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The recording answer of a committed `record_monitor`; the reply and the ID.
pub async fn committed_recording(w: &World, rid: &str, seconds: u32) -> (Value, String) {
    w.fake.set(rec_station(RecSpec::of(w, seconds)));
    let reply = w
        .call(&format!("c-{rid}"), record_call(rid, seconds, None))
        .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    let id = reply["result"]["capture_id"].as_str().unwrap().to_owned();
    (reply, id)
}
