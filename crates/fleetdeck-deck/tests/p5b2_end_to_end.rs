//! The P5b-2 contract end to end over real links (R13 P5b-2 task Q-P1): the
//! `record_monitor` tool of a caller station A (real station crate, real
//! coordinator MCP endpoint), the deck (real deck crate) and a target station B
//! (real station crate, fake desktop, fake `wf-recorder` behind the `shim-exec`
//! seam). The crate tests of Q-F, Q-K, Q-S and Q-W are not repeated here; each
//! test names the items of `docs/v2/design/omarchy-p5b.md` §17 that it shows.
//!
//! A recorder that holds (`recorder::hold`) writes nothing, so a test acts
//! while the recording runs. Every recording of a test lasts 1 to 3 seconds.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod p6_support;
mod settings_support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_deck::store::LinkFeature;
use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind};
use fleetdeck_proto::recording::StopReason;
use fleetdeck_proto::version::feature;
use fleetdeck_station::store::capture_ops::CaptureState;
use p6_support::board::{enable_desktop, revoke};
use p6_support::mcp::Tool;
use p6_support::*;
use serde_json::{Value, json};
use settings_support::{Headers, refs};

use common::board::{BoardClient, Http};

/// The recording cap and the seconds cap of an ordinary recording world.
const BUDGET: (u32, u32) = (5, 600);

/// A world with the recording allowance `budget` and the fake recorder.
async fn world_with(budget: (u32, u32)) -> World {
    let w = World::with(Setup {
        recording: Some(budget),
        ..Setup::default()
    })
    .await;
    recorder::add_recorder(w.b.rig.dir.path());
    w
}

async fn world() -> World {
    world_with(BUDGET).await
}

fn root(w: &World) -> std::path::PathBuf {
    w.b.rig.dir.path().to_owned()
}

fn args(w: &World, id: &str, seconds: u32) -> Value {
    json!({"request_id": id, "station": w.to_b(), "monitor": "DP-1", "seconds": seconds})
}

async fn record(w: &World, id: &str, seconds: u32) -> Tool {
    w.mcp.call("record_monitor", args(w, id, seconds)).await
}

/// A `record_monitor` call of A in a task.
fn spawn_record(w: &World, id: &str, seconds: u32) -> tokio::task::JoinHandle<Tool> {
    let (client, args) = (w.mcp.clone(), args(w, id, seconds));
    tokio::spawn(async move { client.call("record_monitor", args).await })
}

/// The counters of the grant: desktop, capture, recording, recording seconds.
async fn used(w: &World) -> (u32, u32, u32, u32) {
    let g = w
        .deck()
        .store
        .grant(w.grant_id.clone())
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

async fn artifact_count(w: &World) -> usize {
    w.deck()
        .store
        .artifacts(None, None, None)
        .await
        .unwrap()
        .len()
}

/// The headers of an owner request of the board.
fn owner_headers(c: &BoardClient, h: &Headers) -> Headers {
    let mut v = h.clone();
    v.push((
        "Origin".to_owned(),
        format!("https://deck.tail1234.ts.net:{}", c.addr.port()),
    ));
    v.push((
        "X-FleetDeck-Features".to_owned(),
        "artifacts_video.v1".to_owned(),
    ));
    v
}

/// The active recording discovery of the board.
async fn active(c: &BoardClient, h: &Headers) -> Http {
    let headers = owner_headers(c, h);
    c.get("/api/stations/beta/desktop/recording", &refs(&headers))
        .await
}

/// A recording that runs while a test acts: the recorder holds, the board
/// shows the recording phase.
struct Running {
    c: BoardClient,
    h: Headers,
    call: tokio::task::JoinHandle<Tool>,
    capture_id: String,
}

/// Starts a held `record_monitor` call and waits until the active discovery
/// of the board shows the recording phase.
async fn start_held(w: &World, id: &str, seconds: u32) -> Running {
    recorder::hold(&root(w));
    let call = spawn_record(w, id, seconds);
    let (c, h) = w.deck().board().await;
    enable_desktop(&c);
    let found: std::sync::Mutex<Option<String>> = std::sync::Mutex::default();
    eventually("an active recording in the recording phase", || async {
        let r = active(&c, &h).await;
        let now = r.status == 200
            && r.body["state"] == "active"
            && r.body["status"]["recording"]["phase"] == "recording";
        if now {
            *found.lock().unwrap() = r.body["capture_id"].as_str().map(str::to_owned);
        }
        now
    })
    .await;
    let capture_id = found.lock().unwrap().take().expect("a capture ID");
    Running {
        c,
        h,
        call,
        capture_id,
    }
}

/// Waits until no process of the fake recorder lives.
async fn recorder_gone(w: &World) {
    let root = root(w);
    eventually("no live wf-recorder", || async {
        recorder::live(&root) == 0
    })
    .await;
}

/// The sha-256 of `bytes` as lowercase hex.
fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every key of a JSON value, at any depth.
fn keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                keys(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
        _ => {}
    }
}

