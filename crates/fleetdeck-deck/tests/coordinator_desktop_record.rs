//! The P6 deck side of coordinator recordings (R13 P5b-2 task Q-K): the
//! `record_monitor` call with its authorization, its admission and its
//! charges, and the run with the recording record. The lease, the provenance
//! and the status are in `coordinator_desktop_record/lease.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;
#[path = "coordinator_desktop_record/lease.rs"]
mod lease;

use std::time::Duration;

use coordinator_desktop_support::*;
use fleetdeck_deck::desktop_capture::recording::OperationKind;
use fleetdeck_deck::store::coordinator_desktop::OpState;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

const UNKNOWN_TEXT: &str =
    "The recording result is unknown. Check the artifact list before starting another recording.";

/// The next rolling minute: two captures per minute is the rate.
fn next_minute(w: &World) {
    w.f.det.advance(Duration::from_secs(61));
}

fn target_set() -> Vec<String> {
    [
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
        feature::DESKTOP_CAPTURE,
        feature::ARTIFACTS_DELETE,
        feature::CAPTURE_MCP,
    ]
    .map(str::to_owned)
    .to_vec()
}

#[tokio::test]
async fn a_grant_without_the_allowance_and_a_missing_feature_are_refused_before_anything_is_stored()
{
    // No recording allowance: both caps are zero.
    let w = world_with(&BOTH, |_| {}).await;
    w.fake.set(rec_station(RecSpec::of(&w, 30)));
    let r = w.call("c1", record_call("r1", 30, None)).await;
    assert_eq!(code(&r), "not_granted");
    assert_eq!(counters(&w).await, (0, 0, 0, 0));
    assert_eq!(w.fake.count("desktop.act"), 0);
    // A grant of the desktop class only has no recording either.
    let w = world().await;
    let r = w.call("c1", record_call("r1", 30, None)).await;
    assert_eq!(code(&r), "not_granted");
    // The caller link has no `capture_recording_mcp.v1`.
    let w = record_world(5, 600).await;
    let mut caller = target_set();
    caller.push(feature::COORDINATOR.to_owned());
    w.f.store.set_link_features(&w.f.station, 98, caller);
    let r = w.call("c1", record_call("r1", 30, None)).await;
    assert_eq!(code(&r), "desktop_unsupported");
    assert_eq!(counters(&w).await, (0, 0, 0, 0));
    // The target link has the capture features but no recording feature.
    let mut w = record_world(5, 600).await;
    let beta_fake = w.link_beta().await;
    w.f.store.set_link_features(&w.beta, 98, target_set());
    let beta = w.beta.clone();
    let r = w
        .call("c2", record_call("r2", 30, Some(beta.as_str())))
        .await;
    assert_eq!(code(&r), "desktop_unsupported");
    assert_eq!(beta_fake.count("desktop.act"), 0);
    assert_eq!(counters(&w).await, (0, 0, 0, 0));
}

