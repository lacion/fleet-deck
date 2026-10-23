//! The `get_usage` plan limit windows of the Claude core (R4.5).

use std::time::{Duration, Instant};

use fleetdeck_proto::Event;
use serde_json::{Value, json};

use super::tests::{config, emits, feed, input, lifecycle, writes};
use super::*;

fn init_frame() -> Value {
    json!({"type":"system","subtype":"init","session_id":"s-1","model":"claude-haiku-4-5",
           "permissionMode":"default","claude_code_version":"2.1.296"})
}

/// The ids of the `get_usage` requests in `out`.
fn usage_ids(out: &[Output]) -> Vec<String> {
    writes(out)
        .iter()
        .filter(|w| w["request"]["subtype"] == "get_usage")
        .map(|w| w["request_id"].as_str().unwrap().to_owned())
        .collect()
}

fn answer(id: &str, response: &Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,
           "response":response}})
}

/// A started core and the id of its first `get_usage` request.
fn started() -> (ClaudeCore, String) {
    let mut core = ClaudeCore::new(config(false));
    let out = feed(&mut core, init_frame());
    let ids = usage_ids(&out);
    assert_eq!(ids.len(), 1, "{out:?}");
    (core, ids[0].clone())
}

/// A finished turn: the input starts, then its `result` arrives.
fn finish_turn(core: &mut ClaudeCore, n: u8) -> Vec<Output> {
    core.prompt(&input(n, "go")).unwrap();
    lifecycle(core, n, "started");
    feed(
        core,
        json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
               "total_cost_usd":0.01,"usage":{"input_tokens":1,"output_tokens":1}}),
    )
}

#[test]
fn the_usage_request_is_written_after_init_and_asks_to_skip_the_behaviors() {
    let mut core = ClaudeCore::new(config(false));
    let out = feed(&mut core, init_frame());
    assert_eq!(
        writes(&out),
        vec![json!({"type":"control_request","request_id":"fd-usage-1",
                    "request":{"subtype":"get_usage","skip_behaviors":true}})]
    );
    // Claude repeats `init` for each turn: no second ask.
    assert!(usage_ids(&feed(&mut core, init_frame())).is_empty());
}

#[test]
fn a_usage_answer_maps_each_window_to_a_rate_limit_with_its_reset_time() {
    let (mut core, id) = started();
    let response = json!({
    "session": {"total_cost_usd": 0.0}, "subscription_type": "max",
    "rate_limits_available": true,
    "rate_limits": {
        "five_hour": {"utilization": 42, "resets_at": "2026-10-13T20:00:00.000000+00:00"},
        "seven_day": {"utilization": 10.5, "resets_at": "2026-10-20T08:30:00.5+02:00"},
        "seven_day_opus": {"utilization": null, "resets_at": null},
        "seven_day_sonnet": null,
        "seven_day_oauth_apps": {"utilization": 250, "resets_at": "not a time"},
    }});
    let events = emits(&feed(&mut core, answer(&id, &response)));
    let [Event::UsageUpdated { rate_limits, .. }] = events.as_slice() else {
        panic!("{events:?}")
    };
    let got: Vec<_> = rate_limits
        .iter()
        .map(|r| (r.scope.as_str(), r.used_percent, r.resets_at))
        .collect();
    assert_eq!(
        got,
        vec![
            ("five_hour", Some(42.0), Some(1_791_921_600_000)),
            ("seven_day", Some(10.5), Some(1_792_477_800_500)),
            ("seven_day_oauth_apps", Some(100.0), None),
        ]
    );
}

#[test]
fn a_usage_answer_without_plan_limits_emits_nothing() {
    let (mut core, id) = started();
    let response = json!({"rate_limits_available": false, "rate_limits": null,
                          "subscription_type": null});
    assert!(emits(&feed(&mut core, answer(&id, &response))).is_empty());
    assert!(!core.usage_stopped);
}

