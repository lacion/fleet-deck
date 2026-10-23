//! Codex token usage as per-thread running totals (B14, DL-445). The frames
//! are the recordings of 2026-10-23 (`codex` 0.163.0): one thread with
//! three turns, its resume, a fork, and a sub-agent thread.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{USAGE_SCHEME, Usage};
use serde_json::{Value, json};

use super::tests::{T, complete, config, emits, notify, ready_core, start_turn};
use super::thread_usage::{ThreadUsage, TokenTotals};
use super::*;
use crate::checkpoint::AdapterCheckpoint;
use crate::prices::PRICE_TABLE_VERSION;

const CHILD: &str = "thr-2";

/// A token breakdown: `[input (with cached), cached, output]`.
fn block(b: [u64; 3]) -> Value {
    json!({
        "totalTokens": b[0] + b[2],
        "inputTokens": b[0],
        "cachedInputTokens": b[1],
        "cacheWriteInputTokens": 0,
        "outputTokens": b[2],
        "reasoningOutputTokens": 0,
    })
}

fn update_params(thread: &str, total: Option<[u64; 3]>, last: [u64; 3]) -> Value {
    let mut usage = json!({"last": block(last), "modelContextWindow": 258_400});
    if let Some(total) = total {
        usage["total"] = block(total);
    }
    json!({"threadId": thread, "turnId": "n1", "tokenUsage": usage})
}

fn update(
    core: &mut CodexCore,
    thread: &str,
    total: Option<[u64; 3]>,
    last: [u64; 3],
) -> Vec<Usage> {
    let out = notify(
        core,
        "thread/tokenUsage/updated",
        update_params(thread, total, last),
    );
    emits(&out)
        .into_iter()
        .filter_map(|e| match e {
            Event::UsageUpdated { usage, .. } => Some(usage),
            _ => None,
        })
        .collect()
}

/// Every token part of a usage, as the provider's `totalTokens`.
fn tokens(u: &Usage) -> u64 {
    u.input_tokens.unwrap_or(0)
        + u.cache_read_tokens.unwrap_or(0)
        + u.cache_write_tokens.unwrap_or(0)
        + u.output_tokens.unwrap_or(0)
}

fn one(usages: Vec<Usage>) -> Usage {
    assert_eq!(usages.len(), 1, "{usages:?}");
    usages.into_iter().next().unwrap()
}

fn turn_started(core: &mut CodexCore, thread: &str, native: &str) {
    notify(
        core,
        "turn/started",
        json!({"threadId": thread, "turn": {"id": native}}),
    );
}

fn crash_and_restore(core: &CodexCore, strip_usage: bool) -> CodexCore {
    let mut checkpoint = core.checkpoint().unwrap();
    if strip_usage {
        checkpoint.state.as_object_mut().unwrap().remove("usage");
    }
    let text = checkpoint.to_json().unwrap();
    let stored = AdapterCheckpoint::from_json(&text).unwrap();
    CodexCore::restore(config("/w"), &stored).unwrap()
}

#[test]
fn the_total_grows_by_the_turn() {
    let mut core = ready_core();
    let mut sum = 0;
    turn_started(&mut core, T, "n1");
    sum += tokens(&one(update(
        &mut core,
        T,
        Some([13519, 11264, 5]),
        [13519, 11264, 5],
    )));
    complete(&mut core, "n1", "completed");
    turn_started(&mut core, T, "n2");
    sum += tokens(&one(update(
        &mut core,
        T,
        Some([28553, 24576, 10]),
        [15034, 13312, 5],
    )));
    complete(&mut core, "n2", "completed");
    turn_started(&mut core, T, "n3");
    let last = one(update(
        &mut core,
        T,
        Some([43610, 39424, 15]),
        [15057, 14848, 5],
    ));
    assert_eq!(tokens(&last), 15_062);
    sum += tokens(&last);
    assert_eq!(sum, 43_625);
}

