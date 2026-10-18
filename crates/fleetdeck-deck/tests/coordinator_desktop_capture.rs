//! The P6 deck side of coordinator screenshots (R13 P6 task T3): the
//! `screenshot` call with its capture record and provenance, the recovery of a
//! coordinator capture, `capture_status`, and `read_capture_image`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use coordinator_desktop_support::*;
use fleetdeck_deck::store::coordinator_desktop::OpState;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::capture::CaptureStatus;
use serde_json::{Value, json};

const MAX_PNG: usize = 716_800;

#[tokio::test]
async fn a_screenshot_is_recorded_sent_once_indexed_with_a_coordinator_provenance() {
    let (w, reply, id) = committed_shot(&cap_png(300)).await;
    let st = w.station();
    let want = json!({"request_id": "req-1", "state": "committed", "capture_id": id,
        "station": st.as_str(),
        "artifact": {"station_id": st.as_str(), "artifact_id": ART, "version": 1},
        "monitor": "DP-1", "size": 300, "sha256": cap_sha(&cap_png(300)), "at": 1000});
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
    assert_eq!(act["action"], "screen_capture");
    assert_eq!(act["authority"]["action_id"], id.as_str());
    // The record: the coordinator column is the compact context.
    let r = cap_row(&w, &id, &st).await;
    let context = serde_json::to_string(&w.context()).unwrap();
    assert_eq!(r.coordinator.as_deref(), Some(context.as_str()));
    assert_eq!((r.state, r.indexed), (CaptureState::Committed, true));
    let prov = published_of(&r).version.capture.expect("a provenance");
    assert_eq!(
        prov.caller,
        fleetdeck_proto::capture::CaptureCaller::Coordinator
    );
    assert_eq!(prov.caller_station.as_deref(), Some(st.as_str()));
    assert_eq!(prov.caller_session.as_deref(), Some("coord-1"));
    assert_eq!(prov.caller_grant_id.as_deref(), Some(w.b.grant_id.as_str()));
    assert_eq!(prov.station_id, st.as_str());
    // The operation is done, charged once for the desktop and the capture.
    let o = cap_op(&w, "req-1").await;
    assert_eq!(
        (o.state, o.operation_id.as_str()),
        (OpState::Done, id.as_str())
    );
    assert!(!format!("{o:?}").contains("req-1"), "no raw request id");
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!((g.desktop_used, g.capture_used), (1, 1));
    // Two audit rows of `desktop.capture`, correlated by the capture ID.
    let rows = w.audit_rows(Some("desktop.capture")).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let a: Value = serde_json::from_str(&rows[0].2).unwrap();
    let b: Value = serde_json::from_str(&rows[1].2).unwrap();
    assert_eq!(
        (a["stage"].as_str(), b["stage"].as_str()),
        (Some("attempt"), Some("outcome"))
    );
    assert_eq!(a["capture_id"], id.as_str());
    assert_eq!(a["monitor"], "DP-1");
    assert_eq!(a["charged"], json!({"desktop": 1, "capture": 1}));
    // A duplicate replays the capture state and sends nothing.
    let again = w.call("c2", shot_call("req-1", None)).await;
    assert_eq!(again, reply);
    assert_eq!(w.fake.count("desktop.act"), 1);
}

