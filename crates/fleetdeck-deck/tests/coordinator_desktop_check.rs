//! `desktop.authority_check`, the read permits and `desktop_info` (R13 P6
//! task T1b, §5.6, §5.7, §4.1): the valid paths, every refusal, the caller
//! policy and its uncertain answers, the malformed forms, the single use of a
//! permit, and the class filter of the information.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use coordinator_desktop_support::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::coordinator_desktop::{OpOutcome, OpTool};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

const LOST: (bool, &str) = (false, "authority_lost");
const UNKNOWN: (bool, &str) = (false, "authority_unknown");

fn payload(id: &str, hash: &str, phase: &str) -> Value {
    json!({"operation_id": id, "action_hash": hash, "phase": phase})
}

/// `(valid, reason)` of an answer.
fn verdict_of(v: &Value) -> (bool, String) {
    (
        v["valid"].as_bool().unwrap(),
        v["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

fn is(v: &Value, want: (bool, &str)) {
    assert_eq!(verdict_of(v), (want.0, want.1.to_owned()), "{v}");
}

/// A world with an `attempted` notification `op-1` that has hash `H`.
async fn attempted_world() -> World {
    let w = world().await;
    w.attempted(w.new_op("op-1", OpTool::Notify, "H", &w.station()))
        .await;
    w
}

#[tokio::test]
async fn a_valid_execute_check_asks_the_caller_policy_for_the_current_binding() {
    let w = attempted_world().await;
    let v = w.check(payload("op-1", "H", "execute")).await;
    assert_eq!(v, json!({"valid": true, "reason": null}));
    let asked = w.fake.params_of("coordinator.policy_now");
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0],
        json!({"caller_session": "coord-1", "binding": w.b})
    );
}

#[tokio::test]
async fn the_import_phase_is_valid_for_a_screenshot_operation_only() {
    let w = world_with(&BOTH, |_| {}).await;
    w.attempted(w.new_op("op-shot", OpTool::Screenshot, "H2", &w.station()))
        .await;
    let v = w.check(payload("op-shot", "H2", "import")).await;
    assert_eq!(v, json!({"valid": true, "reason": null}));
    let v = w.check(payload("op-shot", "H2", "execute")).await;
    assert_eq!(v, json!({"valid": true, "reason": null}));
    w.attempted(w.new_op("op-note", OpTool::Notify, "H3", &w.station()))
        .await;
    is(&w.check(payload("op-note", "H3", "import")).await, LOST);
    is(
        &w.check(payload("op-note", "H3", "execute")).await,
        (true, ""),
    );
}

/// A recording imports too (R13 P5b-2 §7.6): the station asks for import
/// authority before the import of a coordinator recording.
#[tokio::test]
async fn the_import_phase_is_valid_for_a_recording_operation() {
    let w = world_with(&BOTH, |g| {
        g.recording_cap = 5;
        g.recording_second_cap = 600;
    })
    .await;
    let mut op = w.new_op("op-rec", OpTool::RecordMonitor, "H4", &w.station());
    op.class = fleetdeck_deck::store::coordinator_desktop::OpClass::Capture;
    op.seconds = Some(2);
    w.attempted(op).await;
    let v = w.check(payload("op-rec", "H4", "import")).await;
    assert_eq!(v, json!({"valid": true, "reason": null}));
    let v = w.check(payload("op-rec", "H4", "execute")).await;
    assert_eq!(v, json!({"valid": true, "reason": null}));
}

/// PRE-PR r1 finding 1: an idle-expired grant fails both phases before the
/// grant sweep runs.
#[tokio::test]
async fn an_idle_expired_grant_fails_the_execute_and_import_checks() {
    let w = world_with(&BOTH, |g| {
        g.recording_cap = 5;
        g.recording_second_cap = 600;
        g.idle_ttl_ms = 10_000;
    })
    .await;
    let mut op = w.new_op("op-rec", OpTool::RecordMonitor, "H4", &w.station());
    op.class = fleetdeck_deck::store::coordinator_desktop::OpClass::Capture;
    op.seconds = Some(2);
    w.attempted(op).await;
    for phase in ["execute", "import"] {
        let v = w.check(payload("op-rec", "H4", phase)).await;
        assert_eq!(v, json!({"valid": true, "reason": null}), "{phase}");
    }
    w.f.det.advance(Duration::from_secs(11));
    for phase in ["execute", "import"] {
        is(&w.check(payload("op-rec", "H4", phase)).await, LOST);
    }
}

#[tokio::test]
async fn an_unknown_operation_a_wrong_hash_a_wrong_station_and_a_wrong_state_are_lost() {
    let w = world().await;
    w.attempted(w.new_op("op-1", OpTool::Notify, "H", &w.station()))
        .await;
    // An operation for another target station: this station asks about it.
    w.attempted(w.new_op("op-beta", OpTool::Notify, "HB", &w.beta))
        .await;
    // `prepared`: admitted, never attempted. `done`: final.
    let prepared = w.new_op("op-prep", OpTool::Notify, "HP", &w.station());
    w.f.store.desktop_admit(prepared, w.now()).await.unwrap();
    w.attempted(w.new_op("op-done", OpTool::Notify, "HD", &w.station()))
        .await;
    w.f.store
        .desktop_op_finish("op-done", OpOutcome::Done(json!({})), w.now())
        .await
        .unwrap();
    for (id, hash) in [
        ("op-none", "H"),
        ("op-1", "OTHER"),
        ("op-1", ""),
        ("op-beta", "HB"),
        ("op-prep", "HP"),
        ("op-done", "HD"),
    ] {
        is(&w.check(payload(id, hash, "execute")).await, LOST);
    }
    // All of them stopped at the first local checks: no caller policy ran.
    assert_eq!(w.fake.count("coordinator.policy_now"), 0);
}

#[tokio::test]
async fn a_changed_grant_a_changed_switch_or_a_changed_link_is_lost_before_the_policy() {
    // A revoked grant.
    let w = attempted_world().await;
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    // An expired grant.
    let w = attempted_world().await;
    w.f.det.advance(Duration::from_secs(13 * 3600));
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    // A new generation of the binding.
    let w = attempted_world().await;
    w.rebind().await;
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    // Fleet control and desktop control off.
    let w = attempted_world().await;
    w.f.store.set_fleet_control(false);
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    let w = attempted_world().await;
    w.live.publish(deck_live(false));
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    // The target link lost a feature.
    let w = attempted_world().await;
    w.strip_features();
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    assert_eq!(w.fake.count("coordinator.policy_now"), 0);
}

#[tokio::test]
async fn the_caller_policy_false_is_lost_and_an_uncertain_answer_is_unknown() {
    let w = attempted_world().await;
    let cases = [
        (
            json!({"fleet_control": false, "bound": true, "binding_matches": true}),
            LOST,
        ),
        (
            json!({"fleet_control": true, "bound": false, "binding_matches": true}),
            LOST,
        ),
        (
            json!({"fleet_control": true, "bound": true, "binding_matches": false}),
            LOST,
        ),
        (json!({"fleet_control": true, "bound": true}), UNKNOWN),
        (json!({"nonsense": true}), UNKNOWN),
    ];
    for (answer, want) in cases {
        w.fake.set(except("coordinator.policy_now", move |_| {
            Some(ok(answer.clone()))
        }));
        is(&w.check(payload("op-1", "H", "execute")).await, want);
    }
    w.fake.set(except("coordinator.policy_now", |_| {
        Some(fail("forbidden", "no"))
    }));
    is(&w.check(payload("op-1", "H", "execute")).await, UNKNOWN);
    // The valid answer after all of these: the operation is still attempted.
    w.fake.set(standard());
    is(&w.check(payload("op-1", "H", "execute")).await, (true, ""));
}

#[tokio::test]
async fn a_caller_policy_that_never_answers_is_unknown_inside_the_five_second_deadline() {
    let w = attempted_world().await;
    w.fake.set(except("coordinator.policy_now", |_| None));
    let started = std::time::Instant::now();
    is(&w.check(payload("op-1", "H", "execute")).await, UNKNOWN);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2_900) && took < Duration::from_secs(5),
        "{took:?}"
    );
}

