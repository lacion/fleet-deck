use std::os::unix::fs::PermissionsExt;

use fleetdeck_proto::recording::StopReason;
use serde_json::{Value, json};

use super::*;
use crate::artifacts::capture_import::tests::FP;
use crate::desktop::recording::rig_tests::{ID, ID2, T, open};
use crate::store::capture_ops::{Begin, RecordingBegin, StopIntent};

const OTHER: &str = "cd00000000000000000000000000000000000000000000000000000000000002";

fn caller<'a>(t: &'a T, deck_fp: &'a str) -> Caller<'a> {
    Caller {
        desktop: t.desktop(),
        service: &t.s.service,
        deck_fp,
        video: true,
    }
}

fn value(o: Outcome) -> Value {
    match o {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("a wire error: {} {}", e.code, e.message),
    }
}

fn wire(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("an answer: {v}"),
    }
}

/// A running recording row without a recorder.
async fn row_only(t: &T, id: &str, hash: &str) {
    let begin = t
        .store()
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: id,
            action_hash: hash,
            monitor: "DP-1",
            artifact_id: &format!("ART{id}"),
            seconds: 5,
            link_epoch: "e",
            coordinator: None,
            now: 1,
        })
        .await
        .unwrap();
    assert!(matches!(begin, Begin::Started));
}

#[test]
fn a_request_over_the_policy_length_is_refused_with_its_text() {
    use crate::manager::policy_hub::test_config;
    let root = tempfile::tempdir().unwrap();
    let policy = Policy::from_config(&test_config(
        root.path(),
        "[capture]\nrecording_max_seconds = 600\n",
    ));
    assert!(too_long(&policy, 1).is_none());
    assert!(too_long(&policy, 600).is_none());
    let e = too_long(&policy, 601).unwrap();
    assert_eq!(e.code, "recording_too_long");
    assert_eq!(e.message, text::RECORDING_TOO_LONG);
    assert!(too_long(&policy.with_recording_max_seconds(3600), 3600).is_none());
}

fn allowed_open() -> Allowed {
    open()
}

