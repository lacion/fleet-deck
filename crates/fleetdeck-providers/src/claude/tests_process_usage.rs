//! The per-model running totals of the Claude core (B14, B21; DL-442, DL-443).
//! The numbers are the `result` frames of the recordings of 2026-10-23
//! (`claude` 2.1.308; `~/fleetdeck-briefs/backlog/usage-cost/claude/`).

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{USAGE_SCHEME, Usage};
use serde_json::{Value, json};

use super::frames::{CliFrame, ResultFrame, parse_value};
use super::process_usage::{ModelTotals, ProcessTotals, ProcessUsage, UsageBase};
use super::tests::{config, emits, feed, init, input, lifecycle};
use super::*;

const MODEL: &str = "claude-haiku-5-5";

/// One recorded `result`: `total_cost_usd`, `result.usage` as
/// `[input, output, cache read, cache write]` and the `modelUsage` of the
/// model (the same four counts; its `costUSD` is the total).
type Row = (f64, [u64; 4], [u64; 4]);

const RUN1: [Row; 4] = [
    (0.002_157_075, [2, 5, 0, 17_235], [2, 5, 0, 17_235]),
    (
        0.005_011_310_000_000_001,
        [6, 160, 14_051, 21_065],
        [8, 165, 14_051, 38_300],
    ),
    (
        0.006_404_475_000_000_001,
        [4, 5, 17_819, 54],
        [14, 174, 31_870, 47_979],
    ),
    (
        0.006_649_605,
        [4, 4, 17_873, 512],
        [18, 178, 49_743, 48_491],
    ),
];
const RUN2_RESUME: [Row; 2] = [
    (0.006_837_855, [4, 4, 18_385, 16], [22, 182, 68_128, 48_507]),
    (
        0.007_154_475,
        [4, 6, 17_297, 1122],
        [26, 188, 85_425, 49_629],
    ),
];
const RUN5_AFTER_KILL: Row = (0.007_534_96, [4, 4, 18_455, 16], [34, 197, 122_281, 49_682]);

fn model_usage(counts: [u64; 4], cost: f64) -> Value {
    json!({(MODEL): {"inputTokens": counts[0], "outputTokens": counts[1],
                   "cacheReadInputTokens": counts[2], "cacheCreationInputTokens": counts[3],
                   "costUSD": cost, "costBasis": "list"}})
}

fn usage_value(counts: [u64; 4]) -> Value {
    json!({"input_tokens": counts[0], "output_tokens": counts[1],
           "cache_read_input_tokens": counts[2], "cache_creation_input_tokens": counts[3]})
}

fn frame_json(row: &Row) -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
           "total_cost_usd": row.0, "usage": usage_value(row.1),
           "modelUsage": model_usage(row.2, row.0)})
}

fn result_of(v: Value) -> ResultFrame {
    match parse_value(v).unwrap() {
        CliFrame::Result(r) => r,
        other => panic!("not a result: {other:?}"),
    }
}

