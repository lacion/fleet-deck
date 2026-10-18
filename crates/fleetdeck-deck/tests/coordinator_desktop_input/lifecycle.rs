//! The execute check, the recovery, the duplicate after a revocation and the
//! final delivery pass of an input operation (R13 P7 §6.4, §8.3, §11;
//! DL-242).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::coordinator_desktop_support::*;
use fleetdeck_deck::store::GrantState;
use fleetdeck_deck::store::coordinator_desktop::{OpState, OpTool};
use serde_json::{Value, json};

use super::UNKNOWN_TEXT;

fn payload(id: &str, hash: &str, phase: &str) -> Value {
    json!({"operation_id": id, "action_hash": hash, "phase": phase})
}

fn verdict_of(v: &Value) -> (bool, String) {
    (
        v["valid"].as_bool().unwrap(),
        v["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

/// An `attempted` input operation `id` with hash `H` (the plain step; the
/// check does not read the snapshot).
async fn attempted(w: &World, id: &str, tool: OpTool) {
    let length = u32::from(tool != OpTool::PointerMove);
    w.attempted(input_new_op(w, id, tool, "H", length, &w.station()))
        .await;
}

#[tokio::test]
async fn the_execute_check_accepts_an_input_and_the_import_check_never_does() {
    let w = input_world(10, 100).await;
    for (i, tool) in [OpTool::KeyPress, OpTool::TypeText, OpTool::PointerMove]
        .into_iter()
        .enumerate()
    {
        let id = format!("op-{i}");
        attempted(&w, &id, tool).await;
        let v = w.check(payload(&id, "H", "execute")).await;
        assert_eq!(v, json!({"valid": true, "reason": null}), "{tool:?}");
        let v = w.check(payload(&id, "H", "import")).await;
        assert_eq!(
            verdict_of(&v),
            (false, "authority_lost".to_owned()),
            "{tool:?}"
        );
        // Another hash is lost.
        let v = w.check(payload(&id, "other", "execute")).await;
        assert_eq!(verdict_of(&v), (false, "authority_lost".to_owned()));
    }
    // The caller policy was asked once for each valid execute check.
    assert_eq!(w.fake.count("coordinator.policy_now"), 3);
    // An operation that is not `attempted` is lost.
    let prepared = input_new_op(&w, "op-p", OpTool::KeyPress, "H", 1, &w.station());
    w.f.store.desktop_admit(prepared, w.now()).await.unwrap();
    let v = w.check(payload("op-p", "H", "execute")).await;
    assert_eq!(verdict_of(&v), (false, "authority_lost".to_owned()));
}

#[tokio::test]
async fn the_execute_check_follows_the_allowance_the_grant_and_the_features() {
    let w = input_world(10, 100).await;
    attempted(&w, "op-1", OpTool::TypeText).await;
    let lost = (false, "authority_lost".to_owned());
    // The caller policy says no.
    w.fake.set(input_except("coordinator.policy_now", |_| {
        Some(ok(
            json!({"fleet_control": false, "bound": true, "binding_matches": true}),
        ))
    }));
    assert_eq!(
        verdict_of(&w.check(payload("op-1", "H", "execute")).await),
        lost
    );
    w.fake.set(input_station());
    // The target link loses `desktop_input_mcp.v1`.
    w.f.store.set_link_features(&w.f.station, 97, Vec::new());
    assert_eq!(
        verdict_of(&w.check(payload("op-1", "H", "execute")).await),
        lost
    );
    w.f.store.clear_link_features(&w.f.station, 97);
    assert_eq!(
        verdict_of(&w.check(payload("op-1", "H", "execute")).await),
        (true, String::new())
    );
    // The allowance is gone.
    let gid = w.b.grant_id.clone();
    w.f.store
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE tokens SET allow_input = 0, input_scope = NULL WHERE grant_id = ?1",
                [gid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        verdict_of(&w.check(payload("op-1", "H", "execute")).await),
        lost
    );
}

#[tokio::test]
async fn the_recovery_makes_an_attempted_input_unknown_and_a_duplicate_never_sends() {
    let w = input_world(10, 100).await;
    // `r-unknown` reached the send; `r-prepared` never did.
    let now = w.now();
    for id in ["r-unknown", "r-prepared"] {
        let op = input_new_op(&w, id, OpTool::TypeText, "H", 3, &w.station());
        w.f.store.desktop_admit(op, now).await.unwrap();
    }
    let signed =
        w.f.store
            .desktop_op_attempted_input("r-unknown", &["10.1.1.1".to_owned()], now + 1)
            .await
            .unwrap();
    assert_eq!(signed.map(|s| s.len()), Some(0));
    assert_eq!(
        w.f.store.desktop_ops_recover(now + 7).await.unwrap(),
        (1, 1)
    );
    let unknown = w.f.store.desktop_op("r-unknown").await.unwrap().unwrap();
    assert_eq!(unknown.state, OpState::Unknown);
    let (attempted, finished, addrs) = op_times(&w, "r-unknown").await;
    assert_eq!(
        (attempted, finished, addrs.as_deref()),
        (Some(now + 1), Some(now + 7), Some(r#"["10.1.1.1"]"#))
    );
    let prepared = w.f.store.desktop_op("r-prepared").await.unwrap().unwrap();
    assert_eq!(
        (prepared.state, prepared.code.as_deref()),
        (OpState::Failed, Some("authority_unknown"))
    );
    assert_eq!(op_times(&w, "r-prepared").await.1, Some(now + 7));
    // The status tool tells both.
    let s = w.query(status_query("r-unknown")).await;
    assert_eq!(s["result"]["state"], "unknown");
    assert_eq!(s["result"]["error"], "desktop_timeout");
    assert_eq!(s["result"]["message"], UNKNOWN_TEXT);
    let s = w.query(status_query("r-prepared")).await;
    assert_eq!(s["result"]["state"], "failed");
    assert_eq!(s["result"]["error"], "authority_unknown");
    // A duplicate call with the same request and payload replays and never
    // sends; it also charges nothing.
    let before = input_counters(&w).await;
    let payload_hash = unknown.payload_hash.clone();
    assert!(!payload_hash.is_empty());
    let r = w.call("c-dup", text_call("r-unknown", "abc", None)).await;
    // The stored payload hash is a test value: the same request ID with the
    // real payload conflicts, and still nothing is sent.
    assert_eq!(code(&r), "request_id_conflict");
    assert_eq!(w.fake.count("desktop.act"), 0);
    assert_eq!(input_counters(&w).await, before);
    // A revoked grant gives a duplicate and a status query no answer.
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    let r = w.call("c-dup2", text_call("r-unknown", "abc", None)).await;
    assert_eq!(code(&r), "not_granted");
    let s = w.query(status_query("r-unknown")).await;
    assert_eq!(code(&s), "not_granted");
}

#[tokio::test]
async fn a_duplicate_of_an_unknown_input_after_a_revoke_regains_no_authority() {
    let mut w = input_world(10, 100).await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(input_except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    let first = w.call("c1", key_call("k1", "a", Some(beta.as_str()))).await;
    assert_eq!(code(&first), "desktop_timeout");
    // The status tool tells `unknown`; a duplicate call needs the live link
    // of the target (the P6 authorization), and sends nothing.
    let s = w.query(status_query("k1")).await;
    assert_eq!(s["result"]["state"], "unknown");
    let dup = w.call("c2", key_call("k1", "a", Some(beta.as_str()))).await;
    assert_eq!(code(&dup), "station_unreachable");
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    let dup = w.call("c3", key_call("k1", "a", Some(beta.as_str()))).await;
    assert_eq!(code(&dup), "not_granted");
    assert!(!dup.to_string().contains("unknown"), "{dup}");
    assert_eq!(beta_fake.count("desktop.act"), 1);
    assert_eq!(
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .state,
        GrantState::Revoked
    );
}

#[tokio::test]
async fn a_revoke_during_the_send_keeps_the_durable_result_and_delivers_authority_lost() {
    let w = input_world(10, 100).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "desktop.act" {
                store.revoke_grant(gid, "test".into()).await.unwrap();
            }
            Some(input_answer(&req))
        }
    }));
    let r = w.call("c1", text_call("t1", "delivered?", None)).await;
    assert_eq!(code(&r), "authority_lost");
    let o = op_of(&w, "t1").await;
    assert_eq!(r["operation_id"], o.operation_id.as_str());
    // The durable result stays done, charged once.
    assert_eq!((o.state, o.result), (OpState::Done, Some(json!({}))));
    assert_eq!(input_counters(&w).await, (1, 1, 10, 1));
    // The outcome row (done) and the delivery refusal are both written.
    let rows = audit_details(&w, "desktop.type_text").await;
    let stages: Vec<_> = rows.iter().map(|d| d["stage"].as_str().unwrap()).collect();
    assert_eq!(stages, ["attempt", "outcome", "delivery"], "{rows:?}");
    assert_eq!(rows[2]["code"], "authority_lost");
    assert!(rows[2].get("window_id").is_none(), "{}", rows[2]);
    assert!(!w.audit_text().await.contains("delivered?"));
}

#[tokio::test]
async fn the_allowance_withdrawn_during_the_send_is_authority_lost_and_a_failure_is_not_checked() {
    let w = input_world(10, 100).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "desktop.act" {
                store
                    .db()
                    .call(move |conn| {
                        conn.execute(
                            "UPDATE tokens SET allow_input = 0, input_scope = NULL
                             WHERE grant_id = ?1",
                            [gid],
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            Some(input_answer(&req))
        }
    }));
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(code(&r), "authority_lost");
    assert_eq!(op_of(&w, "k1").await.state, OpState::Done);
    // A failure is delivered as it is, with no delivery pass: the grant has
    // no allowance now, but the next call is refused at the authorization.
    let r = w.call("c2", key_call("k2", "a", None)).await;
    assert_eq!(code(&r), "input_not_allowed");
}

#[tokio::test]
async fn an_idle_expired_grant_delivers_no_input_result() {
    let w = input_world(10, 100).await;
    let (gid, det) = (w.b.grant_id.clone(), w.f.det.clone());
    let store = w.f.store.clone();
    w.fake.set(handler(move |req| {
        let (gid, det, store) = (gid.clone(), det.clone(), store.clone());
        async move {
            if req.method == "desktop.act" {
                store
                    .db()
                    .call(move |conn| {
                        conn.execute(
                            "UPDATE tokens SET idle_ttl_ms = 10000 WHERE grant_id = ?1",
                            [gid],
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
                det.advance(std::time::Duration::from_secs(11));
            }
            Some(input_answer(&req))
        }
    }));
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(code(&r), "authority_lost");
    assert_eq!(op_of(&w, "k1").await.state, OpState::Done);
}