#[tokio::test]
async fn a_recording_is_sent_once_with_the_coordinator_authority_and_indexed_with_its_provenance() {
    let w = rich_world().await;
    let (reply, id) = committed_recording(&w, "req-1", 30).await;
    let st = w.station();
    let want = json!({"request_id": "req-1", "state": "committed", "capture_id": id,
        "station": st.as_str(),
        "artifact": {"station_id": st.as_str(), "artifact_id": REC_ART, "version": 1},
        "monitor": "DP-1", "size": 4096, "sha256": cap_sha(b"video"), "at": 1000,
        "requested_seconds": 30, "duration_ms": 30_000, "stop_reason": "duration"});
    assert_eq!(reply, json!({"status": "ok", "result": want}));
    // The policies, in this order, with the capture purpose and the monitor.
    let methods: Vec<String> = w.fake.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        methods,
        [
            "coordinator.policy_now",
            "desktop.policy_now",
            "desktop.act"
        ]
    );
    assert_eq!(
        w.fake.params_of("desktop.policy_now")[0],
        json!({"purpose": "capture", "monitor": "DP-1"})
    );
    let act = &w.fake.params_of("desktop.act")[0];
    assert_eq!(act["action"], "screen_record");
    assert_eq!(act["monitor"], "DP-1");
    assert_eq!(act["seconds"], 30);
    assert_eq!(act["authority"]["action_id"], id.as_str());
    // The deck record: a recording with the coordinator column, no login.
    let r = rec_row(&w, &id, &st).await;
    let context = serde_json::to_string(&w.context()).unwrap();
    assert_eq!(r.capture.coordinator.as_deref(), Some(context.as_str()));
    assert_eq!(r.kind, OperationKind::Recording);
    assert_eq!(r.requested_seconds, Some(30));
    assert_eq!(r.owner_login, None);
    assert!(r.link_epoch.is_some());
    assert_eq!(
        (r.capture.state, r.capture.indexed),
        (CaptureState::Committed, true)
    );
    let prov = published_of(&r.capture)
        .version
        .capture
        .expect("a provenance");
    assert_eq!(prov.caller, CaptureCaller::Coordinator);
    assert_eq!(prov.kind, CaptureKind::Recording);
    assert_eq!(prov.caller_station.as_deref(), Some(st.as_str()));
    assert_eq!(prov.caller_session.as_deref(), Some("coord-1"));
    assert_eq!(prov.caller_grant_id.as_deref(), Some(w.b.grant_id.as_str()));
    assert_eq!(prov.station_id, st.as_str());
    assert_eq!(prov.recording.expect("a meta").duration_ms, 30_000);
    // The operation is done and charged for the recording.
    let o = cap_op(&w, "req-1").await;
    assert_eq!(
        (o.state, o.operation_id.as_str()),
        (OpState::Done, id.as_str())
    );
    assert_eq!(o.seconds, Some(30));
    assert!(!format!("{o:?}").contains("req-1"), "no raw request id");
    // The audit rows of `desktop.capture`: the attempt with the charged
    // counters, and the outcome with the artifact and the stop reason.
    let rows = w.audit_rows(Some("desktop.capture")).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let a: Value = serde_json::from_str(&rows[0].2).unwrap();
    let b: Value = serde_json::from_str(&rows[1].2).unwrap();
    assert_eq!(
        (a["stage"].as_str(), b["stage"].as_str()),
        (Some("attempt"), Some("outcome"))
    );
    assert_eq!(a["capture_id"], id.as_str());
    assert_eq!(
        (&a["kind"], &a["seconds"]),
        (&json!("recording"), &json!(30))
    );
    assert_eq!(
        a["charged"],
        json!({"desktop": 1, "capture": 1, "recording": 1, "recording_seconds": 30})
    );
    assert_eq!(b["kind"], "recording");
    assert_eq!(b["artifact_id"], REC_ART);
    assert_eq!(b["stop_reason"], "duration");
    assert!(!w.audit_text().await.contains("req-1"));
    // A duplicate replays the capture state and sends nothing.
    let again = w.call("c2", record_call("req-1", 30, None)).await;
    assert_eq!(again, reply);
    assert_eq!(w.fake.count("desktop.act"), 1);
    assert_eq!(counters(&w).await, (1, 1, 1, 30));
}

#[tokio::test]
async fn acceptance_charges_every_counter_and_the_two_caps_refuse_with_a_stored_code() {
    // The recording count.
    let w = record_world(2, 600).await;
    w.fake.set(rec_station(RecSpec::of(&w, 30)));
    let (_, id) = committed_recording(&w, "r1", 30).await;
    assert_eq!(
        charged_json(&w, &id).await,
        json!({"desktop": 1, "capture": 1, "recording": 1, "recording_seconds": 30})
    );
    assert_eq!(counters(&w).await, (1, 1, 1, 30));
    next_minute(&w);
    committed_recording(&w, "r2", 40).await;
    assert_eq!(counters(&w).await, (2, 2, 2, 70));
    next_minute(&w);
    let refused = w.call("c3", record_call("r3", 10, None)).await;
    assert_eq!(code(&refused), "recording_cap_reached");
    assert_eq!(
        refused["message"],
        "This grant has used its recording limit."
    );
    assert_eq!(counters(&w).await, (2, 2, 2, 70));
    // The refusal is stored: the same request ID answers the same code.
    let again = w.call("c4", record_call("r3", 10, None)).await;
    assert_eq!(code(&again), "recording_cap_reached");
    assert_eq!(w.fake.count("desktop.act"), 2);
    // The recording seconds.
    let w = record_world(10, 50).await;
    committed_recording(&w, "r1", 30).await;
    next_minute(&w);
    let refused = w.call("c2", record_call("r2", 30, None)).await;
    assert_eq!(code(&refused), "recording_seconds_reached");
    assert_eq!(
        refused["message"],
        "This grant has used its recording time limit."
    );
    assert_eq!(counters(&w).await, (1, 1, 1, 30));
    let replay = w.call("c3", record_call("r2", 30, None)).await;
    assert_eq!(code(&replay), "recording_seconds_reached");
    // Exactly the seconds that are left fit.
    next_minute(&w);
    committed_recording(&w, "r3", 20).await;
    assert_eq!(counters(&w).await, (2, 2, 2, 50));
    next_minute(&w);
    let none_left = w.call("c5", record_call("r4", 1, None)).await;
    assert_eq!(code(&none_left), "recording_seconds_reached");
}