/// The recorded `modelUsage` of a row, as the totals of a `cost-state` record.
fn totals_of(row: &Row) -> ProcessTotals {
    ProcessTotals::from_cost_state(&json!({"type":"cost-state","modelUsage":
        model_usage(row.2, row.0)}))
    .unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The deltas of `rows`, counted from `base`.
fn deltas(base: &UsageBase, rows: &[Row]) -> Vec<Usage> {
    let mut state = ProcessUsage::new(base);
    rows.iter()
        .map(|r| state.delta(&result_of(frame_json(r))))
        .collect()
}

#[derive(Debug, Default, PartialEq)]
struct Sum {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
}

fn sum(parts: &[Usage]) -> Sum {
    let mut s = Sum::default();
    for u in parts {
        s.input += u.input_tokens.unwrap_or(0);
        s.output += u.output_tokens.unwrap_or(0);
        s.cache_read += u.cache_read_tokens.unwrap_or(0);
        s.cache_write += u.cache_write_tokens.unwrap_or(0);
        s.cost += u.cost_usd.unwrap_or(0.0);
    }
    s
}

#[test]
fn one_process_adds_the_growth_per_model() {
    let parts = deltas(&UsageBase::Zero, &RUN1);
    let s = sum(&parts);
    assert_eq!(
        (s.input, s.output, s.cache_read, s.cache_write),
        (18, 178, 49_743, 48_491)
    );
    assert!(close(s.cost, 0.006_649_605), "{s:?}");
    assert!(parts.iter().all(|u| !u.tokens_partial && !u.cost_partial));
}

#[test]
fn a_subagent_appears_in_the_next_result() {
    let parts = deltas(&UsageBase::Zero, &RUN1);
    // Result 3: the main thread reports 4 input and 5 output; the process
    // total grew by 6 and 9, so the subagent adds 2 and 4.
    assert_eq!(parts[2].input_tokens, Some(RUN1[2].1[0] + 2));
    assert_eq!(parts[2].output_tokens, Some(RUN1[2].1[1] + 4));
}

#[test]
fn a_resume_counts_from_the_restored_record() {
    let base = UsageBase::Restored(totals_of(&RUN1[3]));
    let parts = deltas(&base, &RUN2_RESUME);
    assert!(close(parts[0].cost_usd.unwrap(), 0.000_188_25), "{parts:?}");
    assert!(close(parts[1].cost_usd.unwrap(), 0.000_316_62), "{parts:?}");
    // With run 1 the session total is the last total of the resume.
    let all = sum(&deltas(&UsageBase::Zero, &RUN1)).cost + sum(&parts).cost;
    assert!(close(all, 0.007_154_475), "{all}");
    assert_eq!(parts[0].input_tokens, Some(4));
    assert_eq!(parts[0].cache_read_tokens, Some(68_128 - 49_743));
}

#[test]
fn a_resume_after_a_kill_counts_from_the_older_record() {
    // The killed run wrote no record: the next resume restored the older one
    // (the third `cost-state` record of the recording).
    let record = (0.007_346_01, [0; 4], [30, 193, 103_826, 49_666]);
    let base = UsageBase::Restored(totals_of(&record));
    let parts = deltas(&base, &[RUN5_AFTER_KILL]);
    assert!(close(parts[0].cost_usd.unwrap(), 0.000_188_95), "{parts:?}");
}

#[test]
fn an_unknown_base_counts_the_turn_tokens_and_no_cost() {
    let parts = deltas(&UsageBase::Unknown, &RUN2_RESUME);
    let first = &parts[0];
    assert_eq!(
        (first.input_tokens, first.output_tokens),
        (Some(4), Some(4))
    );
    assert_eq!(first.cache_read_tokens, Some(18_385));
    assert_eq!(first.cache_write_tokens, Some(16));
    assert!(first.tokens_partial);
    assert_eq!(first.cost_usd, None);
    assert!(first.cost_partial);
    // The second adds the growth, with no flag.
    let second = &parts[1];
    assert_eq!(second.input_tokens, Some(4));
    assert_eq!(second.output_tokens, Some(6));
    assert!(!second.tokens_partial && !second.cost_partial);
    assert!(close(second.cost_usd.unwrap(), 0.000_316_62), "{second:?}");
}

/// The usage parts of the `usage.updated` events in `out`.
fn usages(out: &[Output]) -> Vec<Usage> {
    emits(out)
        .iter()
        .filter_map(|e| match e {
            Event::UsageUpdated { usage, .. } if !usage.is_empty() => Some(usage.clone()),
            _ => None,
        })
        .collect()
}

/// A started core.
fn started() -> ClaudeCore {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    core
}

/// A finished turn: the input starts, then `frame` arrives as its `result`.
fn turn(core: &mut ClaudeCore, n: u8, frame: Value) -> Vec<Usage> {
    core.prompt(&input(n, "go")).unwrap();
    lifecycle(core, n, "started");
    usages(&feed(core, frame))
}

#[test]
fn a_legacy_checkpoint_counts_the_cost_growth() {
    let core = started();
    let mut stored = core.checkpoint().unwrap();
    // The state of a build before the per-model totals.
    let state = stored.state.as_object_mut().unwrap();
    state.remove("usage");
    state.insert("last_total_cost".to_owned(), json!(0.5));
    let mut core = ClaudeCore::restore(config(false), &stored).unwrap();
    let parts = turn(
        &mut core,
        1,
        json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
               "total_cost_usd":0.8,"usage":{"input_tokens":3,"output_tokens":4}}),
    );
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert!(close(parts[0].cost_usd.unwrap(), 0.3), "{parts:?}");
    assert_eq!(parts[0].input_tokens, Some(3));
    assert_eq!(parts[0].output_tokens, Some(4));
    assert!(parts[0].tokens_partial);
}