/// The schema of §13.1, as the contract states it.
fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["request_id", "monitor", "seconds"],
        "properties": {
            "request_id": {"type": "string", "minLength": 1, "maxLength": 64,
                           "pattern": "^[A-Za-z0-9_-]+$"},
            "station": {"type": "string", "minLength": 1, "maxLength": 128,
                        "pattern": "^[A-Za-z0-9_-]+$"},
            "monitor": {"type": "string", "minLength": 1, "maxLength": 64,
                        "pattern": "^[A-Za-z0-9._-]+$"},
            "seconds": {"type": "integer", "minimum": 1, "maximum": 120}
        }
    })
}

// ------------------------------------------------------------------ proof 1

/// Proof 1 (§17 items 17, 11, 38): `record_monitor` of A records a monitor of
/// B. The catalog has the §13.1 schema; the fixed disclosure comes before
/// `wf-recorder`; the result is a reference and no video; the deck artifact is
/// a coordinator `video`; the four counters are charged; the audit row has no
/// video bytes.
#[tokio::test]
async fn a_remote_recording_commits_a_reference_only_result_with_its_disclosure_and_charges() {
    let w = world().await;
    let root = root(&w);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let (_, listed) = mcp::post(&w.mcp.url, Some(&w.mcp.token), &body).await;
    let listed: Value = serde_json::from_str(&listed).unwrap();
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "record_monitor")
        .expect("record_monitor is listed");
    assert_eq!(tool["inputSchema"], schema());

    let out = record(&w, "r1", 1).await;
    let result = out.ok();
    assert_eq!(result["request_id"], "r1");
    assert_eq!(result["state"], "committed", "{}", out.raw);
    assert_eq!(result["monitor"], "DP-1");
    assert_eq!(result["requested_seconds"], 1);
    assert_eq!(result["stop_reason"], "duration");
    assert!(result["duration_ms"].as_u64().is_some_and(|d| d > 0));
    assert!(result["at"].as_i64().is_some_and(|t| t > 0));
    let clip = recorder::fixture(&root);
    assert_eq!(result["size"], clip.len());
    assert_eq!(result["sha256"], sha256(&clip));
    let artifact = result["artifact"].clone();
    assert_eq!(artifact["station_id"], w.b_id.as_str());
    assert_eq!(artifact["version"], 1);

    // The disclosure came before the recorder, with the fixed text.
    let order = w.b.rig.order();
    let note = order.iter().position(|p| p == "notify-send").unwrap();
    let rec = order.iter().position(|p| p == "wf-recorder").unwrap();
    assert!(note < rec, "{order:?}");
    let notes = w.b.rig.calls_of("notify-send");
    assert_eq!(notes.len(), 1, "{:?}", w.b.rig.calls());
    let n = notes[0].args.len();
    assert_eq!(
        notes[0].args[n - 2..],
        [
            "Fleet Deck",
            "Fleet Deck is recording DP-1 for swift-fox for up to 1 seconds."
        ]
    );

    // The result is a reference: one text block, no key of a payload, no byte
    // of the clip.
    assert_eq!(out.content.len(), 1, "{}", out.raw);
    assert_eq!(out.content[0]["type"], "text");
    let mut found = Vec::new();
    keys(&result, &mut found);
    for forbidden in ["data", "base64", "path", "token"] {
        assert!(
            !found.iter().any(|k| k == forbidden),
            "{forbidden}: {found:?}"
        );
    }
    let first_64 = STANDARD.encode(&clip[..64]);
    for needle in ["ftyp", "base64", first_64.as_str()] {
        assert!(!out.raw.contains(needle), "{needle} in the result");
    }
    assert!(out.content.iter().all(|b| b["type"] != "image"));

    // The deck artifact is a video with the provenance of the coordinator.
    let id = artifact["artifact_id"].as_str().unwrap().to_owned();
    let store = &w.deck().store;
    let indexed = store
        .artifact(w.b_id.clone(), id.clone())
        .await
        .unwrap()
        .expect("an indexed artifact");
    assert_eq!(indexed.kind, ArtifactKind::Video);
    let versions = store.artifact_versions(w.b_id.clone(), id).await.unwrap();
    let prov = versions[0].capture.clone().expect("a provenance");
    assert_eq!(prov.caller, CaptureCaller::Coordinator);
    assert_eq!(prov.kind, CaptureKind::Recording);
    assert_eq!(prov.station_id, w.b_id.as_str());
    assert_eq!(prov.caller_station.as_deref(), Some(w.a_id.as_str()));
    assert_eq!(prov.caller_session.as_deref(), Some(CALLER));
    assert_eq!(prov.caller_grant_id.as_deref(), Some(w.grant_id.as_str()));
    assert!(prov.recording.is_some());

    // The four counters are charged once.
    assert_eq!(used(&w).await, (1, 1, 1, 1));

    // The audit has the recording row of the coordinator and no clip bytes.
    let rows = store.audit_log(500).await.unwrap();
    let recording_rows = rows
        .iter()
        .filter(|r| r.actor == "coordinator" && r.action == "desktop.capture")
        .filter(|r| {
            let d: Value =
                serde_json::from_str(r.detail.as_deref().unwrap_or("{}")).unwrap_or(Value::Null);
            d["kind"] == "recording"
        })
        .count();
    assert!(recording_rows >= 1, "{}", w.audit_text().await);
    let audit = w.audit_text().await;
    for needle in ["ftyp", first_64.as_str()] {
        assert!(!audit.contains(needle), "{needle} in the audit");
    }
}