#[tokio::test]
async fn a_station_failure_is_a_failed_record_with_the_capture_code_and_text() {
    let w = capture_world().await;
    let cases = [
        (
            "disk_full",
            "x",
            "disk_full",
            "The station has too little free space for a capture.",
        ),
        ("busy", "x", "busy", "A capture is already running."),
        (
            "unsupported",
            "The screenshot tool grim is not installed.",
            "desktop_unsupported",
            "The screenshot tool grim is not installed.",
        ),
        (
            "authority_lost",
            "STATION-TEXT",
            "authority_lost",
            "Desktop authority was withdrawn before this operation could continue.",
        ),
    ];
    for (i, (wire, message, want, text)) in cases.into_iter().enumerate() {
        // Two screenshots in a rolling minute: the window moves on.
        w.f.det.advance(Duration::from_secs(61));
        w.fake
            .set(except("desktop.act", move |_| Some(fail(wire, message))));
        let rid = format!("r{i}");
        let reply = w.call(&format!("c{i}"), shot_call(&rid, None)).await;
        assert_eq!(code(&reply), want, "{wire}");
        assert_eq!(reply["message"], text);
        assert!(!reply.to_string().contains("STATION-TEXT"));
        let o = cap_op(&w, &rid).await;
        assert_eq!(reply["capture_id"], o.operation_id.as_str());
        assert_eq!((o.state, o.code.as_deref()), (OpState::Failed, Some(want)));
        let r = cap_row(&w, &o.operation_id, &w.station()).await;
        assert_eq!(r.state, CaptureState::Failed, "{wire}");
        // The status tool tells the same.
        let s = w.query(capture_status_query(&rid)).await;
        assert_eq!(s["result"]["state"], "failed");
        assert_eq!(
            (
                s["result"]["error"].as_str(),
                s["result"]["message"].as_str()
            ),
            (Some(want), Some(text))
        );
    }
    // A code that is not a capture code: mapped as a relay code, and the
    // record stays attempted (the station may have run).
    w.f.det.advance(Duration::from_secs(61));
    w.fake
        .set(except("desktop.act", |_| Some(fail("forbidden", "x"))));
    let reply = w.call("cf", shot_call("rf", None)).await;
    assert_eq!(code(&reply), "desktop_forbidden");
    let o = cap_op(&w, "rf").await;
    assert_eq!(o.state, OpState::Failed);
    let r = cap_row(&w, &o.operation_id, &w.station()).await;
    assert_eq!(r.state, CaptureState::Attempted);
}

#[tokio::test]
async fn a_lost_link_after_the_send_is_unknown_and_the_record_stays_attempted() {
    let mut w = capture_world().await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    let reply = w.call("c1", shot_call("r1", Some(beta.as_str()))).await;
    assert_eq!(code(&reply), "desktop_timeout");
    let o = cap_op(&w, "r1").await;
    assert_eq!(reply["capture_id"], o.operation_id.as_str());
    assert_eq!(o.state, OpState::Unknown);
    let r = cap_row(&w, &o.operation_id, &beta).await;
    assert_eq!(r.state, CaptureState::Attempted);
    // The status of the record: pending, until the recovery or the 24 h.
    let s = w.query(capture_status_query("r1")).await;
    assert_eq!(s["result"]["state"], "pending");
    // An aged unknown is `unknown` with its text; never a second capture.
    w.f.store
        .capture_unknown(beta.clone(), o.operation_id.clone())
        .await
        .unwrap();
    let s = w.query(capture_status_query("r1")).await;
    assert_eq!(s["result"]["state"], "unknown");
    assert!(
        s["result"]["message"]
            .as_str()
            .unwrap()
            .starts_with("The screenshot result is unknown.")
    );
    assert_eq!(beta_fake.count("desktop.act"), 1);
}

#[tokio::test]
async fn another_provenance_or_a_missing_one_is_an_indexing_failure_never_an_owner_capture() {
    for (n, prov) in [Prov::Grant("other-grant"), Prov::Missing]
        .into_iter()
        .enumerate()
    {
        let w = capture_world().await;
        let mut spec = ShotSpec::of(&w, &cap_png(300));
        spec.prov = prov;
        w.fake.set(shot_station(spec, reads_of(cap_png(300))));
        let reply = w.call("c1", shot_call("req-1", None)).await;
        // The station committed: the call is not a failure, the capture is pending.
        assert_eq!(reply["result"]["state"], "pending", "{n}: {reply}");
        let o = cap_op(&w, "req-1").await;
        let r = cap_row(&w, &o.operation_id, &w.station()).await;
        assert_eq!((r.state, r.indexed), (CaptureState::Committed, false));
        assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 0);
        assert_eq!(count(&w, "SELECT COUNT(*) FROM artifact_versions").await, 0);
        let failed: Vec<String> = w
            .audit_rows(Some("desktop.capture"))
            .await
            .into_iter()
            .map(|r| r.2)
            .filter(|d| d.contains("indexing_failed"))
            .collect();
        assert_eq!(failed.len(), 1, "{n}");
        // The status tells pending with its text, never failed.
        let s = w.query(capture_status_query("req-1")).await;
        assert_eq!(s["result"]["state"], "pending");
        assert!(
            s["result"]["message"]
                .as_str()
                .unwrap()
                .contains("still updating")
        );
    }
}