#[test]
fn an_error_answer_stops_the_usage_requests_of_the_session() {
    let (mut core, id) = started();
    let err = json!({"type":"control_response","response":{"subtype":"error","request_id":id,
                     "error":"Unsupported control request subtype: get_usage"}});
    // No user-visible error event.
    assert!(emits(&feed(&mut core, err)).is_empty());
    core.usage_asked = Instant::now().checked_sub(Duration::from_secs(3600));
    assert!(usage_ids(&finish_turn(&mut core, 1)).is_empty());
}

#[test]
fn an_answer_in_another_shape_stops_the_usage_requests_of_the_session() {
    let (mut core, id) = started();
    assert!(emits(&feed(&mut core, answer(&id, &json!({"unexpected": true})))).is_empty());
    core.usage_asked = Instant::now().checked_sub(Duration::from_secs(3600));
    assert!(usage_ids(&finish_turn(&mut core, 1)).is_empty());
}

#[test]
fn a_usage_request_follows_a_turn_at_most_every_five_minutes_and_never_while_pending() {
    let (mut core, id) = started();
    // The ask after init is pending and recent.
    assert!(usage_ids(&finish_turn(&mut core, 1)).is_empty());
    feed(
        &mut core,
        answer(
            &id,
            &json!({"rate_limits_available": false, "rate_limits": null}),
        ),
    );
    // Answered, but only seconds old.
    assert!(usage_ids(&finish_turn(&mut core, 2)).is_empty());
    // Old enough, and the next turn asks once.
    core.usage_asked = Instant::now().checked_sub(Duration::from_secs(6 * 60));
    let ids = usage_ids(&finish_turn(&mut core, 3));
    assert_eq!(ids.len(), 1);
    // Pending again (and recent): two quick completions write one request only.
    assert!(usage_ids(&finish_turn(&mut core, 4)).is_empty());
    core.usage_asked = Instant::now().checked_sub(Duration::from_secs(6 * 60));
    assert!(
        usage_ids(&finish_turn(&mut core, 5)).is_empty(),
        "still pending"
    );
}

#[test]
fn the_usage_state_is_not_lost_by_a_checkpoint() {
    let (mut core, id) = started();
    feed(&mut core, answer(&id, &json!({"nope": 1})));
    let stored = core.checkpoint().unwrap();
    let mut back = ClaudeCore::restore(config(false), &stored).unwrap();
    assert!(back.usage_stopped);
    assert!(usage_ids(&back.usage_request()).is_empty());
}

/// The cost parts of the `usage.updated` events in `out`.
fn costs(out: &[Output]) -> Vec<f64> {
    emits(out)
        .iter()
        .filter_map(|e| match e {
            Event::UsageUpdated { usage, .. } => usage.cost_usd,
            _ => None,
        })
        .collect()
}

/// A finished turn whose `result` reports `total` as the process total.
fn turn_with_total(core: &mut ClaudeCore, n: u8, total: f64) -> Vec<f64> {
    core.prompt(&input(n, "go")).unwrap();
    lifecycle(core, n, "started");
    costs(&feed(
        core,
        json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
               "total_cost_usd":total,"usage":{"input_tokens":1,"output_tokens":1}}),
    ))
}

#[test]
fn a_result_adds_the_growth_of_the_process_total_not_the_total_again() {
    let (mut core, _) = started();
    let mut parts = turn_with_total(&mut core, 1, 0.50);
    parts.extend(turn_with_total(&mut core, 2, 0.80));
    assert_eq!(parts.len(), 2, "{parts:?}");
    // The fold sums the parts: the session costs 0.80, as the CLI says, not 1.30.
    let sum: f64 = parts.iter().sum();
    assert!((sum - 0.80).abs() < 1e-9, "{parts:?}");
}

#[test]
fn a_resumed_process_adds_nothing_for_the_cost_that_it_restored() {
    let mut config = config(false);
    config.resumed = Some(true);
    let mut core = ClaudeCore::new(config);
    feed(&mut core, init_frame());
    // The CLI restored 403.62 from an earlier process, which reported it.
    assert_eq!(turn_with_total(&mut core, 1, 403.62), vec![0.0]);
    let next = turn_with_total(&mut core, 2, 404.12);
    assert!((next[0] - 0.50).abs() < 1e-9, "{next:?}");
}