// ------------------------------------------------------------------ proof 2

/// Proof 2 (§17 items 46, 17): a screenshot-only grant has no `record_monitor`
/// in its catalog, a call is an error, and nothing ran on B.
#[tokio::test]
async fn a_screenshot_only_grant_has_no_recording_tool_and_nothing_runs_on_the_target() {
    let w = World::start().await;
    recorder::add_recorder(w.b.rig.dir.path());
    let names = w.mcp.names().await;
    assert!(names.iter().any(|n| n == "screenshot"), "{names:?}");
    assert!(!names.iter().any(|n| n == "record_monitor"), "{names:?}");
    let out = record(&w, "r1", 1).await;
    assert!(out.is_error, "{}", out.raw);
    assert_eq!(recorder::runs(&root(&w)), 0);
    assert!(w.b.rig.calls_of("wf-recorder").is_empty());
    assert!(w.b.rig.calls_of("notify-send").is_empty());
    assert_eq!(artifact_count(&w).await, 0);
}

// ------------------------------------------------------------------ proof 3

/// Proof 3 (§17 item 39): `read_capture_image` refuses the artifact of a
/// recording with `unknown_artifact` and no image block.
#[tokio::test]
async fn the_image_reader_refuses_the_artifact_of_a_recording() {
    let w = world().await;
    let artifact = record(&w, "r1", 1).await.ok()["artifact"].clone();
    let read = w
        .mcp
        .call("read_capture_image", json!({"artifact": artifact}))
        .await;
    assert_eq!(read.code(), "unknown_artifact", "{}", read.raw);
    assert!(read.content.iter().all(|b| b["type"] == "text"));
}

