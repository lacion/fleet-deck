//! The usage fold (DL-449, design `usage-cost.md` §3.1, §4.1).

use super::*;
use crate::event::{EventBody, USAGE_SCHEME, UsageSource};
use crate::ids::{CommandId, SessionId};

/// A summary and a sequence counter; each event gets an explicit time.
struct Feed {
    summary: SessionSummary,
    seq: u64,
}

impl Feed {
    fn new() -> Self {
        Self {
            summary: SessionSummary::new(SessionId::from("s1")),
            seq: 0,
        }
    }

    fn push_at(&mut self, at: i64, event: Event) -> &SessionSummary {
        self.seq += 1;
        let envelope = EventEnvelope {
            station_seq: self.seq + 100,
            session_id: SessionId::from("s1"),
            session_seq: self.seq,
            at,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(event),
        };
        self.summary.apply(&envelope);
        &self.summary
    }

    fn usage(&mut self, usage: Usage) -> &SessionSummary {
        self.push_at(1, update(usage))
    }
}

fn update(usage: Usage) -> Event {
    Event::UsageUpdated {
        usage,
        rate_limits: vec![],
    }
}

/// A part of the new scheme.
fn new_part(input: Option<u64>, cost: Option<f64>) -> Usage {
    Usage {
        input_tokens: input,
        cost_usd: cost,
        scheme: Some(USAGE_SCHEME),
        ..Usage::default()
    }
}

/// A part of the old rules.
fn old_part(input: Option<u64>, cost: Option<f64>) -> Usage {
    Usage {
        input_tokens: input,
        cost_usd: cost,
        ..Usage::default()
    }
}

#[test]
fn cache_parts_add_and_unknown_stays_unknown() {
    let mut f = Feed::new();
    let cache = |read, write| Usage {
        cache_read_tokens: read,
        cache_write_tokens: write,
        ..new_part(Some(1), Some(0.1))
    };
    f.usage(cache(Some(3), None));
    let s = f.usage(cache(None, None));
    assert_eq!(s.usage.cache_read_tokens, Some(3));
    assert_eq!(s.usage.cache_write_tokens, None);
    let s = f.usage(cache(Some(4), Some(5)));
    assert_eq!(s.usage.cache_read_tokens, Some(7));
    assert_eq!(s.usage.cache_write_tokens, Some(5));
    let mut f = Feed::new();
    f.usage(cache(None, None));
    let s = f.usage(cache(None, None));
    assert_eq!(s.usage.cache_read_tokens, None);
}

#[test]
fn the_latest_price_table_wins() {
    let mut f = Feed::new();
    let priced = |prices: Option<&str>| Usage {
        prices: prices.map(str::to_owned),
        ..new_part(Some(1), Some(0.1))
    };
    f.usage(priced(Some("a")));
    assert_eq!(f.summary.usage.prices.as_deref(), Some("a"));
    f.usage(priced(None));
    assert_eq!(f.summary.usage.prices.as_deref(), Some("a"));
    let s = f.usage(priced(Some("b")));
    assert_eq!(s.usage.prices.as_deref(), Some("b"));
}

#[test]
fn an_empty_update_changes_nothing() {
    let mut f = Feed::new();
    f.usage(old_part(Some(100), Some(1.0)));
    let before = f.summary.usage.clone();
    let s = f.usage(Usage::default());
    assert_eq!(s.usage, before);
    assert_eq!(s.usage_legacy, None);
    f.usage(new_part(Some(5), Some(0.1)));
    let before = f.summary.clone();
    let s = f.usage(Usage::default());
    assert_eq!(s.usage, before.usage);
    assert_eq!(s.usage_legacy, before.usage_legacy);
}

#[test]
fn the_first_scheme_event_sets_the_old_count_apart() {
    let mut f = Feed::new();
    f.push_at(1, update(old_part(Some(100), Some(1.0))));
    let s = f.push_at(7, update(new_part(Some(5), Some(0.1))));
    assert_eq!(s.usage.scheme, Some(USAGE_SCHEME));
    assert_eq!(s.usage.input_tokens, Some(5));
    assert_eq!(s.usage.cost_usd, Some(0.1));
    let legacy = s.usage_legacy.clone().unwrap();
    assert_eq!(legacy.until, 7);
    assert_eq!(legacy.usage.input_tokens, Some(100));
    assert_eq!(legacy.usage.cost_usd, Some(1.0));
    assert_eq!(legacy.usage.scheme, None);
    // The next new-rule part adds to the new total only.
    let s = f.push_at(9, update(new_part(Some(2), Some(0.2))));
    assert_eq!(s.usage.input_tokens, Some(7));
    assert_eq!(
        s.usage_legacy.as_ref().unwrap().usage.input_tokens,
        Some(100)
    );
    assert_eq!(s.usage_legacy.as_ref().unwrap().until, 7);
}

#[test]
fn a_new_session_counts_with_the_scheme_from_the_start() {
    let mut f = Feed::new();
    let s = f.usage(new_part(Some(5), Some(0.1)));
    assert_eq!(s.usage.scheme, Some(USAGE_SCHEME));
    assert_eq!(s.usage.input_tokens, Some(5));
    assert_eq!(s.usage_legacy, None);
}

