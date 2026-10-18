//! The lease, the provenance and the status of coordinator recordings (R13
//! P5b-2 task Q-K): the continuation lease of a coordinator recording with
//! its scripted station, the coordinator recording provenance and its
//! rejections, the recovery of a recording, `capture_status` with the
//! recording fields, and `read_capture_image` of a recording.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use crate::coordinator_desktop_support::*;
use fleetdeck_deck::store::coordinator_desktop::OpState;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::capture::{CaptureCaller, CaptureStatus};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{CaptureLeaseAnswer, LeaseDecision, LeasePhase, StopReason};
use serde_json::{Value, json};

const SAVED_TEXT: &str =
    "The recording was saved on the station. The artifact list is still updating.";

/// A change of the `published` JSON of the station.
type Edit = fn(&mut Value);

/// The next rolling minute: two captures per minute is the rate.
fn next_minute(w: &World) {
    w.f.det.advance(Duration::from_secs(61));
}

pub(super) fn lease_answer(o: &Outcome) -> (LeaseDecision, Option<StopReason>) {
    let Outcome::Ok(v) = o else {
        panic!("a lease answer, got {o:?}");
    };
    let a: CaptureLeaseAnswer = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(a.sequence, 7);
    (a.decision, a.reason)
}

fn forbidden(o: &Outcome) -> bool {
    matches!(o, Outcome::Err(e) if e.code == "forbidden")
}

/// A hook that runs `sql` on the deck database.
pub(super) fn sql(w: &World, sql: String) -> Step {
    let store = w.f.store.clone();
    hook(move || {
        let (store, sql) = (store.clone(), sql.clone());
        async move {
            store
                .db()
                .call(move |conn| Ok(conn.execute(&sql, [])?))
                .await
                .unwrap();
        }
    })
}

/// Runs a recording with the lease script; the reply and the lease answers.
pub(super) async fn scripted(w: &World, steps: Vec<Step>, end: Final) -> (Value, Vec<Outcome>) {
    let answers = Answers::default();
    w.fake.set(script_station(steps, end, answers.clone()));
    let reply = w.call("c1", record_call("r1", 30, None)).await;
    let got = answers.lock().unwrap().clone();
    (reply, got)
}

pub(super) const LOST: Final = Final::Err("authority_lost", "x");

