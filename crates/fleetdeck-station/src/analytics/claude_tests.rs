//! Tests of the Claude transcript parser.

use super::{Parsed, UsageRecord, claude::parse_line};

const FIXTURE: &str = include_str!("fixtures/claude_message.jsonl");

fn assistant(extra: &str, usage: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"2026-10-20T10:00:01.000Z","sessionId":"s1","requestId":"req_1","message":{{"id":"msg_1","model":"claude-opus-5-5","content":[{{"type":"text","text":"x"}}]{extra},"usage":{usage}}}}}"#
    )
}

fn record(parsed: Parsed) -> UsageRecord {
    match parsed {
        Parsed::Record(record) => record,
        other => panic!("expected a record, got {other:?}"),
    }
}

#[test]
fn a_message_counted_once_across_its_content_blocks() {
    let records: Vec<UsageRecord> = FIXTURE
        .lines()
        .filter_map(|line| match parse_line(line.as_bytes()) {
            Parsed::Record(record) => Some(record),
            _ => None,
        })
        .collect();
    assert_eq!(records.len(), 3);
    let keys: std::collections::BTreeSet<_> = records.iter().map(|r| r.key.clone()).collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys.into_iter().next().unwrap().as_deref(),
        Some("claude:msg_1:req_1")
    );
}

#[test]
fn synthetic_and_usage_free_records_are_nothing() {
    let synthetic = assistant("", r#"{"input_tokens":1,"output_tokens":1}"#)
        .replace("claude-opus-5-5", "<synthetic>");
    assert_eq!(parse_line(synthetic.as_bytes()), Parsed::Nothing);
    let no_model =
        assistant("", r#"{"input_tokens":1}"#).replace(r#""model":"claude-opus-5-5","#, "");
    assert_eq!(parse_line(no_model.as_bytes()), Parsed::Nothing);
    let empty_model = assistant("", r#"{"input_tokens":1}"#).replace("claude-opus-5-5", "");
    assert_eq!(parse_line(empty_model.as_bytes()), Parsed::Nothing);
    let no_usage = r#"{"type":"assistant","timestamp":"2026-10-20T10:00:01.000Z","sessionId":"s1","message":{"id":"m","model":"claude-opus-5-5","content":[]}}"#;
    assert_eq!(parse_line(no_usage.as_bytes()), Parsed::Nothing);
    let user = r#"{"type":"user","timestamp":"2026-10-20T10:00:01.000Z","sessionId":"s1","message":{"content":"assistant usage"}}"#;
    assert_eq!(parse_line(user.as_bytes()), Parsed::Nothing);
    assert_eq!(parse_line(b"{}"), Parsed::Nothing);
}

#[test]
fn a_bad_timestamp_is_malformed() {
    let line = assistant("", r#"{"input_tokens":1,"output_tokens":1}"#)
        .replace("2026-10-20T10:00:01.000Z", "yesterday");
    assert_eq!(parse_line(line.as_bytes()), Parsed::Malformed);
    let no_session = assistant("", r#"{"input_tokens":1}"#).replace(r#""sessionId":"s1","#, "");
    assert_eq!(parse_line(no_session.as_bytes()), Parsed::Malformed);
    let broken = r#"{"type":"assistant","usage":"#;
    assert_eq!(parse_line(broken.as_bytes()), Parsed::Malformed);
}

#[test]
fn the_four_parts_map_to_new_input_cache_read_cache_write_output() {
    let line = assistant(
        "",
        r#"{"input_tokens":3,"cache_read_input_tokens":1000,"cache_creation_input_tokens":200,"output_tokens":50}"#,
    );
    let got = record(parse_line(line.as_bytes()));
    assert_eq!(
        got,
        UsageRecord {
            at: 1_792_490_401_000,
            model: "claude-opus-5-5".into(),
            conversation: "s1".into(),
            key: Some("claude:msg_1:req_1".into()),
            input: 3,
            cache_read: 1000,
            cache_write: 200,
            output: 50,
            reasoning: 0,
        }
    );
}

#[test]
fn a_missing_count_is_zero_and_a_missing_key_is_none() {
    let line = r#"{"type":"assistant","timestamp":"2026-10-20T10:00:01.000Z","sessionId":"s1","message":{"model":"m","usage":{"output_tokens":7}}}"#;
    let got = record(parse_line(line.as_bytes()));
    assert_eq!(
        (got.input, got.cache_read, got.cache_write, got.output),
        (0, 0, 0, 7)
    );
    assert_eq!(got.key, None);
}