#[tokio::test]
async fn a_grant_revoked_during_the_policy_round_trip_is_lost_at_the_second_local_pass() {
    let w = attempted_world().await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "coordinator.policy_now" {
                store.revoke_grant(gid, "test".into()).await.unwrap();
            }
            Some(standard_answer(&req))
        }
    }));
    is(&w.check(payload("op-1", "H", "execute")).await, LOST);
    assert_eq!(w.fake.count("coordinator.policy_now"), 1);
}

#[tokio::test]
async fn a_malformed_check_is_unknown_and_never_an_error() {
    let w = attempted_world().await;
    let read = json!({"kind": "info"});
    for params in [
        json!({"junk": 1}),
        json!({"operation_id": "op-1", "action_hash": "H", "phase": "sideways"}),
        // A `read` with phase `execute` or `import`.
        json!({"operation_id": "op-1", "action_hash": "H", "phase": "execute", "read": read}),
        json!({"operation_id": "op-1", "action_hash": "H", "phase": "import", "read": read}),
        // Phase `read` without a `read`.
        payload("op-1", "H", "read"),
        json!({"operation_id": "op-1", "action_hash": "H", "phase": "execute", "extra": 1}),
    ] {
        is(&w.check(params).await, UNKNOWN);
    }
    assert_eq!(w.fake.count("coordinator.policy_now"), 0);
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// A correct check, then the data.
    Honest,
    /// The check with a wrong hash.
    BadHash,
    /// The check with another read object.
    BadRead,
    /// The check with another read ID.
    BadId,
    /// The correct check twice.
    Twice,
    /// No check; an error answer.
    NoCheck,
}