#[tokio::test]
async fn a_recording_shares_the_capture_cap_and_the_capture_rate_with_a_screenshot() {
    // The capture cap is shared: one screenshot uses it, a recording is refused
    // with the capture-limit text and charges nothing.
    let w = world_with(&BOTH, |g| {
        g.capture_cap = Some(1);
        g.recording_cap = 5;
        g.recording_second_cap = 600;
    })
    .await;
    w.fake.set(shot_station(
        ShotSpec::of(&w, &cap_png(300)),
        reads_of(cap_png(300)),
    ));
    assert_eq!(w.call("c1", shot_call("s1", None)).await["status"], "ok");
    w.fake.set(rec_station(RecSpec::of(&w, 30)));
    let refused = w.call("c2", record_call("r1", 30, None)).await;
    assert_eq!(code(&refused), "capture_cap_reached");
    assert_eq!(refused["message"], "This grant has used its capture limit.");
    assert_eq!(counters(&w).await, (1, 1, 0, 0));
    // The screenshot answer of the same code is unchanged.
    let shot = w.call("c3", shot_call("s2", None)).await;
    assert_eq!(code(&shot), "capture_cap_reached");
    assert_eq!(shot["message"], "This grant has used its screenshot limit.");
    // The rate: a screenshot and a recording fill the two places of a minute.
    let w = rich_world().await;
    w.fake.set(shot_station(
        ShotSpec::of(&w, &cap_png(300)),
        reads_of(cap_png(300)),
    ));
    assert_eq!(w.call("c1", shot_call("s1", None)).await["status"], "ok");
    committed_recording(&w, "r1", 30).await;
    let limited = w.call("c2", record_call("r2", 30, None)).await;
    assert_eq!(code(&limited), "rate_limited");
    assert_eq!(counters(&w).await, (2, 2, 1, 30));
    // And the other way round: two recordings stop a screenshot.
    next_minute(&w);
    committed_recording(&w, "r3", 10).await;
    committed_recording(&w, "r4", 10).await;
    w.fake.set(shot_station(
        ShotSpec::of(&w, &cap_png(300)),
        reads_of(cap_png(300)),
    ));
    let limited = w.call("c3", shot_call("s3", None)).await;
    assert_eq!(code(&limited), "rate_limited");
    next_minute(&w);
    committed_recording(&w, "r5", 10).await;
}