#[test]
fn an_old_event_after_the_change_goes_to_the_old_count() {
    let mut f = Feed::new();
    f.push_at(1, update(new_part(Some(5), Some(0.1))));
    let s = f.push_at(8, update(old_part(Some(40), Some(0.4))));
    assert_eq!(s.usage.input_tokens, Some(5));
    assert_eq!(s.usage.cost_usd, Some(0.1));
    let legacy = s.usage_legacy.clone().unwrap();
    assert_eq!(legacy.usage.input_tokens, Some(40));
    assert_eq!(legacy.until, 8);
    let s = f.push_at(9, update(old_part(Some(2), Some(0.1))));
    assert_eq!(
        s.usage_legacy.as_ref().unwrap().usage.input_tokens,
        Some(42)
    );
    assert_eq!(s.usage.input_tokens, Some(5));
}

#[test]
fn partial_flags_stay_once_set() {
    let mut f = Feed::new();
    let flags = |tokens_partial, cost_partial| Usage {
        output_tokens: Some(1),
        cost_usd: Some(0.1),
        tokens_partial,
        cost_partial,
        scheme: Some(USAGE_SCHEME),
        ..Usage::default()
    };
    let s = f.usage(flags(false, false));
    assert!(!s.usage.tokens_partial && !s.usage.cost_partial);
    let s = f.usage(flags(true, false));
    assert!(s.usage.tokens_partial && !s.usage.cost_partial);
    let s = f.usage(flags(false, true));
    assert!(s.usage.tokens_partial && s.usage.cost_partial);
    let s = f.usage(flags(false, false));
    assert!(s.usage.tokens_partial && s.usage.cost_partial);
}

#[test]
fn a_cost_after_unpriced_tokens_is_partial() {
    let mut f = Feed::new();
    f.usage(new_part(Some(10), None));
    let s = f.usage(new_part(Some(5), Some(0.2)));
    assert_eq!(s.usage.cost_usd, Some(0.2));
    assert!(s.usage.cost_partial);
}

#[test]
fn unpriced_tokens_after_a_cost_are_partial() {
    let mut f = Feed::new();
    f.usage(new_part(Some(5), Some(0.2)));
    assert!(!f.summary.usage.cost_partial);
    let s = f.usage(new_part(Some(10), None));
    assert_eq!(s.usage.cost_usd, Some(0.2));
    assert!(s.usage.cost_partial);
}

#[test]
fn usage_set_replaces_the_total_and_marks_the_recount() {
    let mut f = Feed::new();
    let limit = RateLimit {
        scope: "five_hour".into(),
        used_percent: Some(12.5),
        resets_at: Some(1),
    };
    f.push_at(
        1,
        Event::UsageUpdated {
            usage: old_part(Some(100), Some(1.0)),
            rate_limits: vec![limit.clone()],
        },
    );
    f.push_at(2, update(old_part(Some(50), Some(0.5))));
    let previous = f.summary.usage.clone();
    assert_eq!(previous.input_tokens, Some(150));
    let set = Usage {
        input_tokens: Some(30),
        cost_usd: Some(0.3),
        scheme: Some(USAGE_SCHEME),
        tokens_partial: true,
        cost_partial: true,
        ..Usage::default()
    };
    let s = f.push_at(
        9,
        Event::UsageSet {
            usage: set.clone(),
            previous: Box::new(previous.clone()),
            source: UsageSource::ClaudeTranscript,
            command_id: CommandId::from("c1"),
        },
    );
    assert_eq!(s.usage, set);
    assert_eq!(s.usage_legacy, None);
    assert_eq!(
        s.usage_recount,
        Some(UsageRecount {
            at: 9,
            source: UsageSource::ClaudeTranscript,
            command_id: CommandId::from("c1"),
            previous,
        })
    );
    assert_eq!(s.rate_limits, vec![limit]);
    // A new-rule part now adds to the recounted total.
    let s = f.usage(new_part(Some(1), Some(0.1)));
    assert_eq!(s.usage.input_tokens, Some(31));
}

#[test]
fn a_usage_set_event_round_trips_and_has_its_kind() {
    let event = Event::UsageSet {
        usage: new_part(Some(1), None),
        previous: Box::new(old_part(Some(2), Some(0.5))),
        source: UsageSource::CodexRollout,
        command_id: CommandId::from("c1"),
    };
    assert_eq!(event.kind_name(), "usage.set");
    assert!(Event::KINDS.contains(&"usage.set"));
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], "usage.set");
    assert_eq!(value["source"], "codex_rollout");
    let body: EventBody = serde_json::from_value(value).unwrap();
    assert_eq!(body, EventBody::Known(event));
}

#[test]
fn an_old_usage_json_reads_with_no_new_fields() {
    let text = r#"{"input_tokens":1,"output_tokens":2,"cost_usd":0.1,"cost_inferred":false}"#;
    let usage: Usage = serde_json::from_str(text).unwrap();
    assert_eq!(usage.cache_read_tokens, None);
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.prices, None);
    assert_eq!(usage.scheme, None);
    assert!(!usage.tokens_partial && !usage.cost_partial);
    let back = serde_json::to_value(&usage).unwrap();
    assert_eq!(
        back,
        serde_json::json!({
            "input_tokens": 1,
            "output_tokens": 2,
            "cost_usd": 0.1,
            "cost_inferred": false
        })
    );
}