/// A station that answers `desktop.coordinator_read` the way `mode` says. The
/// verdicts of its checks go to `verdicts`.
fn reading(answer: Outcome, mode: Mode, verdicts: Arc<Mutex<Vec<Value>>>) -> Handler {
    let answer = Arc::new(Mutex::new(Some(answer)));
    handler(move |req| {
        let verdicts = Arc::clone(&verdicts);
        let answer = Arc::clone(&answer);
        async move {
            if req.method != "desktop.coordinator_read" {
                return Some(standard_answer(&req));
            }
            if mode == Mode::NoCheck {
                return Some(fail("failed", "no check was made"));
            }
            let id = req.params["read_id"].as_str().unwrap().to_owned();
            let (mut hash, mut read) = (
                req.params["read_hash"].as_str().unwrap().to_owned(),
                req.params["read"].clone(),
            );
            let mut asked = id.clone();
            match mode {
                Mode::BadHash => hash = "0".repeat(64),
                Mode::BadRead => read = json!({"kind": "action_status", "operation_id": "x"}),
                Mode::BadId => asked = "f".repeat(32),
                _ => {}
            }
            let check = |id: &str, hash: &str, read: &Value| {
                req.control.request(
                    "desktop.authority_check",
                    None,
                    json!({"operation_id": id, "action_hash": hash, "phase": "read", "read": read}),
                )
            };
            let Outcome::Ok(first) = check(&asked, &hash, &read).await.unwrap() else {
                panic!("a wire error");
            };
            verdicts.lock().unwrap().push(first.clone());
            if mode == Mode::Twice {
                let Outcome::Ok(second) = check(&asked, &hash, &read).await.unwrap() else {
                    panic!("a wire error");
                };
                verdicts.lock().unwrap().push(second);
            }
            if first["valid"] == true {
                answer.lock().unwrap().clone()
            } else {
                Some(fail(first["reason"].as_str().unwrap(), "no"))
            }
        }
    })
}