// ------------------------------------------------------------------ proof 4

/// Proof 4 (§17 item 6): the owner stops an agent recording on the board. The
/// agent gets `committed` with `owner_stop`; the seconds stay charged.
#[tokio::test]
async fn the_owner_stops_an_agent_recording_and_the_seconds_stay_charged() {
    let w = world().await;
    let run = start_held(&w, "r1", 3).await;
    recorder::release(&root(&w));
    recorder::written(&root(&w)).await;
    let headers = owner_headers(&run.c, &run.h);
    let stop = run
        .c
        .post(
            &format!(
                "/api/stations/beta/desktop/captures/{}/stop",
                run.capture_id
            ),
            &refs(&headers),
            json!({}),
        )
        .await;
    assert!(
        stop.status == 200 || stop.status == 202,
        "{} {}",
        stop.status,
        stop.body
    );
    let out = run.call.await.unwrap();
    let result = out.ok();
    assert_eq!(result["state"], "committed", "{}", out.raw);
    assert_eq!(result["stop_reason"], "owner_stop");
    assert_eq!(result["capture_id"], run.capture_id.as_str());
    assert_eq!(used(&w).await, (1, 1, 1, 3));
}

// ------------------------------------------------------------------ proof 5

/// Proof 5 (§17 item 21): the owner revokes the grant during the recording.
/// The agent gets `authority_lost`; no artifact; no live recorder; no refund.
#[tokio::test]
async fn a_grant_revoked_during_the_recording_ends_it_with_authority_lost() {
    let w = world().await;
    let run = start_held(&w, "r1", 3).await;
    revoke(&run.c, &run.h, &w.grant_id).await;
    let out = run.call.await.unwrap();
    assert_eq!(out.code(), "authority_lost", "{}", out.raw);
    assert_eq!(artifact_count(&w).await, 0);
    recorder_gone(&w).await;
    assert_eq!(used(&w).await.2, 1);
}

// ------------------------------------------------------------------ proof 6

/// Proof 6 (§17 item 21): the caller A turns fleet control off during the
/// recording. The agent gets `authority_lost`; no artifact.
#[tokio::test]
async fn a_caller_that_turns_fleet_control_off_during_the_recording_loses_it() {
    let w = world().await;
    let run = start_held(&w, "r1", 3).await;
    w.set_a_fleet(false);
    let out = run.call.await.unwrap();
    assert_eq!(out.code(), "authority_lost", "{}", out.raw);
    assert_eq!(artifact_count(&w).await, 0);
    recorder_gone(&w).await;
}

// ------------------------------------------------------------------ proof 7

/// Proof 7 (§17 item 21): the deck switch `desktop_control` turns off during
/// the recording. The agent gets `authority_lost`; the station latched
/// `deck_policy`; no artifact.
#[tokio::test]
async fn the_deck_switch_that_turns_off_during_the_recording_ends_it_with_deck_policy() {
    let w = world().await;
    let run = start_held(&w, "r1", 3).await;
    w.deck().desktop_control(false);
    let out = run.call.await.unwrap();
    assert_eq!(out.code(), "authority_lost", "{}", out.raw);
    assert_eq!(artifact_count(&w).await, 0);
    let fp = w.identity.fingerprint().to_string();
    let reason =
        w.b.store
            .recording_get(&fp, &run.capture_id)
            .await
            .unwrap()
            .and_then(|r| r.stop_reason);
    assert_eq!(reason, Some(StopReason::DeckPolicy));
    recorder_gone(&w).await;
}

// ------------------------------------------------------------------ proof 8