fn broken_pgrep(t: &T) {
    let pgrep = t.dir().join("bin/pgrep");
    std::fs::write(&pgrep, "#!/bin/sh\nexit 2\n").unwrap();
    std::fs::set_permissions(&pgrep, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn active_answers_idle_without_a_recording() {
    let t = T::new().await;
    let c = caller(&t, FP);
    assert_eq!(
        value(active(&c, &json!({})).await),
        json!({"state": "idle"})
    );
    assert_eq!(
        value(active(&c, &Value::Null).await),
        json!({"state": "idle"})
    );
    // A screenshot that holds the slot also answers idle.
    let guard = t.desktop().hold_screenshot();
    assert_eq!(
        value(active(&c, &json!({})).await),
        json!({"state": "idle"})
    );
    drop(guard);
}

#[tokio::test]
async fn active_refuses_params() {
    let t = T::new().await;
    let (code, _) = wire(active(&caller(&t, FP), &json!({"x": 1})).await);
    assert_eq!(code, "invalid_params");
}

#[tokio::test]
async fn active_answers_the_recording_of_this_deck_with_its_progress() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    let answer = value(active(&caller(&t, FP), &json!({})).await);
    assert_eq!(answer["state"], "active");
    assert_eq!(answer["capture_id"], ID);
    assert_eq!(answer["status"]["state"], "pending");
    assert_eq!(answer["status"]["recording"]["phase"], "recording");
    assert_eq!(answer["status"]["recording"]["requested_seconds"], 60);
    // Another deck sees no recording.
    assert_eq!(
        value(active(&caller(&t, OTHER), &json!({})).await),
        json!({"state": "idle"})
    );
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    run.await.unwrap().unwrap();
    assert_eq!(
        value(active(&caller(&t, FP), &json!({})).await),
        json!({"state": "idle"})
    );
}

#[tokio::test]
async fn stop_takes_only_the_reasons_of_the_deck() {
    let t = T::new().await;
    row_only(&t, ID, "h").await;
    let c = caller(&t, FP);
    for reason in [
        "duration",
        "policy_off",
        "screen_locked",
        "monitor_changed",
        "link_closed",
        "lease_expired",
        "lease_denied",
        "output_limit",
        "disk_reserve",
        "recorder_exited",
        "station_shutdown",
        "recording_interrupted",
        "nonsense",
    ] {
        let (code, _) = wire(stop(&c, json!({"capture_id": ID, "reason": reason})).await);
        assert_eq!(code, "invalid_params", "{reason}");
    }
    for params in [
        json!({"capture_id": ID}),
        json!({"capture_id": ID, "reason": "owner_stop", "extra": 1}),
        json!({"capture_id": "not-an-id", "reason": "owner_stop"}),
        json!({}),
    ] {
        let (code, _) = wire(stop(&c, params).await);
        assert_eq!(code, "invalid_params");
    }
    // Nothing was latched.
    assert_eq!(t.row(ID).await.unwrap().stop_intent, StopIntent::None);
}

#[tokio::test]
async fn stop_of_an_unknown_capture_or_another_decks_is_not_found() {
    let t = T::new().await;
    let params = json!({"capture_id": ID, "reason": "owner_stop"});
    let (code, _) = wire(stop(&caller(&t, FP), params.clone()).await);
    assert_eq!(code, "not_found");
    row_only(&t, ID, "h").await;
    let (code, _) = wire(stop(&caller(&t, OTHER), params).await);
    assert_eq!(code, "not_found");
    assert_eq!(t.row(ID).await.unwrap().stop_intent, StopIntent::None);
}

#[tokio::test]
async fn stop_of_a_screenshot_is_not_recording() {
    let t = T::new().await;
    t.store()
        .capture_begin(FP, ID, "h", "DP-1", "ART", 1)
        .await
        .unwrap();
    let params = json!({"capture_id": ID, "reason": "owner_stop"});
    let (code, message) = wire(stop(&caller(&t, FP), params).await);
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("not_recording", text::NOT_RECORDING)
    );
}

#[tokio::test]
async fn stop_without_a_live_handle_writes_the_latch_and_answers_the_status() {
    let t = T::new().await;
    row_only(&t, ID, "h").await;
    let params = json!({"capture_id": ID, "reason": "deck_policy"});
    let answer = value(stop(&caller(&t, FP), params).await);
    assert_eq!(answer["state"], "pending");
    assert_eq!(answer["capture_id"], ID);
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.stop_intent, StopIntent::Discard);
    assert_eq!(row.stop_reason, Some(StopReason::DeckPolicy));
}

#[tokio::test]
async fn stop_of_a_live_recording_saves_and_a_repeat_answers_the_terminal_status() {
    let t = T::new().await;
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    let c = caller(&t, FP);
    let params = json!({"capture_id": ID, "reason": "owner_stop"});
    let answer = value(stop(&c, params.clone()).await);
    assert_eq!(answer["state"], "pending");
    assert_eq!(answer["capture_id"], ID);
    run.await.unwrap().unwrap();
    // The stop is idempotent: a terminal operation answers its status.
    let again = value(stop(&c, params).await);
    assert_eq!(again["state"], "committed");
    assert_eq!(again["stop_reason"], "owner_stop");
    assert_eq!(again["published"]["artifact"]["kind"], "video");
    // A discard reason on a committed operation changes nothing.
    let late = json!({"capture_id": ID, "reason": "deck_policy"});
    assert_eq!(value(stop(&c, late).await)["state"], "committed");
    assert_eq!(
        t.row(ID).await.unwrap().stop_reason,
        Some(StopReason::OwnerStop)
    );
}