fn plain_result(total: f64, counts: [u64; 4]) -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed",
           "total_cost_usd": total, "usage": usage_value(counts)})
}

#[test]
fn fallback_results_advance_the_cost_cursor() {
    let mut core = started();
    let mut parts = turn(&mut core, 1, plain_result(0.10, [1, 1, 0, 0]));
    parts.extend(turn(&mut core, 2, plain_result(0.20, [1, 1, 0, 0])));
    let costs: Vec<f64> = parts.iter().map(|u| u.cost_usd.unwrap()).collect();
    assert!(close(costs[0], 0.10) && close(costs[1], 0.10), "{costs:?}");
    assert!(parts.iter().all(|u| u.tokens_partial));
}

#[test]
fn alternating_shapes_do_not_count_twice() {
    let mut state = ProcessUsage::new(&UsageBase::Zero);
    let with_m1 = json!({"type":"result","subtype":"success","is_error":false,
        "total_cost_usd":0.2,"usage":usage_value([1, 1, 1, 1]),
        "modelUsage":model_usage([10, 5, 100, 50], 0.2)});
    let without = plain_result(0.3, [3, 2, 10, 5]);
    let with_m2 = json!({"type":"result","subtype":"success","is_error":false,
        "total_cost_usd":0.45,"usage":usage_value([4, 3, 20, 6]),
        "modelUsage":model_usage([20, 9, 200, 80], 0.45)});
    let parts: Vec<Usage> = [with_m1, without, with_m2]
        .into_iter()
        .map(|v| state.delta(&result_of(v)))
        .collect();
    let s = sum(&parts);
    assert!(close(s.cost, 0.45), "{s:?}");
    // M1, plus the turn tokens of the two results that follow.
    assert_eq!(s.input, 10 + 3 + 4);
    assert_eq!(s.output, 5 + 2 + 3);
    assert_eq!(s.cache_read, 100 + 10 + 20);
    assert_eq!(s.cache_write, 50 + 5 + 6);
    assert!(!parts[0].tokens_partial);
    assert!(parts[1].tokens_partial && parts[2].tokens_partial);
}

#[test]
fn every_delta_has_scheme_2() {
    let mut all = deltas(&UsageBase::Zero, &RUN1);
    all.extend(deltas(&UsageBase::Unknown, &RUN2_RESUME));
    all.extend(deltas(
        &UsageBase::Restored(ProcessTotals::default()),
        &[RUN5_AFTER_KILL],
    ));
    let mut state = ProcessUsage::new(&UsageBase::Zero);
    all.push(state.delta(&result_of(plain_result(0.1, [1, 1, 0, 0]))));
    all.push(state.delta(&result_of(
        json!({"type":"result","subtype":"error_max_turns"}),
    )));
    for u in &all {
        assert_eq!(u.scheme, Some(USAGE_SCHEME), "{u:?}");
        assert!(!u.cost_inferred && u.prices.is_none(), "{u:?}");
    }
}

