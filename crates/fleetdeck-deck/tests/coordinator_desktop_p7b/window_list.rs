//! The `window_list` protected read (R13 P7b §5.2, §6, §6.1, §11.5, task K1):
//! the projection of the station, the rates, the one read in flight for each
//! target, the limits of the answer, the checks of the read phase and of the
//! delivery, and the audit row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::coordinator_desktop_support::*;

/// A world of the broader scope whose station answers `listing`.
async fn listing_world(listing: impl FnOnce(&World) -> Value) -> (World, Arc<Verdicts>) {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        listing(&w),
        Arc::clone(&verdicts),
        Around::default(),
    ));
    (w, verdicts)
}

type Verdicts = std::sync::Mutex<Vec<Value>>;

#[tokio::test]
async fn the_station_projection_is_returned_unchanged_and_the_read_uses_no_budget() {
    let (w, verdicts) = listing_world(|w| window_list_value(w.station().as_str(), 2)).await;
    let listing = window_list_value(w.station().as_str(), 2);
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r, json!({"status": "ok", "result": listing}), "{r}");
    // The target policy has the broader purpose and no monitor; the read is
    // the closed `input_windows` read, and its check was valid.
    assert_eq!(
        w.fake.params_of("desktop.policy_now"),
        [json!({"purpose": "input_any", "monitor": null})]
    );
    assert_eq!(
        w.fake.params_of("desktop.coordinator_read")[0]["read"],
        json!({"kind": "input_windows"})
    );
    assert_eq!(seen_verdicts(&verdicts), [(true, String::new())]);
    // No mutation: no act, no operation row, no budget, no notification.
    assert_eq!(w.fake.count("desktop.act"), 0);
    assert_eq!(w.fake.count("desktop.notify"), 0);
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    // No sign-out.
    let signed =
        w.f.store
            .audit_log(100)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == "board.signed_out_for_input")
            .count();
    assert_eq!(signed, 0);
}

#[tokio::test]
async fn the_window_list_read_carries_the_coordinator_context_and_an_info_read_does_not() {
    let (w, _) = listing_world(|w| window_list_value(w.station().as_str(), 1)).await;
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let sent = &w.fake.params_of("desktop.coordinator_read")[0];
    // The context of the authorization: the same one that `desktop.act`
    // carries.
    assert_eq!(
        sent["context"],
        json!({
            "grant_id": w.b.grant_id,
            "binding_id": w.b.binding_id,
            "generation": w.b.generation,
            "caller_station": w.station().as_str(),
            "caller_session": "coord-1",
            "caller_label": "swift-fox",
        })
    );
    assert_eq!(sent["read"], json!({"kind": "input_windows"}));
    // The same context as an input action of the same grant.
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let act = &w.fake.params_of("desktop.act")[0];
    assert_eq!(act["authority"]["coordinator"], sent["context"], "{act}");
    // A `desktop_info` read carries no `context` key.
    w.fake.set(p7b_except("desktop.coordinator_read", |_| {
        Some(ok(read_info(1)))
    }));
    let r = w.query(info_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let info_read = &w.fake.params_of("desktop.coordinator_read")[1];
    assert_eq!(info_read["read"], json!({"kind": "info"}));
    assert!(info_read.get("context").is_none(), "{info_read}");
}

#[tokio::test]
async fn the_target_station_argument_is_the_station_of_the_list() {
    let mut w = p7b_world(InputScope::AnyWindow, false).await;
    let beta_fake = w.link_beta().await;
    let beta = w.beta.clone();
    let verdicts = new_verdicts();
    beta_fake.set(list_station(
        window_list_value(beta.as_str(), 1),
        Arc::clone(&verdicts),
        Around::default(),
    ));
    let r = w.query(window_list_query(Some(beta.as_str()))).await;
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["result"]["station"], beta.as_str());
    assert_eq!(beta_fake.count("desktop.coordinator_read"), 1);
}