#[test]
fn a_resume_replay_counts_nothing() {
    let mut core = ready_core();
    // Right after `thread/resume`, before any `turn/started`: the old turn's
    // `last` and the restored `total`.
    assert!(update(&mut core, T, Some([43610, 39424, 15]), [15057, 14848, 5]).is_empty());
    turn_started(&mut core, T, "n1");
    let first = one(update(
        &mut core,
        T,
        Some([58690, 54272, 20]),
        [15080, 14848, 5],
    ));
    assert_eq!(tokens(&first), 15_085);
    complete(&mut core, "n1", "completed");
    turn_started(&mut core, T, "n2");
    let second = one(update(
        &mut core,
        T,
        Some([73793, 69120, 25]),
        [15103, 14848, 5],
    ));
    assert_eq!(tokens(&second), 15_108);
}

#[test]
fn a_fork_replay_counts_nothing() {
    // `fork_thread.jsonl` lines 44-58: the new thread's first update carries
    // the parent's total, before the first `turn/started`.
    let mut core = ready_core();
    assert!(update(&mut core, T, Some([12012, 9984, 6]), [12012, 9984, 6]).is_empty());
    turn_started(&mut core, T, "n1");
    let next = one(update(
        &mut core,
        T,
        Some([24030, 19968, 12]),
        [12018, 9984, 6],
    ));
    assert_eq!(tokens(&next), 12_024);
}

#[test]
fn a_sub_agent_thread_is_counted() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    let mut sum = 0;
    sum += tokens(&one(update(
        &mut core,
        T,
        Some([13542, 11264, 40]),
        [13542, 11264, 40],
    )));
    turn_started(&mut core, CHILD, "c1");
    sum += tokens(&one(update(
        &mut core,
        CHILD,
        Some([15064, 11264, 5]),
        [15064, 11264, 5],
    )));
    notify(
        &mut core,
        "turn/completed",
        json!({"threadId": CHILD, "turn": {"id": "c1", "status": "completed"}}),
    );
    // The sub-agent never ends the session's turn.
    assert!(core.current_turn().is_some());
    sum += tokens(&one(update(
        &mut core,
        T,
        Some([27145, 22528, 62]),
        [13603, 11264, 22],
    )));
    assert_eq!(sum, 42_276);
}

#[test]
fn a_sub_agent_update_never_starts_the_turn_state() {
    // A sub-agent update before the own `turn/started` is a new thread, and
    // the own replay base still counts nothing afterwards.
    let mut core = ready_core();
    assert_eq!(
        tokens(&one(update(
            &mut core,
            CHILD,
            Some([100, 0, 10]),
            [100, 0, 10]
        ))),
        110
    );
    assert!(update(&mut core, T, Some([500, 0, 5]), [500, 0, 5]).is_empty());
}

#[test]
fn a_lower_total_is_a_new_count() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    assert_eq!(
        tokens(&one(update(
            &mut core,
            T,
            Some([1000, 0, 10]),
            [1000, 0, 10]
        ))),
        1010
    );
    // The CLI count started again: the whole new total is the delta.
    assert_eq!(
        tokens(&one(update(&mut core, T, Some([400, 0, 4]), [400, 0, 4]))),
        404
    );
    // An unchanged total adds nothing.
    assert!(update(&mut core, T, Some([400, 0, 4]), [400, 0, 4]).is_empty());
}

#[test]
fn without_total_the_last_block_counts() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    assert_eq!(tokens(&one(update(&mut core, T, None, [700, 100, 7]))), 707);
    assert_eq!(tokens(&one(update(&mut core, T, None, [700, 100, 7]))), 707);
}

#[test]
fn input_is_new_input() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    let u = one(update(
        &mut core,
        T,
        Some([13519, 11264, 5]),
        [13519, 11264, 5],
    ));
    assert_eq!(u.input_tokens, Some(2255));
    assert_eq!(u.cache_read_tokens, Some(11_264));
    assert_eq!(u.cache_write_tokens, Some(0));
    assert_eq!(u.output_tokens, Some(5));
}

#[test]
fn every_update_has_scheme_2() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    let mut all = update(&mut core, T, Some([10, 0, 1]), [10, 0, 1]);
    all.extend(update(&mut core, T, Some([30, 5, 3]), [20, 5, 2]));
    all.extend(update(&mut core, CHILD, Some([7, 0, 1]), [7, 0, 1]));
    all.extend(update(&mut core, T, None, [5, 0, 1]));
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|u| u.scheme == Some(USAGE_SCHEME)));
}