#[tokio::test]
async fn a_valid_lease_continues_sets_the_start_once_and_audits_it_as_the_coordinator() {
    let w = rich_world().await;
    let steps = vec![
        Step::Lease(LeasePhase::Start),
        Step::Lease(LeasePhase::Record),
        Step::Lease(LeasePhase::Record),
        Step::Lease(LeasePhase::Import),
    ];
    let end = Final::Ok(RecSpec::of(&w, 30));
    let (reply, got) = scripted(&w, steps, end).await;
    assert_eq!(reply["result"]["state"], "committed", "{reply}");
    assert_eq!(got.len(), 4);
    for o in &got {
        assert_eq!(lease_answer(o), (LeaseDecision::Continue, None));
    }
    let id = reply["result"]["capture_id"].as_str().unwrap();
    let r = rec_row(&w, id, &w.station()).await;
    assert!(r.started_at.is_some());
    // The caller policy was asked for each lease, with the current binding.
    assert_eq!(w.fake.count("coordinator.policy_now"), 5);
    let stages: Vec<String> = w
        .audit_rows(Some("desktop.capture"))
        .await
        .into_iter()
        .map(|r| {
            let d: Value = serde_json::from_str(&r.2).unwrap();
            d["stage"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(stages, ["attempt", "started", "outcome"]);
    // No owner row: every recording row of this call is the coordinator's.
    let owner: Vec<_> =
        w.f.store
            .audit_log(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.actor == "owner" && e.action.starts_with("desktop."))
            .collect();
    assert!(owner.is_empty(), "{owner:?}");
}

#[tokio::test]
async fn a_lease_after_a_loss_of_the_deck_switches_discards_with_deck_policy() {
    // The fleet-control switch.
    let w = rich_world().await;
    let store = w.f.store.clone();
    let off = hook(move || {
        let store = store.clone();
        async move { store.set_fleet_control(false) }
    });
    let steps = vec![
        Step::Lease(LeasePhase::Start),
        off,
        Step::Lease(LeasePhase::Record),
    ];
    let (reply, got) = scripted(&w, steps, LOST).await;
    assert_eq!(lease_answer(&got[0]), (LeaseDecision::Continue, None));
    assert_eq!(
        lease_answer(&got[1]),
        (LeaseDecision::Discard, Some(StopReason::DeckPolicy))
    );
    assert_eq!(code(&reply), "authority_lost");
    // The desktop-control switch.
    let w = rich_world().await;
    let live = w.live.clone();
    let off = hook(move || {
        let live = live.clone();
        async move { live.publish(deck_live(false)) }
    });
    let (_, got) = scripted(&w, vec![off, Step::Lease(LeasePhase::Record)], LOST).await;
    assert_eq!(
        lease_answer(&got[0]),
        (LeaseDecision::Discard, Some(StopReason::DeckPolicy))
    );
}

#[tokio::test]
async fn a_lease_after_the_loss_of_the_grant_discards_with_coordinator_authority_lost() {
    let lost = (
        LeaseDecision::Discard,
        Some(StopReason::CoordinatorAuthorityLost),
    );
    // A revoke during the recording: the call ends with `authority_lost`.
    let w = rich_world().await;
    let store = w.f.store.clone();
    let gid = w.b.grant_id.clone();
    let revoke = hook(move || {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            store.revoke_grant(gid, "test".into()).await.unwrap();
        }
    });
    let steps = vec![
        Step::Lease(LeasePhase::Start),
        revoke,
        Step::Lease(LeasePhase::Record),
    ];
    let (reply, got) = scripted(&w, steps, LOST).await;
    assert_eq!(lease_answer(&got[0]), (LeaseDecision::Continue, None));
    assert_eq!(lease_answer(&got[1]), lost);
    assert_eq!(code(&reply), "authority_lost");
    let o = cap_op(&w, "r1").await;
    assert_eq!(
        (o.state, o.code.as_deref()),
        (OpState::Failed, Some("authority_lost"))
    );
    let r = rec_row(&w, &o.operation_id, &w.station()).await;
    assert_eq!(r.capture.state, CaptureState::Failed);
    assert_eq!(r.capture.code.as_deref(), Some("authority_lost"));
    // A binding of another generation.
    let w = rich_world().await;
    let gid = w.b.grant_id.clone();
    let rebind = sql(
        &w,
        format!("UPDATE tokens SET bound_generation = 2 WHERE grant_id = '{gid}'"),
    );
    let (_, got) = scripted(&w, vec![rebind, Step::Lease(LeasePhase::Record)], LOST).await;
    assert_eq!(lease_answer(&got[0]), lost);
    // An expired grant.
    let w = rich_world().await;
    let gid = w.b.grant_id.clone();
    let expire = sql(
        &w,
        format!("UPDATE tokens SET expires_at = 1 WHERE grant_id = '{gid}'"),
    );
    let (_, got) = scripted(&w, vec![expire, Step::Lease(LeasePhase::Record)], LOST).await;
    assert_eq!(lease_answer(&got[0]), lost);
}

#[tokio::test]
async fn a_lease_past_the_deck_deadline_discards_with_deadline() {
    let w = rich_world().await;
    // 30 s plus 35 s: the `attempted` commit is 100 s old.
    let old = sql(
        &w,
        "UPDATE desktop_captures SET updated_at = updated_at - 100000".to_owned(),
    );
    let (_, got) = scripted(&w, vec![old, Step::Lease(LeasePhase::Record)], LOST).await;
    assert_eq!(
        lease_answer(&got[0]),
        (LeaseDecision::Discard, Some(StopReason::Deadline))
    );
}

#[tokio::test]
async fn the_caller_policy_is_asked_for_each_lease_false_discards_and_uncertain_is_forbidden() {
    let w = rich_world().await;
    let steps = vec![
        Step::Caller("false"),
        Step::Lease(LeasePhase::Record),
        Step::Caller("uncertain"),
        Step::Lease(LeasePhase::Record),
        Step::Caller("ok"),
        Step::Lease(LeasePhase::Record),
    ];
    let (_, got) = scripted(&w, steps, LOST).await;
    assert_eq!(
        lease_answer(&got[0]),
        (
            LeaseDecision::Discard,
            Some(StopReason::CoordinatorAuthorityLost)
        )
    );
    assert!(forbidden(&got[1]), "{:?}", got[1]);
    assert_eq!(lease_answer(&got[2]), (LeaseDecision::Continue, None));
    // The target policy is the one of the owner recording: false is forbidden.
    let w = rich_world().await;
    let steps = vec![Step::Target("unlocked"), Step::Lease(LeasePhase::Record)];
    let (_, got) = scripted(&w, steps, LOST).await;
    assert!(forbidden(&got[0]), "{:?}", got[0]);
}

#[tokio::test]
async fn a_coordinator_recording_is_never_indexed_with_another_provenance_and_each_rejection_is_pending()
 {
    let w = rich_world().await;
    let accepted: [(&str, Edit); 2] = [
        ("owner stop", |v| {
            v["version"]["capture"]["recording"]["stop_reason"] = json!("owner_stop");
            v["version"]["capture"]["recording"]["duration_ms"] = json!(12_000);
        }),
        ("one second more", |v| {
            v["version"]["capture"]["recording"]["duration_ms"] = json!(31_000);
        }),
    ];
    let mut n = 0;
    for (what, tweak) in accepted {
        next_minute(&w);
        n += 1;
        // Each capture of the station has its own artifact.
        let mut spec = RecSpec::of(&w, 30).tweaked(tweak);
        spec.artifact = format!("ARTACC{n}");
        w.fake.set(rec_station(spec));
        let reply = w
            .call(&format!("a{n}"), record_call(&format!("ra{n}"), 30, None))
            .await;
        assert_eq!(reply["result"]["state"], "committed", "{what}: {reply}");
    }
    let rejected: [(&str, Edit); 13] = [
        ("an owner caller", |v| {
            v["version"]["capture"]["caller"] = json!("owner");
        }),
        ("another grant", |v| {
            v["version"]["capture"]["caller_grant_id"] = json!("other-grant");
        }),
        ("another session", |v| {
            v["version"]["capture"]["caller_session"] = json!("other");
        }),
        ("another caller station", |v| {
            v["version"]["capture"]["caller_station"] = json!("other");
        }),
        ("no provenance", |v| {
            v["version"]["capture"] = Value::Null;
        }),
        ("no meta", |v| {
            v["version"]["capture"]
                .as_object_mut()
                .unwrap()
                .remove("recording");
        }),
        ("other requested seconds", |v| {
            v["version"]["capture"]["recording"]["requested_seconds"] = json!(31);
        }),
        ("too long a duration", |v| {
            v["version"]["capture"]["recording"]["duration_ms"] = json!(31_001);
        }),
        ("a discard stop reason", |v| {
            v["version"]["capture"]["recording"]["stop_reason"] = json!("lease_expired");
        }),
        ("odd dimensions", |v| {
            v["version"]["capture"]["recording"]["width"] = json!(1921);
        }),
        ("a screenshot kind", |v| {
            v["version"]["capture"]["kind"] = json!("screenshot");
        }),
        ("another capture", |v| {
            v["version"]["capture"]["capture_id"] = json!("OTHER");
        }),
        ("another monitor", |v| {
            v["version"]["capture"]["monitor"] = json!("DP-2");
        }),
    ];
    // The two accepted recordings are indexed; the rejected ones add nothing.
    let artifacts = count(&w, "SELECT COUNT(*) FROM artifacts").await;
    let versions = count(&w, "SELECT COUNT(*) FROM artifact_versions").await;
    assert_eq!((artifacts, versions), (2, 2));
    let mut failed_rows = 0;
    for (what, tweak) in rejected {
        next_minute(&w);
        n += 1;
        let rid = format!("rr{n}");
        w.fake.set(rec_station(RecSpec::of(&w, 30).tweaked(tweak)));
        let reply = w.call(&format!("b{n}"), record_call(&rid, 30, None)).await;
        // The station committed: the call is not a failure, it is pending.
        assert_eq!(reply["result"]["state"], "pending", "{what}: {reply}");
        assert_eq!(reply["result"]["message"], SAVED_TEXT, "{what}");
        assert_eq!(reply["result"]["requested_seconds"], 30, "{what}");
        let o = cap_op(&w, &rid).await;
        let r = rec_row(&w, &o.operation_id, &w.station()).await;
        assert_eq!(
            (r.capture.state, r.capture.indexed),
            (CaptureState::Committed, false),
            "{what}"
        );
        assert_eq!(
            count(&w, "SELECT COUNT(*) FROM artifacts").await,
            artifacts,
            "{what}"
        );
        assert_eq!(
            count(&w, "SELECT COUNT(*) FROM artifact_versions").await,
            versions,
            "{what}"
        );
        // The status tells pending with its text, never failed.
        let s = w.query(capture_status_query(&rid)).await;
        assert_eq!(s["result"]["state"], "pending", "{what}");
        assert_eq!(s["result"]["message"], SAVED_TEXT);
        failed_rows += 1;
    }
    let indexing: Vec<String> = w
        .audit_rows(Some("desktop.capture"))
        .await
        .into_iter()
        .map(|r| r.2)
        .filter(|d| d.contains("indexing_failed"))
        .collect();
    assert_eq!(indexing.len(), failed_rows);
}

#[tokio::test]
async fn recovery_indexes_a_coordinator_recording_after_a_revoke_and_delivers_nothing() {
    let mut w = record_world(5, 600).await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    assert_eq!(
        code(
            &w.call("c1", record_call("r1", 30, Some(beta.as_str())))
                .await
        ),
        "desktop_timeout"
    );
    let o = cap_op(&w, "r1").await;
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    let r = rec_row(&w, &o.operation_id, &beta).await;
    let mut spec = RecSpec::of(&w, 30);
    spec.station = w.station().to_string();
    let mut published = spec.published(&o.operation_id);
    published["artifact"]["station_id"] = json!("beta-side");
    let status = CaptureStatus::Committed {
        capture_id: o.operation_id.clone(),
        published: serde_json::from_value(published).unwrap(),
    };
    fleetdeck_deck::desktop_capture::reconcile(&w.f.store, &r.capture, status)
        .await
        .unwrap();
    let after = rec_row(&w, &o.operation_id, &beta).await;
    assert_eq!(
        (after.capture.state, after.capture.indexed),
        (CaptureState::Committed, true)
    );
    let prov = published_of(&after.capture).version.capture.unwrap();
    assert_eq!(prov.caller, CaptureCaller::Coordinator);
    assert_eq!(prov.caller_grant_id.as_deref(), Some(w.b.grant_id.as_str()));
    assert!(prov.recording.is_some());
    // The operation row is unchanged, and the revoked caller sees nothing.
    assert_eq!(cap_op(&w, "r1").await.state, OpState::Unknown);
    assert_eq!(
        code(&w.query(capture_status_query("r1")).await),
        "not_granted"
    );
    assert_eq!(
        code(&w.query(image_query(beta.as_str(), REC_ART, 1)).await),
        "not_granted"
    );
}

#[tokio::test]
async fn capture_status_answers_a_recording_with_its_fields_and_the_action_status_hides_it() {
    let w = rich_world().await;
    let (reply, id) = committed_recording(&w, "req-1", 45).await;
    let s = w.query(capture_status_query("req-1")).await;
    assert_eq!(s, reply);
    let result = &s["result"];
    assert_eq!(result["requested_seconds"], 45);
    assert_eq!(result["duration_ms"], 45_000);
    assert_eq!(result["stop_reason"], "duration");
    assert_eq!(result["capture_id"], id.as_str());
    // Never bytes, a path or a token.
    let text = s.to_string();
    for forbidden in ["capture.mp4", "data_base64", "token", "/home"] {
        assert!(!text.contains(forbidden), "{forbidden}: {text}");
    }
    let keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut want = vec![
        "artifact",
        "at",
        "capture_id",
        "duration_ms",
        "monitor",
        "request_id",
        "requested_seconds",
        "sha256",
        "size",
        "state",
        "station",
        "stop_reason",
    ];
    want.sort_unstable();
    let mut got = keys.clone();
    got.sort_unstable();
    assert_eq!(got, want);
    // `desktop_action_status` does not see a recording: the same unknown.
    let other = w.query(status_query("req-1")).await;
    assert_eq!(
        other,
        json!({"status": "ok", "result": {"request_id": "req-1", "state": "unknown"}})
    );
    // A request that no recording of this grant holds is unknown.
    let nope = w.query(capture_status_query("nope")).await;
    assert_eq!(
        nope,
        json!({"status": "ok", "result": {"request_id": "nope", "state": "unknown"}})
    );
}

#[tokio::test]
async fn read_capture_image_refuses_a_recording_with_the_unknown_artifact_answer() {
    let w = rich_world().await;
    let (_, _) = committed_recording(&w, "req-1", 30).await;
    let st = w.station().to_string();
    let none = w.query(image_query(&st, "NOSUCHART", 1)).await;
    assert_eq!(code(&none), "unknown_artifact");
    // The recording artifact is committed and indexed, and small: the answer
    // is the identical one, with no read and no reservation.
    let rec = w.query(image_query(&st, REC_ART, 1)).await;
    assert_eq!(rec, none);
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
    assert_eq!(charged(&w).await, 0);
}