#[tokio::test]
async fn the_audit_row_has_the_count_and_no_geometry() {
    let (w, _) = listing_world(|w| window_list_value(w.station().as_str(), 3)).await;
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = rows[0].as_object().unwrap();
    assert_eq!(row["stage"], "outcome");
    assert_eq!(row["code"], Value::Null);
    assert_eq!(row["windows"], 3);
    assert_eq!(row["grant_id"], w.b.grant_id.as_str());
    assert_eq!(row["target_station"], w.station().as_str());
    let mut keys: Vec<&str> = row.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "caller_session",
            "caller_station",
            "code",
            "generation",
            "grant_id",
            "operation_id",
            "stage",
            "target_station",
            "windows"
        ]
    );
    let dump = w.audit_text().await;
    for secret in [
        format!("{:032x}", 0),
        format!("{:032x}", 1),
        "rect".to_owned(),
        "width".to_owned(),
        "DP-1".to_owned(),
        "640".to_owned(),
    ] {
        assert!(!dump.contains(&secret), "{secret} in {dump}");
    }
}

#[tokio::test]
async fn the_eleventh_read_in_sixty_seconds_is_rate_limited() {
    let (w, _) = listing_world(|w| window_list_value(w.station().as_str(), 1)).await;
    for i in 0..10 {
        let r = w.query(window_list_query(None)).await;
        assert_eq!(r["status"], "ok", "{i}: {r}");
    }
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "rate_limited");
    assert_eq!(
        r["message"],
        "This grant's desktop rate limit has been reached. Try again later."
    );
    // The refused read asked nothing of the station.
    assert_eq!(w.fake.count("desktop.coordinator_read"), 10);
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows.len(), 11);
    assert_eq!(rows[10]["stage"], "refused");
    assert_eq!(rows[10]["code"], "rate_limited");
    assert!(rows[10].get("windows").is_none());
    // A minute later the rate opens again.
    w.f.det.advance(std::time::Duration::from_secs(61));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
}

#[tokio::test]
async fn a_second_read_of_the_same_target_is_busy_at_once_and_the_slot_is_released() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        Arc::clone(&verdicts),
        Around {
            before_check: Some(step_fn(move || {
                let gate = Arc::clone(&gate);
                async move { gate.notified().await }
            })),
            after_check: None,
        },
    ));
    let first = w.query(window_list_query(None));
    let second = async {
        w.fake.wait_for("desktop.coordinator_read", 1).await;
        let r = w.query(window_list_query(None)).await;
        release.notify_one();
        r
    };
    let (first, second) = tokio::join!(first, second);
    assert_eq!(code(&second), "busy", "{second}");
    assert_eq!(
        second["message"],
        fleetdeck_proto::coordinator_desktop_text::text("busy").unwrap()
    );
    assert_eq!(first["status"], "ok", "{first}");
    // Only the first read reached the station; a third one is admitted.
    assert_eq!(w.fake.count("desktop.coordinator_read"), 1);
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        new_verdicts(),
        Around::default(),
    ));
    let third = w.query(window_list_query(None)).await;
    assert_eq!(third["status"], "ok", "{third}");
    let rows = audit_details(&w, "desktop.window_list").await;
    assert!(
        rows.iter()
            .any(|d| d["stage"] == "refused" && d["code"] == "busy"),
        "{rows:?}"
    );
}