#[test]
fn a_new_model_counts_from_zero() {
    let mut state = ProcessUsage::new(&UsageBase::Zero);
    let one = json!({"type":"result","subtype":"success","total_cost_usd":0.1,
        "usage":usage_value([1, 1, 0, 0]),
        "modelUsage":{"a":{"inputTokens":10,"outputTokens":5,"cacheReadInputTokens":0,
                           "cacheCreationInputTokens":0,"costUSD":0.1}}});
    let two = json!({"type":"result","subtype":"success","total_cost_usd":0.35,
        "usage":usage_value([1, 1, 0, 0]),
        "modelUsage":{"a":{"inputTokens":12,"outputTokens":6,"cacheReadInputTokens":0,
                           "cacheCreationInputTokens":0,"costUSD":0.15},
                      "b":{"inputTokens":7,"outputTokens":3,"cacheReadInputTokens":0,
                           "cacheCreationInputTokens":0,"costUSD":0.20}}});
    state.delta(&result_of(one));
    let u = state.delta(&result_of(two));
    assert_eq!(
        (u.input_tokens, u.output_tokens),
        (Some(2 + 7), Some(1 + 3))
    );
    assert!(close(u.cost_usd.unwrap(), 0.25), "{u:?}");
}

#[test]
fn a_lower_model_total_is_a_new_count() {
    let mut state = ProcessUsage::new(&UsageBase::Zero);
    let row = |counts: [u64; 4], a: f64, b: f64| {
        json!({"type":"result","subtype":"success","total_cost_usd":a + b,
            "usage":usage_value([1, 1, 0, 0]),
            "modelUsage":{
                "a":{"inputTokens":counts[0],"outputTokens":counts[1],"cacheReadInputTokens":0,
                     "cacheCreationInputTokens":0,"costUSD":a},
                "b":{"inputTokens":100,"outputTokens":100,"cacheReadInputTokens":0,
                     "cacheCreationInputTokens":0,"costUSD":b}}})
    };
    state.delta(&result_of(row([50, 40, 0, 0], 0.5, 0.25)));
    // Model `a` started again (lower counts); model `b` only grew by cost.
    let u = state.delta(&result_of(row([5, 4, 0, 0], 0.05, 0.25)));
    assert_eq!((u.input_tokens, u.output_tokens), (Some(5), Some(4)));
    // The cost of `a` is all new; `b` is not counted again.
    assert!(close(u.cost_usd.unwrap(), 0.05), "{u:?}");
}

#[test]
fn the_usage_state_survives_a_checkpoint() {
    let mut core = started();
    let first = turn(&mut core, 1, frame_json(&RUN1[0]));
    assert!(close(first[0].cost_usd.unwrap(), RUN1[0].0));
    let stored = core.checkpoint().unwrap();
    // The legacy field is not written any more.
    assert!(stored.state.get("last_total_cost").is_none());
    let mut back = ClaudeCore::restore(config(false), &stored).unwrap();
    assert_eq!(back.checkpoint().unwrap(), stored);
    let next = turn(&mut back, 2, frame_json(&RUN1[1]));
    assert_eq!(next[0].cache_write_tokens, Some(38_300 - 17_235));
    assert!(close(next[0].cost_usd.unwrap(), RUN1[1].0 - RUN1[0].0));
    assert!(!next[0].tokens_partial);
}

#[test]
fn the_base_comes_from_the_core_config() {
    let mut cfg = config(false);
    cfg.usage_base = UsageBase::Restored(totals_of(&RUN1[3]));
    cfg.resumed = Some(true);
    let mut core = ClaudeCore::new(cfg);
    init(&mut core);
    let parts = turn(&mut core, 1, frame_json(&RUN2_RESUME[0]));
    assert!(close(parts[0].cost_usd.unwrap(), 0.000_188_25), "{parts:?}");
}

#[test]
fn model_totals_since_is_the_difference() {
    let totals = ProcessTotals::from_cost_state(&json!({"modelUsage":
        model_usage([1, 2, 3, 4], 0.5)}))
    .unwrap();
    assert_eq!(
        totals.models[MODEL],
        ModelTotals {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cost_usd: 0.5
        }
    );
    assert!(close(totals.cost(), 0.5));
    let usage = totals.usage();
    assert_eq!(usage.cache_write_tokens, Some(4));
    assert_eq!(usage.scheme, Some(USAGE_SCHEME));
}