/// Proof 8 (§17 item 34): a grant with one recording commits the first and
/// refuses the second with `recording_cap_reached`; the second never runs.
#[tokio::test]
async fn the_recording_count_cap_refuses_the_next_recording_before_anything_runs() {
    let w = world_with((1, 600)).await;
    assert_eq!(record(&w, "r1", 1).await.ok()["state"], "committed");
    let second = record(&w, "r2", 1).await;
    assert_eq!(second.code(), "recording_cap_reached", "{}", second.raw);
    assert_eq!(recorder::runs(&root(&w)), 1);
    assert_eq!(w.b.rig.calls_of("notify-send").len(), 1);
    assert_eq!(used(&w).await, (1, 1, 1, 1));
}

/// Proof 8 (§17 item 34): a grant with 2 recording seconds refuses a call of 3
/// seconds with `recording_seconds_reached`; nothing runs on B.
#[tokio::test]
async fn the_recording_seconds_cap_refuses_a_longer_recording_before_anything_runs() {
    let w = world_with((5, 2)).await;
    let out = record(&w, "r1", 3).await;
    assert_eq!(out.code(), "recording_seconds_reached", "{}", out.raw);
    assert_eq!(recorder::runs(&root(&w)), 0);
    assert!(w.b.rig.calls_of("notify-send").is_empty());
    assert_eq!(used(&w).await, (0, 0, 0, 0));
}

/// Proof 8 (§17 item 34): the same request ID with the same arguments after a
/// committed recording is charged once and starts no second recorder.
#[tokio::test]
async fn a_repeated_recording_request_is_charged_once() {
    let w = world().await;
    let first = record(&w, "r1", 1).await.ok();
    assert_eq!(first["state"], "committed");
    let again = record(&w, "r1", 1).await;
    assert_eq!(again.ok(), first, "{}", again.raw);
    assert_eq!(recorder::runs(&root(&w)), 1);
    assert_eq!(used(&w).await, (1, 1, 1, 1));
}

// ------------------------------------------------------------------ proof 9