#[tokio::test]
async fn an_answer_over_a_limit_or_not_valid_is_refused_and_returns_no_list() {
    // 65 windows.
    let (w, _) = listing_world(|w| window_list_value(w.station().as_str(), 65)).await;
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_result_too_large", "{r}");
    assert!(r.get("result").is_none());
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows[0]["stage"], "outcome");
    assert_eq!(rows[0]["code"], "desktop_result_too_large");
    // A zero width (the slot of the first failure was released).
    let mut zero = window_list_value(w.station().as_str(), 2);
    zero["windows"][1]["rect"]["width"] = json!(0);
    w.fake
        .set(list_station(zero, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    // A zero monitor height.
    let mut zero = window_list_value(w.station().as_str(), 1);
    zero["monitors"][0]["height"] = json!(0);
    w.fake
        .set(list_station(zero, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    // The station names itself by its readable name: the deck answers the
    // target ID instead (as `desktop_info`), never the station's own value.
    let named = window_list_value("Lighting", 1);
    w.fake
        .set(list_station(named, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r["result"]["station"], w.station().as_str(), "{r}");
    assert!(!r.to_string().contains("Lighting"));
    // A handle that is not of the P7 form.
    let mut bad = window_list_value(w.station().as_str(), 1);
    bad["windows"][0]["window_id"] = json!("NOT-A-HANDLE");
    w.fake
        .set(list_station(bad, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    // A window on a monitor that the list does not name.
    let mut stray = window_list_value(w.station().as_str(), 1);
    stray["windows"][0]["monitors"] = json!(["HDMI-9"]);
    w.fake
        .set(list_station(stray, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    // An answer with an unknown field is not the closed projection.
    let mut open = window_list_value(w.station().as_str(), 1);
    open["windows"][0]["title"] = json!("SENTINEL-TITLE");
    w.fake
        .set(list_station(open, new_verdicts(), Around::default()));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    assert!(!r.to_string().contains("SENTINEL"));
    assert!(!w.audit_text().await.contains("SENTINEL"));
}

#[tokio::test]
async fn a_revoke_before_the_delivery_refuses_and_returns_no_list() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 2),
        Arc::clone(&verdicts),
        Around {
            before_check: None,
            after_check: Some(step_fn(move || {
                let (store, gid) = (store.clone(), gid.clone());
                async move {
                    store.revoke_grant(gid, "test".into()).await.unwrap();
                }
            })),
        },
    ));
    let r = w.query(window_list_query(None)).await;
    // The check was valid; the delivery pass refuses with its code.
    assert_eq!(seen_verdicts(&verdicts), [(true, String::new())]);
    assert_eq!(code(&r), "not_granted", "{r}");
    assert!(r.get("result").is_none());
    assert!(!r.to_string().contains(&format!("{:032x}", 0)));
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows[0]["stage"], "refused");
    assert_eq!(rows[0]["code"], "not_granted");
    assert!(rows[0].get("windows").is_none());
}

#[tokio::test]
async fn the_allowance_withdrawn_before_the_delivery_refuses_with_input_not_allowed() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        new_verdicts(),
        Around {
            before_check: None,
            after_check: Some(step_fn(move || {
                let (store, gid) = (store.clone(), gid.clone());
                async move {
                    exec_tokens(&store, &gid, "allow_input = 0, input_scope = NULL").await;
                }
            })),
        },
    ));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "input_not_allowed", "{r}");
    // The scope changed to `own_windows`: refused with its own code.
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        new_verdicts(),
        Around {
            before_check: None,
            after_check: Some(step_fn(move || {
                let (store, gid) = (store.clone(), gid.clone());
                async move {
                    exec_tokens(&store, &gid, "input_scope = 'own_windows'").await;
                }
            })),
        },
    ));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "input_scope_not_allowed", "{r}");
}

#[tokio::test]
async fn the_read_phase_check_applies_the_window_list_rules() {
    // The scope is `own_windows` when the target checks: the permit has the
    // input class and its read runs the rules of `window_list`.
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        Arc::clone(&verdicts),
        Around {
            before_check: Some(step_fn(move || {
                let (store, gid) = (store.clone(), gid.clone());
                async move {
                    exec_tokens(&store, &gid, "input_scope = 'own_windows'").await;
                }
            })),
            after_check: None,
        },
    ));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(
        seen_verdicts(&verdicts),
        [(false, "authority_lost".to_owned())]
    );
    assert_eq!(code(&r), "authority_lost", "{r}");
    assert!(r.get("result").is_none());
    // A target link that loses `desktop_input_any.v1` at the check.
    let w = p7b_world(InputScope::AnyWindow, false).await;
    let (store, station) = (w.f.store.clone(), w.f.station.clone());
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        Arc::clone(&verdicts),
        Around {
            before_check: Some(step_fn(move || {
                let (store, station) = (store.clone(), station.clone());
                async move {
                    store.set_link_features(
                        &station,
                        97,
                        without_feature(feature::DESKTOP_INPUT_ANY),
                    );
                }
            })),
            after_check: None,
        },
    ));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(
        seen_verdicts(&verdicts),
        [(false, "authority_lost".to_owned())]
    );
    assert_eq!(code(&r), "authority_lost", "{r}");
}