#[tokio::test]
async fn stop_of_a_failed_recording_answers_its_failure() {
    let t = T::new().await;
    t.flag("exit_early");
    assert!(t.run(ID, "DP-1", 60).await.is_err());
    let params = json!({"capture_id": ID, "reason": "owner_stop"});
    let answer = value(stop(&caller(&t, FP), params).await);
    assert_eq!(answer["state"], "failed");
    assert_eq!(answer["error"], "recording_failed");
    assert_eq!(answer["stop_reason"], "recorder_exited");
}

fn policy_params(id: &str, hash: &str) -> Value {
    json!({"capture_id": id, "action_hash": hash})
}

#[tokio::test]
async fn policy_now_answers_the_live_policy_of_this_deck_and_hash() {
    let t = T::new().await;
    row_only(&t, ID, "the-hash").await;
    let c = caller(&t, FP);
    let answer =
        value(policy_now(&c, &allowed_open, "epoch-7", policy_params(ID, "the-hash")).await);
    assert_eq!(
        answer,
        json!({
            "valid": true,
            "desktop_epoch": "epoch-7",
            "allow_desktop": true,
            "allow_desktop_capture": true,
            "monitor_allowed": true,
            "monitor_present": true,
            "unlocked": true
        })
    );
}

#[tokio::test]
async fn policy_now_for_another_deck_or_hash_or_capture_is_not_found() {
    let t = T::new().await;
    row_only(&t, ID, "the-hash").await;
    for (c, params) in [
        (caller(&t, OTHER), policy_params(ID, "the-hash")),
        (caller(&t, FP), policy_params(ID, "another-hash")),
        (caller(&t, FP), policy_params(ID2, "the-hash")),
    ] {
        let (code, _) = wire(policy_now(&c, &allowed_open, "e", params).await);
        assert_eq!(code, "not_found");
    }
    let params = json!({"capture_id": ID});
    let (code, _) = wire(policy_now(&caller(&t, FP), &allowed_open, "e", params).await);
    assert_eq!(code, "invalid_params");
}

#[tokio::test]
async fn policy_now_is_not_valid_for_a_locked_unknown_or_missing_state() {
    let t = T::new().await;
    row_only(&t, ID, "h").await;
    let c = caller(&t, FP);
    let ask = || policy_now(&c, &allowed_open, "e", policy_params(ID, "h"));
    t.flag("hyprlock");
    let a = value(ask().await);
    assert_eq!(
        (&a["unlocked"], &a["valid"]),
        (&json!(false), &json!(false))
    );
    assert_eq!(a["monitor_present"], true);
    // An unknown lock state is not unlocked.
    std::fs::remove_file(t.dir().join("hyprlock")).unwrap();
    broken_pgrep(&t);
    let a = value(ask().await);
    assert_eq!(
        (&a["unlocked"], &a["valid"]),
        (&json!(false), &json!(false))
    );
    // A monitor that is gone.
    t.set_monitors("[]");
    let a = value(ask().await);
    assert_eq!(a["monitor_present"], false);
    assert_eq!(a["valid"], false);
}

#[tokio::test]
async fn policy_now_reads_the_settings_after_the_probe() {
    let t = T::new().await;
    row_only(&t, ID, "h").await;
    let c = caller(&t, FP);
    for (desktop, capture, monitors, mon) in [
        (false, true, vec![], true),
        (true, false, vec![], true),
        (true, true, vec!["HDMI-A-1".to_owned()], false),
        (true, true, vec!["DP-1".to_owned()], true),
    ] {
        let allowed = move || Allowed {
            desktop,
            capture,
            monitors: monitors.clone(),
        };
        let a = value(policy_now(&c, &allowed, "e", policy_params(ID, "h")).await);
        assert_eq!(a["allow_desktop"], desktop);
        assert_eq!(a["allow_desktop_capture"], capture);
        assert_eq!(a["monitor_allowed"], mon);
        assert_eq!(a["valid"], desktop && capture && mon);
    }
}