fn verdicts() -> Arc<Mutex<Vec<Value>>> {
    Arc::default()
}

fn seen(v: &Arc<Mutex<Vec<Value>>>) -> Vec<(bool, String)> {
    v.lock().unwrap().iter().map(verdict_of).collect()
}

#[tokio::test]
async fn desktop_info_reads_through_a_permit_and_both_policies_and_names_the_station() {
    let w = world_with(&BOTH, |_| {}).await;
    let v = verdicts();
    w.fake
        .set(reading(ok(read_info(2)), Mode::Honest, Arc::clone(&v)));
    let r = w.query(info_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let result = &r["result"];
    assert_eq!(result["station"], w.station().as_str());
    assert_eq!(result["kind"], "omarchy");
    assert_eq!(result["monitors"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["walls"],
        json!([{"display_id": "wall-1", "monitor": "DP-0"}])
    );
    assert_eq!(
        result["capabilities"],
        json!({"notify": true, "preview": true, "screenshot": true})
    );
    assert_eq!(seen(&v), [(true, String::new())]);
    // The target policy has the purpose of the grant, no monitor.
    assert_eq!(
        w.fake.params_of("desktop.policy_now"),
        [json!({"purpose": "desktop", "monitor": null})]
    );
    // The caller policy ran at the query and at the dispatch check.
    assert_eq!(w.fake.count("coordinator.policy_now"), 2);
    // One row for the query.
    let rows = w.audit_rows(Some("desktop.info")).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].1, w.b.grant_id,
        "the target of an info row is the grant ID"
    );
}