#[tokio::test]
async fn a_closed_gate_or_a_refused_policy_asks_the_station_for_no_data() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    w.fake.set(p7b_except("desktop.policy_now", |_| {
        Some(ok(gates_policy(
            "input_any",
            Some("allow_desktop_input_any"),
        )))
    }));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "input_any_disabled", "{r}");
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows[0]["stage"], "refused");
    assert_eq!(rows[0]["code"], "input_any_disabled");
    // The P7 flags.
    w.fake.set(p7b_except("desktop.policy_now", |_| {
        Some(ok(gates_policy("input_any", Some("allow_desktop"))))
    }));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "not_granted", "{r}");
    // An answer that is not the gates answer of the purpose.
    w.fake.set(p7b_except("desktop.policy_now", |_| {
        Some(ok(input_policy(None)))
    }));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "authority_unknown", "{r}");
    assert_eq!(w.fake.count("desktop.coordinator_read"), 0);
}

#[tokio::test]
async fn a_station_refusal_of_the_read_reaches_the_caller_with_its_code_and_text() {
    use fleetdeck_proto::coordinator_desktop_text::text;
    let w = p7b_world(InputScope::AnyWindow, false).await;
    // (the wire code, the fixed text).
    let cases = [
        ("input_any_disabled", text("input_any_disabled").unwrap()),
        (
            "screen_locked",
            fleetdeck_proto::capture::text::SCREEN_LOCKED,
        ),
        (
            "input_scope_not_allowed",
            text("input_scope_not_allowed").unwrap(),
        ),
        ("input_not_allowed", text("input_not_allowed").unwrap()),
        ("busy", text("busy").unwrap()),
        (
            "desktop_result_too_large",
            text("desktop_result_too_large").unwrap(),
        ),
        (
            "desktop_window_not_found",
            "No input window available to this grant matches that handle.",
        ),
        ("desktop_control_off", text("desktop_control_off").unwrap()),
        ("desktop_unsupported", text("desktop_unsupported").unwrap()),
    ];
    for (wire, expected) in cases {
        w.fake.set(p7b_except("desktop.coordinator_read", move |_| {
            Some(fail(wire, "SENTINEL-wire-text"))
        }));
        // The read rate is ten in a window: move the clock on.
        w.f.det.advance(std::time::Duration::from_secs(61));
        let r = w.query(window_list_query(None)).await;
        assert_eq!(code(&r), wire, "{r}");
        assert_eq!(r["message"], expected, "{wire}");
        assert!(!r.to_string().contains("SENTINEL"));
    }
    let rows = audit_details(&w, "desktop.window_list").await;
    assert_eq!(rows[0]["stage"], "outcome");
    assert_eq!(rows[0]["code"], "input_any_disabled");
}

#[tokio::test]
async fn any_other_unknown_code_of_the_read_stays_desktop_failed() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    for wire in [
        "input_destination_unavailable",
        "focus_changed",
        "no_such_code",
    ] {
        w.fake.set(p7b_except("desktop.coordinator_read", move |_| {
            Some(fail(wire, "x"))
        }));
        let r = w.query(window_list_query(None)).await;
        assert_eq!(code(&r), "desktop_failed", "{wire}: {r}");
    }
    // The pass-through is for the `window_list` read only: a `desktop_info`
    // read keeps the mapping.
    w.fake.set(p7b_except("desktop.coordinator_read", |_| {
        Some(fail("input_any_disabled", "x"))
    }));
    let r = w.query(info_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
}

#[tokio::test]
async fn a_malformed_station_answer_and_a_station_error_are_desktop_failed() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    w.fake.set(p7b_except("desktop.coordinator_read", |_| {
        Some(ok(json!({"unexpected": true})))
    }));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    w.fake.set(p7b_except("desktop.coordinator_read", |_| {
        Some(fail("SENTINEL-code", "SENTINEL-text"))
    }));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "desktop_failed", "{r}");
    assert!(!r.to_string().contains("SENTINEL"));
}

#[tokio::test]
async fn a_grant_lists_with_or_without_the_click_allowance() {
    for click in [false, true] {
        let w = p7b_world(InputScope::AnyWindow, click).await;
        w.fake.set(list_station(
            window_list_value(w.station().as_str(), 1),
            new_verdicts(),
            Around::default(),
        ));
        let r = w.query(window_list_query(None)).await;
        assert_eq!(r["status"], "ok", "{click}: {r}");
    }
}