#[test]
fn an_unlisted_model_is_cost_partial() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    // `ready_core` runs the model `m`, which has no price.
    let u = one(update(&mut core, T, Some([100, 0, 10]), [100, 0, 10]));
    assert_eq!(u.cost_usd, None);
    assert!(!u.cost_inferred);
    assert_eq!(u.prices, None);
    assert!(u.cost_partial);
    // A listed model has an inferred cost with the table date.
    core.thread_model = Some("gpt-5-codex".into());
    let u = one(update(&mut core, T, Some([1_000_100, 0, 10]), [100, 0, 10]));
    assert!(u.cost_usd.unwrap() > 1.0);
    assert!(u.cost_inferred);
    assert!(!u.cost_partial);
    assert_eq!(u.prices.as_deref(), Some(PRICE_TABLE_VERSION));
}

#[test]
fn the_usage_state_survives_a_checkpoint() {
    let mut core = ready_core();
    turn_started(&mut core, T, "n1");
    update(&mut core, T, Some([1000, 0, 10]), [1000, 0, 10]);
    let mut core = crash_and_restore(&core, false);
    // The restored core counts the growth of the total, not the total.
    assert_eq!(
        tokens(&one(update(&mut core, T, Some([1600, 0, 14]), [600, 0, 4]))),
        604
    );
    // A replay base stays a base after a restore: no turn started yet.
    let mut fresh = crash_and_restore(&ready_core(), false);
    assert!(update(&mut fresh, T, Some([900, 0, 9]), [900, 0, 9]).is_empty());
}

#[test]
fn a_legacy_checkpoint_restored_during_a_turn_counts_the_next_call() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let mut core = crash_and_restore(&core, true);
    assert_eq!(core.usage, Some(ThreadUsage::legacy()));
    assert!(core.current_turn().is_some());
    // The model call that this update reports: its `last` block.
    let first = one(update(&mut core, T, Some([29990, 0, 10]), [14995, 0, 5]));
    assert_eq!(tokens(&first), 15_000);
    // Then the growth of the total.
    let second = one(update(&mut core, T, Some([44995, 0, 15]), [15005, 0, 5]));
    assert_eq!(tokens(&second), 15_010);
}

#[test]
fn a_legacy_checkpoint_restored_between_turns_does_not_count_the_thread_total() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    complete(&mut core, "n1", "completed");
    let mut core = crash_and_restore(&core, true);
    assert!(core.current_turn().is_none());
    turn_started(&mut core, T, "n2");
    let u = one(update(&mut core, T, Some([59950, 0, 50]), [15095, 0, 5]));
    assert_eq!(tokens(&u), 15_100);
}

#[test]
fn a_legacy_sub_agent_update_counts_its_last_block() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let mut core = crash_and_restore(&core, true);
    // The old core dropped the sub-agent's earlier calls: its first update
    // counts the `last` block and seeds the thread.
    let first = one(update(&mut core, CHILD, Some([4990, 0, 10]), [1995, 0, 5]));
    assert_eq!(tokens(&first), 2_000);
    let second = one(update(&mut core, CHILD, Some([6995, 0, 15]), [2005, 0, 5]));
    assert_eq!(tokens(&second), 2_010);
}

#[test]
fn token_totals_read_both_shapes_and_subtract() {
    let app = TokenTotals::from_app_server(&block([100, 40, 10])).unwrap();
    assert_eq!(
        app,
        TokenTotals {
            input: 100,
            cached: 40,
            cache_write: 0,
            output: 10
        }
    );
    let rollout = TokenTotals::from_rollout(&json!({
        "input_tokens": 100, "cached_input_tokens": 40,
        "cache_write_input_tokens": 3, "output_tokens": 10
    }))
    .unwrap();
    assert_eq!(rollout.cache_write, 3);
    assert!(TokenTotals::from_app_server(&json!({"inputTokens": 1})).is_none());
    assert!(TokenTotals::from_rollout(&json!({"output_tokens": 1})).is_none());
    assert_eq!(
        rollout.since(&TokenTotals {
            input: 60,
            cached: 40,
            cache_write: 0,
            output: 4
        }),
        TokenTotals {
            input: 40,
            cached: 0,
            cache_write: 3,
            output: 6
        }
    );
    // One lower field: the whole value.
    assert_eq!(
        app.since(&TokenTotals {
            input: 10,
            cached: 41,
            cache_write: 0,
            output: 1
        }),
        app
    );
}