#[tokio::test]
async fn desktop_info_filters_by_the_classes_of_the_grant() {
    // A capture-only grant: capture monitors, no walls, no desktop capability.
    let w = world_with(&[GrantClass::Capture], |_| {}).await;
    w.fake
        .set(reading(ok(read_info(3)), Mode::Honest, verdicts()));
    let r = w.query(info_query(None)).await;
    let result = &r["result"];
    let names: Vec<&str> = result["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["DP-0", "DP-2"]);
    assert_eq!(result["walls"], json!([]));
    assert_eq!(
        result["capabilities"],
        json!({"notify": false, "preview": false, "screenshot": true})
    );
    assert_eq!(
        w.fake.params_of("desktop.policy_now"),
        [json!({"purpose": "capture", "monitor": null})]
    );
    // A desktop-only grant: everything but the screenshot capability.
    let w = world().await;
    w.fake
        .set(reading(ok(read_info(3)), Mode::Honest, verdicts()));
    let r = w.query(info_query(None)).await;
    assert_eq!(r["result"]["monitors"].as_array().unwrap().len(), 3);
    assert_eq!(
        r["result"]["capabilities"],
        json!({"notify": true, "preview": true, "screenshot": false})
    );
}

#[tokio::test]
async fn desktop_info_enforces_the_limits_again_and_maps_the_station_errors() {
    let w = world().await;
    w.fake
        .set(reading(ok(read_info(65)), Mode::Honest, verdicts()));
    let r = w.query(info_query(None)).await;
    assert_eq!(code(&r), "desktop_result_too_large");
    assert_eq!(
        r["message"],
        "The desktop information exceeds the tool response limit."
    );
    // A malformed answer, a code of §11 and an unknown code.
    w.fake
        .set(reading(ok(json!({"nope": 1})), Mode::Honest, verdicts()));
    assert_eq!(code(&w.query(info_query(None)).await), "desktop_failed");
    w.fake.set(reading(
        fail("desktop_unsupported", "x"),
        Mode::Honest,
        verdicts(),
    ));
    assert_eq!(
        code(&w.query(info_query(None)).await),
        "desktop_unsupported"
    );
    w.fake.set(reading(
        fail("weird_station_code", "SECRET-TEXT"),
        Mode::Honest,
        verdicts(),
    ));
    let r = w.query(info_query(None)).await;
    assert_eq!(code(&r), "desktop_failed");
    assert!(!r.to_string().contains("SECRET-TEXT"), "{r}");
}

#[tokio::test]
async fn desktop_info_refuses_a_target_outside_the_grant_and_one_without_a_link() {
    let w = world().await;
    assert_eq!(
        code(&w.query(info_query(Some("outside"))).await),
        "not_granted"
    );
    assert_eq!(
        code(&w.query(info_query(Some(w.beta.as_str()))).await),
        "station_unreachable"
    );
    w.fake.quiet(Duration::from_millis(100), &[]).await;
    // The policy answers decide before any read.
    w.fake.set(except("desktop.policy_now", |_| {
        Some(ok(
            json!({"allow_fleet_control": true, "allow_desktop": false,
            "allow_desktop_capture": false, "monitor_allowed": true}),
        ))
    }));
    assert_eq!(code(&w.query(info_query(None)).await), "not_granted");
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
    let rows = w.audit_rows(Some("desktop.info")).await;
    let last: Value = serde_json::from_str(&rows.last().unwrap().2).unwrap();
    assert_eq!(
        (last["stage"].as_str(), last["code"].as_str()),
        (Some("refused"), Some("not_granted"))
    );
}

#[tokio::test]
async fn a_read_permit_allows_exactly_one_check_and_each_failed_check_consumes_it() {
    // A wrong hash, a wrong read object and an unknown ID: lost.
    for mode in [Mode::BadHash, Mode::BadRead, Mode::BadId] {
        let w = world().await;
        let v = verdicts();
        w.fake.set(reading(ok(read_info(1)), mode, Arc::clone(&v)));
        let r = w.query(info_query(None)).await;
        assert_eq!(code(&r), "authority_lost");
        assert_eq!(seen(&v), [(false, "authority_lost".to_owned())]);
    }
    // The same check a second time: lost, though the first one was valid.
    let w = world().await;
    let v = verdicts();
    w.fake
        .set(reading(ok(read_info(1)), Mode::Twice, Arc::clone(&v)));
    let r = w.query(info_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(
        seen(&v),
        [(true, String::new()), (false, "authority_lost".to_owned())]
    );
}

#[tokio::test]
async fn a_permit_is_removed_when_the_read_ends_without_a_check() {
    let w = world().await;
    w.fake
        .set(reading(ok(read_info(1)), Mode::NoCheck, verdicts()));
    assert_eq!(code(&w.query(info_query(None)).await), "desktop_failed");
    // The station asks late, with the right read and hash: the permit is gone.
    let sent = &w.fake.params_of("desktop.coordinator_read")[0];
    let late = json!({"operation_id": sent["read_id"], "action_hash": sent["read_hash"],
        "phase": "read", "read": sent["read"]});
    is(&w.check(late).await, LOST);
}

#[tokio::test]
async fn a_binding_changed_before_the_read_check_is_lost_and_no_data_returns() {
    let w = world().await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    let v = verdicts();
    let inner = reading(ok(read_info(1)), Mode::Honest, Arc::clone(&v));
    // The target policy answer moves the binding to generation 2; the read
    // then starts.
    w.fake.set(handler(move |req| {
        let (store, gid, inner) = (store.clone(), gid.clone(), Arc::clone(&inner));
        async move {
            if req.method == "desktop.policy_now" {
                store
                    .db()
                    .call(move |conn| {
                        Ok(conn.execute(
                            "UPDATE tokens SET bound_generation = 2 WHERE grant_id = ?1",
                            [gid],
                        )?)
                    })
                    .await
                    .unwrap();
                return Some(standard_answer(&req));
            }
            inner(req).await
        }
    }));
    let r = w.query(info_query(None)).await;
    // The check of the read finds the grant gone; the station refuses to
    // return data and the code passes through.
    assert_eq!(code(&r), "authority_lost");
    assert_eq!(seen(&v), [(false, "authority_lost".to_owned())]);
    assert!(r["result"].is_null(), "{r}");
}