#[tokio::test]
async fn recovery_never_indexes_a_coordinator_row_as_an_owner_capture() {
    let w = capture_world().await;
    let st = w.station();
    let id = "01ARZ3NDEKTSV4RRFFQ69G5F01";
    let context = serde_json::to_string(&w.context()).unwrap();
    // A malformed `coordinator` value, and a context of another grant.
    let other = context.replace(&w.b.grant_id, "other-grant");
    for (i, value) in ["not json".to_owned(), other].into_iter().enumerate() {
        let id = format!("{}{i}", &id[..25]);
        w.f.store
            .capture_prepare_coordinator(st.clone(), id.clone(), "h".into(), "DP-1".into(), value)
            .await
            .unwrap();
        w.f.store
            .capture_attempted(st.clone(), id.clone())
            .await
            .unwrap();
        let r = cap_row(&w, &id, &st).await;
        let sent = ShotSpec::of(&w, &cap_png(100)).published(&id);
        let published = serde_json::from_value(sent).unwrap();
        let status = CaptureStatus::Committed {
            capture_id: id.clone(),
            published,
        };
        let got = fleetdeck_deck::desktop_capture::reconcile(&w.f.store, &r, status).await;
        // Malformed, or another grant than the reported one: no index, and
        // the row is kept for the owner.
        got.unwrap();
        let after = cap_row(&w, &id, &st).await;
        assert_eq!(
            (after.state, after.indexed),
            (CaptureState::Committed, false),
            "{i}"
        );
        assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 0);
        assert_eq!(count(&w, "SELECT COUNT(*) FROM artifact_versions").await, 0);
    }
    // One audit row for each, with the fixed code.
    let rows = w.audit_rows(Some("desktop.capture")).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r.2.contains("indexing_failed")));
}

#[tokio::test]
async fn recovery_finishes_a_coordinator_capture_after_a_revoke_and_delivers_nothing() {
    let mut w = capture_world().await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    assert_eq!(
        code(&w.call("c1", shot_call("r1", Some(beta.as_str()))).await),
        "desktop_timeout"
    );
    let o = cap_op(&w, "r1").await;
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    let r = cap_row(&w, &o.operation_id, &beta).await;
    let mut spec = ShotSpec::of(&w, &cap_png(100));
    spec.station = w.station().to_string();
    let mut published: Value = spec.published(&o.operation_id);
    published["artifact"]["station_id"] = json!("beta-side");
    let status = CaptureStatus::Committed {
        capture_id: o.operation_id.clone(),
        published: serde_json::from_value(published).unwrap(),
    };
    fleetdeck_deck::desktop_capture::reconcile(&w.f.store, &r, status)
        .await
        .unwrap();
    let after = cap_row(&w, &o.operation_id, &beta).await;
    assert_eq!(
        (after.state, after.indexed),
        (CaptureState::Committed, true)
    );
    let prov = published_of(&after).version.capture.unwrap();
    assert_eq!(prov.caller_grant_id.as_deref(), Some(w.b.grant_id.as_str()));
    // The operation row is unchanged, and the revoked caller sees nothing.
    assert_eq!(cap_op(&w, "r1").await.state, OpState::Unknown);
    assert_eq!(
        code(&w.query(capture_status_query("r1")).await),
        "not_granted"
    );
    assert_eq!(
        code(&w.query(image_query(beta.as_str(), ART, 1)).await),
        "not_granted"
    );
}

