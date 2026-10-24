use super::*;

const DAY: i64 = 24 * HOUR_MS;

fn query() -> AnalyticsQuery {
    AnalyticsQuery {
        since: 10 * HOUR_MS,
        until: 10 * HOUR_MS + DAY,
        before: None,
        sessions: None,
        audience: Audience::Owner,
    }
}

fn usage(session: Option<&str>, cost: Option<f64>) -> UsageHour {
    UsageHour {
        hour: HOUR_MS,
        provider: ProviderKind::Claude,
        model: "claude-opus-5-5".to_owned(),
        conversation: "0123456789abcdef".to_owned(),
        session_id: session.map(SessionId::new),
        source: 0,
        requests: 1,
        input: 2,
        cache_read: 3,
        cache_write: 4,
        output: 5,
        reasoning: 0,
        cost_usd: cost,
    }
}

fn answer(next_before: Option<i64>, usage: Vec<UsageHour>) -> AnalyticsAnswer {
    AnalyticsAnswer {
        read_at: 1,
        price_table: "2026-10-24".to_owned(),
        sources: Vec::new(),
        coverage: AnalyticsCoverage {
            events_since: None,
            kept_since: None,
        },
        usage,
        activity: Vec::new(),
        next_before,
        partial_hour: None,
    }
}

#[test]
fn analytics_query_check_refuses_a_bad_window() {
    assert_eq!(query().check(), None);
    let bad_window = Some("the window is not valid");
    let mut q = query();
    q.since = -HOUR_MS;
    assert_eq!(q.check(), bad_window);
    let mut q = query();
    q.since = HOUR_MS + 1;
    assert_eq!(q.check(), bad_window);
    let mut q = query();
    q.since = q.until;
    assert_eq!(q.check(), bad_window);
    let mut q = query();
    q.since = q.until + HOUR_MS;
    assert_eq!(q.check(), bad_window);

    let bad_cursor = Some("the page cursor is not valid");
    let mut q = query();
    q.before = Some(q.since + 1);
    assert_eq!(q.check(), bad_cursor);
    let mut q = query();
    q.before = Some(q.since);
    assert_eq!(q.check(), bad_cursor);
    let mut q = query();
    q.before = Some(q.since - HOUR_MS);
    assert_eq!(q.check(), bad_cursor);
    let mut q = query();
    q.before = Some(q.until + HOUR_MS);
    assert_eq!(q.check(), bad_cursor);
    let mut q = query();
    q.before = Some(q.since + 5 * HOUR_MS);
    assert_eq!(q.check(), None);
    // `until` inside an hour: the cursor may be the hour that holds it, rounded up.
    let mut q = query();
    q.until += 1;
    q.before = Some(q.since + DAY + HOUR_MS);
    assert_eq!(q.check(), None);

    let mut q = query();
    q.sessions = Some(vec![SessionId::new("s"); MAX_QUERY_SESSIONS]);
    assert_eq!(q.check(), None);
    q.sessions = Some(vec![SessionId::new("s"); MAX_QUERY_SESSIONS + 1]);
    assert_eq!(q.check(), Some("too many sessions"));
    let mut q = query();
    q.sessions = Some(vec![SessionId::new("../x")]);
    assert_eq!(q.check(), Some("a session id is not valid"));

    // A window of 10 years is valid; so is "all the history".
    let mut q = query();
    q.until = q.since + 3650 * DAY;
    assert_eq!(q.check(), None);
    let mut q = query();
    q.since = 0;
    assert_eq!(q.check(), None);
}

#[test]
fn analytics_answer_round_trips_and_omits_empty_options() {
    let row = usage(None, None);
    let json = serde_json::to_value(&row).unwrap();
    assert!(json.get("session_id").is_none());
    assert!(json.get("cost_usd").is_none());
    let full = usage(Some("01J"), Some(1.5));
    let json = serde_json::to_value(&full).unwrap();
    assert_eq!(json["session_id"], "01J");
    assert_eq!(json["cost_usd"], 1.5);

    let last = answer(None, vec![row, full]);
    let json = serde_json::to_value(&last).unwrap();
    assert!(json.get("next_before").is_none());
    assert!(json.get("partial_hour").is_none());
    let back: AnalyticsAnswer = serde_json::from_value(json).unwrap();
    assert_eq!(back, last);

    let paged = answer(Some(HOUR_MS), Vec::new());
    let json = serde_json::to_value(&paged).unwrap();
    assert_eq!(json["next_before"], HOUR_MS);
    let back: AnalyticsAnswer = serde_json::from_value(json).unwrap();
    assert_eq!(back, paged);
}

#[test]
fn source_state_is_flattened() {
    let source = AnalyticsSource {
        provider: ProviderKind::Claude,
        key: "0123456789abcdef".to_owned(),
        state: SourceState::Unavailable {
            reason: "owner".to_owned(),
        },
        verified: true,
        complete: false,
        first_at: None,
        scanned_at: None,
        files: None,
        pending_bytes: None,
        malformed: None,
        keeps_days: None,
    };
    let json = serde_json::to_value(&source).unwrap();
    assert_eq!(json["state"], "unavailable");
    assert_eq!(json["reason"], "owner");
    let back: AnalyticsSource = serde_json::from_value(json).unwrap();
    assert_eq!(back, source);
    let ok = serde_json::to_value(SourceState::Ok).unwrap();
    assert_eq!(ok, serde_json::json!({"state": "ok"}));
}

#[test]
fn max_page_bytes_leaves_room_for_the_envelope() {
    assert_eq!(
        MAX_PAGE_BYTES + 64 * 1024,
        crate::link::MAX_CONTROL_FRAME_BYTES
    );
}
