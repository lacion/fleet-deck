//! Tests of the Codex rollout parser.

use super::codex::{CodexState, parse_line, parse_meta};
use super::{Parsed, UsageRecord};

const FORK: &str = include_str!("fixtures/codex_fork.jsonl");

/// `2026-10-20T10:00:SS.mmmZ`.
fn ts(seconds: u32, millis: u32) -> String {
    format!("2026-10-20T10:00:{seconds:02}.{millis:03}Z")
}

fn totals(input: u64, output: u64) -> String {
    format!(r#"{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":{output}}}"#)
}

fn token_count(at: &str, c: (u64, u64), l: (u64, u64)) -> String {
    format!(
        r#"{{"timestamp":"{at}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{},"last_token_usage":{}}}}}}}"#,
        totals(c.0, c.1),
        totals(l.0, l.1)
    )
}

fn usage_record(at: &str, response: &str, c: (u64, u64), l: (u64, u64)) -> String {
    format!(
        r#"{{"timestamp":"{at}","type":"token_usage_record","payload":{{"response_id":"{response}","thread_token_usage":{},"usage":{}}}}}"#,
        totals(c.0, c.1),
        totals(l.0, l.1)
    )
}

fn meta(at: &str, payload: &str) -> String {
    format!(r#"{{"timestamp":"{at}","type":"session_meta","payload":{payload}}}"#)
}

fn feed(state: &mut CodexState, lines: &[String]) -> Vec<Parsed> {
    lines
        .iter()
        .map(|line| parse_line(line.as_bytes(), state))
        .collect()
}

fn records(parsed: Vec<Parsed>) -> Vec<UsageRecord> {
    parsed
        .into_iter()
        .filter_map(|p| match p {
            Parsed::Record(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn fresh() -> CodexState {
    let mut state = CodexState::default();
    let _ = parse_line(meta(&ts(0, 0), r#"{"id":"th1"}"#).as_bytes(), &mut state);
    state
}

fn pair(r: &UsageRecord) -> (u64, u64) {
    (r.input, r.output)
}

#[test]
fn token_count_deltas_follow_the_cumulative() {
    let mut state = fresh();
    let lines = [
        token_count(&ts(1, 0), (100, 10), (100, 10)),
        token_count(&ts(2, 0), (250, 20), (150, 10)),
        token_count(&ts(3, 0), (400, 30), (150, 10)),
    ];
    let got = records(feed(&mut state, &lines));
    let inputs: Vec<u64> = got.iter().map(|r| r.input).collect();
    assert_eq!(inputs, vec![100, 150, 150]);
    let outputs: Vec<u64> = got.iter().map(|r| r.output).collect();
    assert_eq!(outputs, vec![10, 10, 10]);
}

#[test]
fn a_token_usage_record_of_the_same_request_is_not_counted_again() {
    let mut state = fresh();
    let lines = [
        usage_record(&ts(1, 0), "resp_a", (100, 10), (100, 10)),
        token_count(&ts(1, 100), (100, 10), (100, 10)),
    ];
    let got = feed(&mut state, &lines);
    assert!(matches!(got[0], Parsed::Record(_)));
    assert_eq!(got[1], Parsed::Nothing);
}

#[test]
fn fork_copies_set_the_baseline_only() {
    let mut state = CodexState::default();
    let lines: Vec<String> = FORK.lines().map(str::to_owned).collect();
    let got = records(feed(&mut state, &lines));
    assert_eq!(got.len(), 1);
    // The delta of the real record over the last copy (250, 30) -> (400, 60).
    assert_eq!(pair(&got[0]), (150, 30));
    assert_eq!(got[0].model, "gpt-6.1-sol");
    assert_eq!(got[0].conversation, "th-fork");
}

#[test]
fn a_restart_larger_than_the_baseline_counts_the_whole_request() {
    let mut state = fresh();
    let lines = [
        token_count(&ts(1, 0), (100, 10), (100, 10)),
        token_count(&ts(2, 0), (500, 50), (500, 50)),
    ];
    let got = records(feed(&mut state, &lines));
    assert_eq!(got.len(), 2);
    assert_eq!(pair(&got[1]), (500, 50));
    assert_ne!(got[0].key, got[1].key);
    assert!(state.epoch_ms > 0);
}

#[test]
fn a_restart_below_the_baseline_counts_the_request() {
    let mut state = fresh();
    let lines = [
        token_count(&ts(1, 0), (300, 30), (300, 30)),
        token_count(&ts(2, 0), (120, 12), (20, 2)),
    ];
    let got = records(feed(&mut state, &lines));
    assert_eq!(got.len(), 2);
    assert_eq!(pair(&got[1]), (20, 2));
}

#[test]
fn a_missing_report_counts_the_growth() {
    let mut state = fresh();
    let lines = [
        token_count(&ts(1, 0), (100, 10), (100, 10)),
        token_count(&ts(2, 0), (400, 40), (100, 10)),
    ];
    let got = records(feed(&mut state, &lines));
    assert_eq!(pair(&got[1]), (300, 30));
}

#[test]
fn keys_differ_across_counter_epochs() {
    let lines = [
        token_count(&ts(1, 0), (100, 10), (100, 10)),
        token_count(&ts(2, 0), (300, 30), (200, 20)),
        token_count(&ts(3, 0), (100, 10), (100, 10)),
    ];
    let mut first = fresh();
    let keys: Vec<_> = records(feed(&mut first, &lines))
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert_eq!(keys.len(), 3);
    assert_eq!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    let mut second = fresh();
    let again: Vec<_> = records(feed(&mut second, &lines))
        .into_iter()
        .map(|r| r.key)
        .collect();
    assert_eq!(keys, again);
}

#[test]
fn a_response_id_is_the_key() {
    let mut state = fresh();
    let line = usage_record(&ts(1, 0), "resp_abc", (100, 10), (100, 10));
    let got = records(feed(&mut state, &[line]));
    assert_eq!(got[0].key.as_deref(), Some("codex:resp:resp_abc"));
    let mut state = fresh();
    let line = token_count(&ts(1, 0), (100, 10), (100, 10));
    let got = records(feed(&mut state, &[line]));
    let epoch = state.epoch_ms;
    assert_eq!(got[0].key, Some(format!("codex:th1:{epoch}:100:10")));
}

#[test]
fn both_parent_encodings_are_read() {
    let spawn = meta(
        &ts(0, 0),
        r#"{"id":"child","source":{"subagent":{"thread_spawn":{"parent_thread_id":"root"}}}}"#,
    );
    let flat = meta(&ts(0, 0), r#"{"id":"child","parent_thread_id":"root"}"#);
    let none = meta(&ts(0, 0), r#"{"id":"child","parent_thread_id":7}"#);
    for line in [&spawn, &flat] {
        let found = parse_meta(line.as_bytes()).unwrap();
        assert_eq!(found.thread, "child");
        assert_eq!(found.parent.as_deref(), Some("root"));
        let mut state = CodexState::default();
        let _ = parse_line(line.as_bytes(), &mut state);
        assert_eq!(state.thread, "child");
        assert!(state.copying);
    }
    assert_eq!(parse_meta(none.as_bytes()).unwrap().parent, None);
    assert_eq!(
        parse_meta(token_count(&ts(1, 0), (1, 1), (1, 1)).as_bytes()),
        None
    );
}

#[test]
fn cached_input_is_taken_out_of_new_input() {
    let mut state = fresh();
    let line = format!(
        r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{t},"last_token_usage":{t}}}}}}}"#,
        ts(1, 0),
        t = r#"{"input_tokens":1000,"cached_input_tokens":800,"cache_write_input_tokens":100,"output_tokens":5,"reasoning_output_tokens":2}"#
    );
    let got = records(feed(&mut state, &[line]));
    assert_eq!(
        (
            got[0].input,
            got[0].cache_read,
            got[0].cache_write,
            got[0].output,
            got[0].reasoning
        ),
        (100, 800, 100, 5, 2)
    );
}

#[test]
fn the_model_comes_from_the_last_turn_context() {
    let mut state = fresh();
    let context = |model: &str| {
        format!(
            r#"{{"timestamp":"{}","type":"turn_context","payload":{{"model":"{model}"}}}}"#,
            ts(1, 0)
        )
    };
    let lines = [
        context("gpt-a"),
        token_count(&ts(2, 0), (100, 10), (100, 10)),
        context("gpt-b"),
        token_count(&ts(3, 0), (200, 20), (100, 10)),
    ];
    let got = records(feed(&mut state, &lines));
    assert_eq!(got[0].model, "gpt-a");
    assert_eq!(got[1].model, "gpt-b");
    assert_eq!(
        records(feed(
            &mut fresh(),
            &[token_count(&ts(1, 0), (1, 1), (1, 1))]
        ))[0]
            .model,
        "unknown"
    );
}

#[test]
fn state_round_trips_through_json() {
    let mut state = fresh();
    feed(&mut state, &[token_count(&ts(1, 0), (100, 10), (100, 10))]);
    let text = serde_json::to_string(&state).unwrap();
    let back: CodexState = serde_json::from_str(&text).unwrap();
    assert_eq!(back, state);
    assert!(back.baseline.is_some());
}

#[test]
fn bad_lines_are_nothing_or_malformed() {
    let mut state = fresh();
    let null_info = format!(
        r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"token_count","info":null}}}}"#,
        ts(1, 0)
    );
    assert_eq!(
        parse_line(null_info.as_bytes(), &mut state),
        Parsed::Nothing
    );
    let bad_time = token_count("later", (1, 1), (1, 1));
    assert_eq!(
        parse_line(bad_time.as_bytes(), &mut state),
        Parsed::Malformed
    );
    let no_output = format!(
        r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":1}},"last_token_usage":{{"input_tokens":1}}}}}}}}"#,
        ts(1, 0)
    );
    assert_eq!(
        parse_line(no_output.as_bytes(), &mut state),
        Parsed::Malformed
    );
    assert_eq!(
        parse_line(b"not json token_count", &mut state),
        Parsed::Malformed
    );
    assert_eq!(
        parse_line(br#"{"type":"event_msg"}"#, &mut state),
        Parsed::Nothing
    );
}

#[test]
fn a_context_size_report_counts_nothing() {
    let mut state = fresh();
    let lines = [
        token_count(&ts(1, 0), (100, 10), (100, 10)),
        token_count(&ts(2, 0), (100, 10), (0, 0)),
        token_count(&ts(3, 0), (150, 15), (50, 5)),
    ];
    let got = records(feed(&mut state, &lines));
    assert_eq!(got.len(), 2);
    assert_eq!(pair(&got[1]), (50, 5));
}