#[tokio::test]
async fn capture_status_maps_the_states_and_hides_what_is_not_this_callers() {
    let (w, _, id) = committed_shot(&cap_png(300)).await;
    let want_unknown =
        |rid: &str| json!({"status": "ok", "result": {"request_id": rid, "state": "unknown"}});
    // Unknown, wrong-kind and foreign request IDs answer the same.
    assert_eq!(w.call("c2", notify("req-n", "t")).await["status"], "ok");
    assert_eq!(
        w.query(capture_status_query("nope")).await,
        want_unknown("nope")
    );
    assert_eq!(
        w.query(capture_status_query("req-n")).await,
        want_unknown("req-n")
    );
    let b2 = common::fleet::bound_edit(&w.f, &w.f.station, "coord-2", &BOTH, |_| {}).await;
    let mut env = w.query_env(capture_status_query("req-1"));
    env["caller"] = json!("coord-2");
    env["binding"] = json!(b2);
    assert_eq!(reply(w.query_raw(env).await), want_unknown("req-1"));
    // The notify tool does not see a screenshot either.
    assert_eq!(
        w.query(status_query("req-1")).await["result"]["state"],
        "unknown"
    );
    // Committed, then deleted by a tombstone.
    let s = w.query(capture_status_query("req-1")).await;
    assert_eq!(s["result"]["state"], "committed");
    assert_eq!(s["result"]["capture_id"], id.as_str());
    w.f.store
        .artifact_delete(w.station(), ART.to_owned())
        .await
        .unwrap();
    let s = w.query(capture_status_query("req-1")).await;
    assert_eq!(
        s["result"],
        json!({"request_id": "req-1", "state": "deleted",
        "capture_id": id, "station": w.station().as_str()})
    );
    // A lost authority never sees a stored result.
    w.rebind().await;
    assert_eq!(
        code(&w.query(capture_status_query("req-1")).await),
        "binding_stale"
    );
    let rows = w.audit_rows(Some("desktop.capture_status")).await;
    assert!(rows.len() >= 4, "{rows:?}");
}

#[tokio::test]
async fn capture_status_of_a_saved_but_not_indexed_capture_asks_the_station_once() {
    let w = capture_world().await;
    let mut spec = ShotSpec::of(&w, &cap_png(300));
    spec.prov = Prov::Missing;
    let station_spec = spec.clone();
    let committed_status = move |req: &Req| {
        let id = req.params["read"]["capture_id"].as_str().unwrap_or("");
        ok(json!({"state": "committed", "capture_id": id, "published": station_spec.published(id)}))
    };
    w.fake
        .set(shot_station(spec.clone(), Arc::new(committed_status)));
    assert_eq!(
        w.call("c1", shot_call("req-1", None)).await["result"]["state"],
        "pending"
    );
    let before = w.fake.count("desktop.coordinator_read");
    let s = w.query(capture_status_query("req-1")).await;
    assert_eq!(s["result"]["state"], "pending");
    assert!(
        s["result"]["message"]
            .as_str()
            .unwrap()
            .contains("saved on the station")
    );
    let reads = w.fake.params_of("desktop.coordinator_read");
    assert_eq!(reads.len(), before + 1);
    assert_eq!(reads.last().unwrap()["read"]["kind"], "capture_status");
    // A read that fails keeps the deck state.
    w.fake
        .set(shot_station(spec, Arc::new(|_| fail("failed", "x"))));
    assert_eq!(
        w.query(capture_status_query("req-1")).await["result"]["state"],
        "pending"
    );
}

#[tokio::test]
async fn the_image_of_the_exact_cap_passes_through_both_hops_within_the_frame() {
    let bytes = cap_png(MAX_PNG);
    let (w, _, id) = committed_shot(&bytes).await;
    let reply = w.query(image_query(w.station().as_str(), ART, 1)).await;
    assert_eq!(
        reply["status"],
        "ok",
        "{}",
        &reply.to_string()[..200.min(reply.to_string().len())]
    );
    let result = &reply["result"];
    assert_eq!(result["png_base64"], STANDARD.encode(&bytes).as_str());
    assert_eq!(result["png_base64"].as_str().unwrap().len(), 955_736);
    assert_eq!(
        result["meta"],
        json!({
        "artifact": {"station_id": w.station().as_str(), "artifact_id": ART, "version": 1},
        "capture_id": id, "monitor": "DP-1", "at": 1000, "size": MAX_PNG,
        "sha256": cap_sha(&bytes)})
    );
    assert!(serde_json::to_vec(&reply).unwrap().len() <= 1_048_576 - 4096);
    assert_eq!(charged(&w).await, 716_800);
    let read = &w.fake.params_of("desktop.coordinator_read")[0]["read"];
    assert_eq!(
        *read,
        json!({"kind": "capture_file", "artifact_id": ART, "version": 1,
        "offset": 0, "length": MAX_PNG})
    );
}