/// Proof 9 (§17 items 48, 19): the link of B closes during the recording. The
/// agent call answers `desktop_timeout` (the deck cannot know the outcome);
/// B's row is terminal with `authority_unknown` and `link_closed`; after B
/// links again and the deck recovery runs, `capture_status` answers
/// `authority_unknown`; B never runs a second recorder.
#[tokio::test]
async fn a_link_that_closes_during_the_recording_ends_it_and_never_restarts_it() {
    let w = world().await;
    let run = start_held(&w, "r1", 3).await;
    w.b.unlink();
    let out = run.call.await.unwrap();
    // The deck lost the link after `attempted`: it cannot know the outcome.
    assert_eq!(out.code(), "desktop_timeout", "{}", out.raw);
    assert_eq!(artifact_count(&w).await, 0);
    // The station row on B is terminal: `authority_unknown`, `link_closed`.
    let fp = w.identity.fingerprint().to_string();
    eventually("a terminal station row", || async {
        let row = w.b.store.recording_get(&fp, &run.capture_id).await.unwrap();
        row.is_some_and(|r| r.op.state != CaptureState::Running)
    })
    .await;
    let row = w.b.store.recording_get(&fp, &run.capture_id).await.unwrap();
    let row = row.expect("a station row");
    assert_eq!(row.op.state, CaptureState::Failed, "{row:?}");
    assert_eq!(row.op.code.as_deref(), Some("authority_unknown"));
    assert_eq!(row.stop_reason, Some(StopReason::LinkClosed));
    // The capture recovery of the deck (the production deck starts it in
    // `serve.rs`; the test deck does not), then B links again.
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let recovery = fleetdeck_deck::desktop_capture::spawn_with(
        w.deck().store.clone(),
        std::sync::Arc::clone(w.deck().node.link()),
        shutdown,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(2),
    );
    w.b.link(w.deck().addr, w.identity.public_key(), &[]).await;
    let (store, b) = (w.deck().store.clone(), w.b_id.clone());
    eventually("B links again", || {
        let (store, b) = (store.clone(), b.clone());
        async move {
            store.is_reachable(&b)
                && store.link_feature(&b, feature::CAPTURE_RECORDING_MCP) == LinkFeature::Negotiated
        }
    })
    .await;
    // The status tells the terminal code of the station, with the capture ID:
    // a `failed` state whose `error` is `authority_unknown`. The poll is slow,
    // because the status tool has a rate limit.
    let answer: std::sync::Mutex<Value> = std::sync::Mutex::default();
    eventually("the status authority_unknown", || async {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let call = w
            .mcp
            .call("capture_status", json!({"request_id": "r1"}))
            .await;
        let body = if call.content.is_empty() {
            Value::Null
        } else {
            call.json()
        };
        let done = body["error"] == "authority_unknown";
        *answer.lock().unwrap() = body;
        done
    })
    .await;
    let answer = answer.lock().unwrap().clone();
    assert_eq!(answer["state"], "failed", "{answer}");
    assert_eq!(answer["capture_id"], run.capture_id.as_str(), "{answer}");
    assert_eq!(answer["request_id"], "r1", "{answer}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(recorder::runs(&root(&w)), 1);
    assert_eq!(artifact_count(&w).await, 0);
    recorder_gone(&w).await;
    stop.send(true).unwrap();
    recovery.await.unwrap();
}

// ----------------------------------------------------------------- proof 10

/// The elevated owner start of a coordinator session on `alpha` with a grant
/// of the `capture` class over B. `command_id` names the start.
async fn start(
    w: &World,
    (c, h, elevation): (&BoardClient, &Headers, &str),
    command_id: &str,
    allow_recording: bool,
) -> Http {
    let mut coordinator = json!({
        "grant_id": "", "classes": ["capture"], "stations": [w.b_id.as_str()],
        "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}});
    if allow_recording {
        coordinator["allow_recording"] = json!(true);
    }
    let body = json!({"station": "alpha", "command_id": command_id, "elevation": elevation,
        "command": {"op": "start", "prompt": null, "spec": {
            "provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
            "title": null, "coordinator": coordinator}}});
    settings_support::post(c, h, "/api/commands/session.start", body).await
}

/// The first object under the key `coordinator` of a JSON value.
fn coordinator_of(v: &Value) -> Option<Value> {
    match v {
        Value::Object(map) => map
            .get("coordinator")
            .cloned()
            .or_else(|| map.values().find_map(coordinator_of)),
        Value::Array(a) => a.iter().find_map(coordinator_of),
        _ => None,
    }
}

/// Proof 10 (§14 item 13, §17 item 44): a target without
/// `capture_recording_mcp.v1` cannot be a station of a recording grant. The
/// same start without the allowance is accepted, and the start that A receives
/// has no recording field.
#[tokio::test]
async fn a_target_without_the_recording_feature_cannot_be_a_recording_grant_station() {
    let w = World::with(Setup {
        b_without: vec![feature::CAPTURE_RECORDING_MCP],
        grant: false,
        ..Setup::default()
    })
    .await;
    // The refusal is about B: A has the feature, B has not.
    let store = &w.deck().store;
    eventually("the feature sets of both links", || async {
        store.link_feature(&w.a_id, feature::CAPTURE_RECORDING_MCP) == LinkFeature::Negotiated
            && store.link_feature(&w.b_id, feature::CAPTURE_RECORDING_MCP) == LinkFeature::Without
    })
    .await;
    let (c, h, elevation) = w.deck().board_elevated().await;
    let refused = start(&w, (&c, &h, &elevation), "grant-1", true).await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, "unsupported"),
        "{}",
        refused.body
    );
    assert!(store.grants(10).await.unwrap().is_empty());

    let passed = start(&w, (&c, &h, &elevation), "grant-2", false).await;
    assert_eq!(passed.status, 200, "{}", passed.body);
    assert_eq!(store.grants(10).await.unwrap().len(), 1);
    let sent = store
        .commands()
        .await
        .unwrap()
        .into_iter()
        .find(|cmd| cmd.command_id.as_str() == "grant-2")
        .expect("the stored start command");
    let spec = coordinator_of(&sent.envelope).expect("a coordinator spec in the start");
    for key in ["allow_recording", "recording_cap", "recording_second_cap"] {
        assert!(spec.get(key).is_none(), "{key}: {spec}");
    }
}