#[tokio::test]
async fn a_station_failure_is_a_failed_record_with_its_terminal_code_and_nothing_is_refunded() {
    let w = rich_world().await;
    let recording_failed = "The recording tool failed.";
    let cases = [
        (
            "recording_failed",
            "x",
            "recording_failed",
            recording_failed,
        ),
        (
            "screen_locked",
            "x",
            "screen_locked",
            "The screen is locked or its lock state is unknown.",
        ),
        (
            "disk_full",
            "x",
            "disk_full",
            "The station has too little free space for a capture.",
        ),
        (
            "recording_too_large",
            "x",
            "recording_too_large",
            "The recording exceeds 256 MiB.",
        ),
        (
            "desktop_not_found",
            "x",
            "desktop_not_found",
            "The target of the desktop action was not found.",
        ),
        (
            "authority_lost",
            "STATION-TEXT",
            "authority_lost",
            "Desktop authority was withdrawn before this operation could continue.",
        ),
        (
            "authority_unknown",
            "STATION-TEXT",
            "authority_unknown",
            "Desktop authority could not be checked. No further desktop work was started.",
        ),
        (
            "desktop_read_timeout",
            "STATION-TEXT",
            "desktop_read_timeout",
            "The desktop data did not finish loading in time.",
        ),
        (
            "unsupported",
            "The recording tool wf-recorder is not installed.",
            "desktop_unsupported",
            "The recording tool wf-recorder is not installed.",
        ),
    ];
    for (i, (wire, message, want, text)) in cases.into_iter().enumerate() {
        next_minute(&w);
        w.fake
            .set(except("desktop.act", move |_| Some(fail(wire, message))));
        let rid = format!("r{i}");
        let reply = w.call(&format!("c{i}"), record_call(&rid, 30, None)).await;
        assert_eq!(code(&reply), want, "{wire}");
        assert_eq!(reply["message"], text, "{wire}");
        assert!(!reply.to_string().contains("STATION-TEXT"));
        let o = cap_op(&w, &rid).await;
        assert_eq!(reply["capture_id"], o.operation_id.as_str());
        assert_eq!((o.state, o.code.as_deref()), (OpState::Failed, Some(want)));
        let r = rec_row(&w, &o.operation_id, &w.station()).await;
        assert_eq!(r.capture.state, CaptureState::Failed, "{wire}");
        // The status tool tells the same, with the requested seconds.
        let s = w.query(capture_status_query(&rid)).await;
        assert_eq!(s["result"]["state"], "failed");
        assert_eq!(
            (
                s["result"]["error"].as_str(),
                s["result"]["message"].as_str(),
                s["result"]["requested_seconds"].as_u64()
            ),
            (Some(want), Some(text), Some(30)),
            "{wire}"
        );
        // No refund after a failure.
        let n = u32::try_from(i + 1).unwrap();
        assert_eq!(counters(&w).await, (n, n, n, 30 * n), "{wire}");
    }
    // A code that is no recording code: mapped as a relay code, and the
    // record stays attempted (the station may have run).
    next_minute(&w);
    w.fake
        .set(except("desktop.act", |_| Some(fail("forbidden", "x"))));
    let reply = w.call("cf", record_call("rf", 30, None)).await;
    assert_eq!(code(&reply), "desktop_forbidden");
    let o = cap_op(&w, "rf").await;
    assert_eq!(o.state, OpState::Failed);
    let r = rec_row(&w, &o.operation_id, &w.station()).await;
    assert_eq!(r.capture.state, CaptureState::Attempted);
}

#[tokio::test]
async fn a_lost_link_after_the_send_is_unknown_and_the_record_stays_attempted() {
    let mut w = record_world(5, 600).await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    let reply = w
        .call("c1", record_call("r1", 30, Some(beta.as_str())))
        .await;
    assert_eq!(code(&reply), "desktop_timeout");
    let o = cap_op(&w, "r1").await;
    assert_eq!(reply["capture_id"], o.operation_id.as_str());
    assert_eq!(o.state, OpState::Unknown);
    let r = rec_row(&w, &o.operation_id, &beta).await;
    assert_eq!(r.capture.state, CaptureState::Attempted);
    assert_eq!(counters(&w).await, (1, 1, 1, 30));
    // Pending, until the recovery or the 24 h; never a second recording.
    let s = w.query(capture_status_query("r1")).await;
    assert_eq!(s["result"]["state"], "pending");
    assert_eq!(s["result"]["requested_seconds"], 30);
    w.f.store
        .capture_unknown(beta.clone(), o.operation_id.clone())
        .await
        .unwrap();
    let s = w.query(capture_status_query("r1")).await;
    assert_eq!(s["result"]["state"], "unknown");
    assert_eq!(s["result"]["message"], UNKNOWN_TEXT);
    assert_eq!(beta_fake.count("desktop.act"), 1);
}