#[tokio::test]
async fn an_image_over_the_cap_is_too_large_before_any_reservation() {
    // The version claims one byte more than the cap.
    let w = capture_world().await;
    let mut spec = ShotSpec::of(&w, &cap_png(300));
    spec.size = 716_801;
    w.fake.set(shot_station(spec, reads_of(cap_png(300))));
    assert_eq!(w.call("c1", shot_call("r1", None)).await["status"], "ok");
    let reply = w.query(image_query(w.station().as_str(), ART, 1)).await;
    assert_eq!(code(&reply), "image_too_large");
    assert_eq!(
        reply["message"],
        "The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board."
    );
    assert_eq!(charged(&w).await, 0);
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
}

#[tokio::test]
async fn every_artifact_that_is_not_this_callers_committed_capture_is_the_same_answer() {
    let (w, _, id) = committed_shot(&cap_png(300)).await;
    let st = w.station();
    let ask = |station: String, artifact: &'static str, version: u32| {
        let w = &w;
        async move { w.query(image_query(&station, artifact, version)).await }
    };
    let unknown = ask(st.to_string(), "NOSUCHART", 1).await;
    assert_eq!(code(&unknown), "unknown_artifact");
    assert_eq!(
        unknown["message"],
        "No such capture artifact is available to this grant."
    );
    // The wrong version.
    assert_eq!(ask(st.to_string(), ART, 2).await, unknown);
    // Another grant's capture, and another session's.
    for (from, to) in [
        (w.b.grant_id.clone(), "g-other"),
        ("coord-1".to_owned(), "coord-9"),
    ] {
        swap(&w, &id, &from, to).await;
        assert_eq!(ask(st.to_string(), ART, 1).await, unknown, "{to}");
        swap(&w, &id, to, &from).await;
    }
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
    // An owner capture (no coordinator value) is not reachable either.
    w.f.store
        .db()
        .call(|conn| Ok(conn.execute("UPDATE desktop_captures SET coordinator = NULL", [])?))
        .await
        .unwrap();
    assert_eq!(ask(st.to_string(), ART, 1).await, unknown);
    // A station that is not a station of the grant is refused earlier.
    assert_eq!(
        code(&ask("other-station".into(), ART, 1).await),
        "not_granted"
    );
    assert_eq!(charged(&w).await, 0);
    // A tombstoned capture answers the same.
    let (w, _, _) = committed_shot(&cap_png(300)).await;
    w.f.store
        .artifact_delete(w.station(), ART.to_owned())
        .await
        .unwrap();
    let gone = w.query(image_query(w.station().as_str(), ART, 1)).await;
    assert_eq!(gone, unknown);
    assert_eq!(charged(&w).await, 0);
}

/// Replaces `from` by `to` in the `coordinator` value of the capture `id`.
async fn swap(w: &World, id: &str, from: &str, to: &str) {
    let (id, from, to) = (id.to_owned(), from.to_owned(), to.to_owned());
    let n =
        w.f.store
            .db()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE desktop_captures SET coordinator = replace(coordinator, ?1, ?2)
                 WHERE capture_id = ?3",
                    [from, to, id],
                )?)
            })
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn bytes_that_do_not_match_the_version_are_image_invalid_and_the_charge_stays() {
    let bytes = cap_png(300);
    let (w, _, _) = committed_shot(&bytes).await;
    let mut flipped = bytes.clone();
    flipped[100] ^= 1;
    let mut short = chunk_of(&bytes);
    short["data_base64"] = json!(STANDARD.encode(&bytes[..299]));
    let mut offset = chunk_of(&bytes);
    offset["offset"] = json!(1);
    let mut open_end = chunk_of(&bytes);
    open_end["eof"] = json!(false);
    let mut no_signature = bytes.clone();
    no_signature[1] = b'X';
    let cases = [
        chunk_of(&flipped),
        short,
        offset,
        open_end,
        chunk_of(&no_signature),
    ];
    for (i, case) in cases.into_iter().enumerate() {
        let case = Arc::new(case);
        w.fake.set(shot_station(
            ShotSpec::of(&w, &bytes),
            Arc::new(move |_| ok((*case).clone())),
        ));
        let reply = w.query(image_query(w.station().as_str(), ART, 1)).await;
        assert_eq!(code(&reply), "image_invalid", "case {i}");
        assert_eq!(
            reply["message"],
            "The screenshot artifact could not be verified."
        );
        assert!(!reply.to_string().contains("png_base64"));
        assert_eq!(charged(&w).await, 716_800 * (i as u64 + 1), "case {i}");
    }
    // The slot is free again: a good read works (the rate has room for six).
    w.fake.set(shot_station(
        ShotSpec::of(&w, &bytes),
        reads_of(bytes.clone()),
    ));
    let good = w.query(image_query(w.station().as_str(), ART, 1)).await;
    assert_eq!(good["status"], "ok");
    assert_eq!(charged(&w).await, 716_800 * 6);
    // The seventh read of the window is `rate_limited`, and charges nothing.
    assert_eq!(
        code(&w.query(image_query(w.station().as_str(), ART, 1)).await),
        "rate_limited"
    );
    assert_eq!(charged(&w).await, 716_800 * 6);
}

#[tokio::test]
async fn a_tombstone_or_a_revoke_between_the_read_and_the_answer_gives_no_image() {
    let bytes = cap_png(300);
    for revoke in [false, true] {
        let (w, _, _) = committed_shot(&bytes).await;
        let (store, st, gid) = (w.f.store.clone(), w.station(), w.b.grant_id.clone());
        let body = chunk_of(&bytes);
        w.fake.set(handler(move |req| {
            let (store, st, gid, body) = (store.clone(), st.clone(), gid.clone(), body.clone());
            async move {
                if req.method != "desktop.coordinator_read" {
                    return Some(standard_answer(&req));
                }
                if revoke {
                    store.revoke_grant(gid, "test".into()).await.unwrap();
                } else {
                    store.artifact_delete(st, ART.to_owned()).await.unwrap();
                }
                Some(ok(body))
            }
        }));
        let reply = w.query(image_query(w.station().as_str(), ART, 1)).await;
        let want = if revoke {
            "authority_lost"
        } else {
            "unknown_artifact"
        };
        assert_eq!(code(&reply), want);
        assert!(!reply.to_string().contains("png_base64"));
        assert_eq!(charged(&w).await, 716_800, "the charge stays");
    }
}

#[tokio::test]
async fn a_second_image_read_of_a_grant_in_flight_is_busy_and_charges_nothing() {
    let bytes = cap_png(300);
    let (w, _, _) = committed_shot(&bytes).await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let held = Arc::clone(&gate);
    let body = chunk_of(&bytes);
    w.fake.set(handler(move |req| {
        let (gate, body) = (Arc::clone(&held), body.clone());
        async move {
            if req.method == "desktop.coordinator_read" {
                gate.acquire().await.unwrap().forget();
                return Some(ok(body));
            }
            Some(standard_answer(&req))
        }
    }));
    let query = image_query(w.station().as_str(), ART, 1);
    let first = w.query(query.clone());
    let second = async {
        w.fake.wait_for("desktop.coordinator_read", 1).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let busy = w.query(query.clone()).await;
        gate.add_permits(1);
        busy
    };
    let (a, busy) = tokio::join!(first, second);
    assert_eq!(a["status"], "ok");
    assert_eq!(code(&busy), "busy");
    assert_eq!(charged(&w).await, 716_800, "only the first reserved");
}

#[tokio::test]
async fn the_image_budget_is_charged_for_a_whole_reservation_and_audit_rows_hold_no_image() {
    let bytes = cap_png(300);
    let (w, _, id) = committed_shot(&bytes).await;
    let reply = w.query(image_query(w.station().as_str(), ART, 1)).await;
    let b64 = reply["result"]["png_base64"].as_str().unwrap().to_owned();
    assert_eq!(
        charged(&w).await,
        716_800,
        "the whole reservation, not 300 bytes"
    );
    let dump = w.audit_text().await;
    assert!(!dump.contains(&b64) && !dump.contains(&b64[..32]), "{dump}");
    let rows = w.audit_rows(Some("desktop.capture_read")).await;
    assert_eq!(rows.len(), 1);
    let d: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(
        (d["stage"].as_str(), d["capture_id"].as_str()),
        (Some("outcome"), Some(id.as_str()))
    );
    assert_eq!(d["artifact_id"], ART);
    assert_eq!(d["reserved"], 716_800);
    // A refused read names its code and no supplied identifier.
    let r = w
        .query(image_query(w.station().as_str(), "SENTINEL-ART-4711", 1))
        .await;
    assert_eq!(code(&r), "unknown_artifact");
    assert!(!w.audit_text().await.contains("SENTINEL"));
}
