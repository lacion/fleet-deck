//! Tests of the analytics query (B29, design `analytics.md` section 3.7).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::analytics::{
    AnalyticsAnswer, AnalyticsQuery, Audience, HOUR_MS, MAX_PAGE_BYTES, SourceState,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{ControlFrame, MAX_CONTROL_FRAME_BYTES, Outcome};
use fleetdeck_providers::prices::estimate_cost;
use fleetdeck_testkit::Det;

use super::query::{Limits, answer_with, conversation_hash};
use super::{HomeStatus, ScanStatus};
use crate::store::{Store, StoreConfig};

async fn store() -> Store {
    let det = Det::new();
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

async fn exec(s: &Store, text: &str) {
    let text = text.to_owned();
    s.db()
        .call(move |conn| {
            conn.execute_batch(&text)?;
            Ok(())
        })
        .await
        .unwrap();
}

/// One `analytics_usage` row of the home `h1`, provider `claude`.
struct Row<'a> {
    hour: i64,
    model: &'a str,
    conversation: &'a str,
    session: &'a str,
    input: u64,
}

async fn put(s: &Store, rows: Vec<(String, i64, String, String, String, u64)>) {
    s.db()
        .call(move |conn| {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO analytics_usage (hour, home, provider, model, conversation,
                         session_id, requests, input, cache_read, cache_write, output, reasoning)
                     VALUES (?1, ?2, 'claude', ?3, ?4, ?5, 1, ?6, 0, 0, 0, 0)",
                )?;
                for (_, hour, model, conversation, session, input) in rows {
                    stmt.execute(rusqlite::params![
                        hour,
                        "h1",
                        model,
                        conversation,
                        session,
                        i64::try_from(input).unwrap()
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn usage(s: &Store, rows: &[Row<'_>]) {
    put(
        s,
        rows.iter()
            .map(|r| {
                (
                    String::new(),
                    r.hour,
                    r.model.to_owned(),
                    r.conversation.to_owned(),
                    r.session.to_owned(),
                    r.input,
                )
            })
            .collect(),
    )
    .await;
}

fn query(until_hours: i64, audience: Audience) -> AnalyticsQuery {
    AnalyticsQuery {
        since: 0,
        until: until_hours * HOUR_MS,
        before: None,
        sessions: None,
        audience,
    }
}

async fn read(s: &Store, status: &ScanStatus, q: AnalyticsQuery) -> AnalyticsAnswer {
    answer_with(s, status, q, Limits::default()).await.unwrap()
}

#[tokio::test]
async fn the_answer_prices_the_buckets() {
    let s = store().await;
    let status = ScanStatus::new();
    exec(
        &s,
        "INSERT INTO analytics_usage VALUES (7200000, 'h1', 'claude', 'claude-opus-5-5', 'conv-1', '',
             1, 1000000, 0, 0, 1000000, 0)",
    )
    .await;
    let a = read(&s, &status, query(10, Audience::Owner)).await;
    assert_eq!(a.usage.len(), 1);
    let row = &a.usage[0];
    let expected = estimate_cost(
        ProviderKind::Claude,
        "claude-opus-5-5",
        1_000_000,
        0,
        0,
        1_000_000,
    );
    assert_eq!(row.cost_usd, expected);
    assert!((row.cost_usd.unwrap() - 24.0).abs() < 1e-9);
    assert_eq!(row.hour, 2 * HOUR_MS);
    assert_eq!(row.session_id, None);
    assert_eq!(row.source, 0);
    assert_eq!(
        row.conversation,
        conversation_hash(ProviderKind::Claude, "conv-1")
    );
    assert_eq!(row.conversation.len(), 16);
    assert_eq!(
        a.price_table,
        fleetdeck_providers::prices::PRICE_TABLE_VERSION
    );
    assert_eq!(a.next_before, None);
}

#[tokio::test]
async fn the_answer_filters_by_sessions() {
    let s = store().await;
    let status = ScanStatus::new();
    usage(
        &s,
        &[
            Row {
                hour: HOUR_MS,
                model: "m1",
                conversation: "c1",
                session: "s1",
                input: 1,
            },
            Row {
                hour: HOUR_MS,
                model: "m1",
                conversation: "c2",
                session: "s2",
                input: 2,
            },
            Row {
                hour: HOUR_MS,
                model: "m1",
                conversation: "c3",
                session: "",
                input: 3,
            },
        ],
    )
    .await;
    exec(
        &s,
        "INSERT INTO analytics_activity VALUES (3600000, 's1', 'runs', 4), (3600000, 's2', 'runs', 5)",
    )
    .await;
    let mut q = query(10, Audience::Owner);
    q.sessions = Some(vec![SessionId::new("s1")]);
    let a = read(&s, &status, q).await;
    assert_eq!(a.usage.len(), 1);
    assert_eq!(a.usage[0].session_id, Some(SessionId::new("s1")));
    assert_eq!(a.activity.len(), 1);
    assert_eq!(a.activity[0].session_id, SessionId::new("s1"));
    assert_eq!(a.activity[0].counts.get("runs"), Some(&4));
    // The owner without a list sees all three usage rows.
    let all = read(&s, &status, query(10, Audience::Owner)).await;
    assert_eq!(all.usage.len(), 3);
}

#[tokio::test]
async fn a_member_answer_has_only_attributed_rows_and_the_member_form() {
    let s = store().await;
    let status = ScanStatus::new();
    usage(
        &s,
        &[
            Row {
                hour: 10 * HOUR_MS,
                model: "m1",
                conversation: "c1",
                session: "s1",
                input: 1,
            },
            Row {
                hour: 10 * HOUR_MS,
                model: "m1",
                conversation: "c2",
                session: "",
                input: 2,
            },
        ],
    )
    .await;
    exec(&s, "UPDATE analytics_meta SET events_since = 72000000").await;
    status.set(vec![HomeStatus {
        provider: ProviderKind::Claude,
        key: "h1".to_owned(),
        state: SourceState::Unavailable {
            reason: "home_not_trusted".to_owned(),
        },
        verified: true,
        complete: true,
        pending_bytes: 77,
        scanned_at: Some(123),
        keeps_days: Some(30),
    }]);
    let member = read(&s, &status, query(50, Audience::Member)).await;
    assert_eq!(member.usage.len(), 1);
    assert_eq!(member.usage[0].session_id, Some(SessionId::new("s1")));
    let src = &member.sources[0];
    assert_eq!(
        src.state,
        SourceState::Unavailable {
            reason: "unavailable".to_owned()
        }
    );
    assert_eq!(src.files, None);
    assert_eq!(src.pending_bytes, None);
    assert_eq!(src.scanned_at, None);
    assert_eq!(src.malformed, None);
    assert_eq!(src.keeps_days, None);
    // Owner first hour is 10 h; events_since is 20 h: the member form clips.
    assert_eq!(src.first_at, Some(72_000_000));
    assert_eq!(member.coverage.events_since, Some(72_000_000));
    let owner = read(&s, &status, query(50, Audience::Owner)).await;
    assert_eq!(owner.usage.len(), 2);
    let src = &owner.sources[0];
    assert_eq!(
        src.state,
        SourceState::Unavailable {
            reason: "home_not_trusted".to_owned()
        }
    );
    assert_eq!(src.pending_bytes, Some(77));
    assert_eq!(src.scanned_at, Some(123));
    assert_eq!(src.keeps_days, Some(30));
    assert_eq!(src.first_at, Some(10 * HOUR_MS));
}

fn five_hours() -> Vec<(String, i64, String, String, String, u64)> {
    (1..=5)
        .map(|h| {
            (
                String::new(),
                (100 + h) * HOUR_MS,
                "m1".to_owned(),
                "c1".to_owned(),
                String::new(),
                1000 + u64::try_from(h).unwrap(),
            )
        })
        .collect()
}

/// The size that the query counts for an answer without rows.
fn base_size(a: &AnalyticsAnswer) -> usize {
    let mut empty = a.clone();
    empty.usage.clear();
    empty.activity.clear();
    empty.next_before = Some(i64::MAX);
    empty.partial_hour = Some(i64::MAX);
    serde_json::to_vec(&empty).unwrap().len()
}

fn row_len(a: &AnalyticsAnswer) -> usize {
    serde_json::to_vec(&a.usage[0]).unwrap().len() + 1
}

#[tokio::test]
async fn pages_hold_whole_hours_newest_first() {
    let s = store().await;
    let status = ScanStatus::new();
    put(&s, five_hours()).await;
    let q = query(200, Audience::Owner);
    let full = read(&s, &status, q.clone()).await;
    assert_eq!(full.usage.len(), 5);
    assert_eq!(full.next_before, None);
    let budget = base_size(&full) + 2 * row_len(&full);
    let limits = Limits {
        budget,
        wait: Duration::from_secs(2),
    };
    let mut pages = Vec::new();
    let mut next = None;
    loop {
        let mut q = q.clone();
        q.before = next;
        let page = answer_with(&s, &status, q, limits).await.unwrap();
        next = page.next_before;
        pages.push(page);
        if next.is_none() {
            break;
        }
    }
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].usage.len(), 2);
    assert_eq!(pages[0].next_before, Some(104 * HOUR_MS));
    assert_eq!(pages[1].next_before, Some(102 * HOUR_MS));
    assert_eq!(pages[2].usage.len(), 1);
    assert!(pages.iter().all(|p| p.partial_hour.is_none()));
    let union: Vec<_> = pages.iter().flat_map(|p| p.usage.clone()).collect();
    assert_eq!(union, full.usage);
    let hours: Vec<i64> = union.iter().map(|r| r.hour).collect();
    assert!(hours.windows(2).all(|w| w[0] > w[1]));
}

#[tokio::test]
async fn a_page_fits_the_real_frame() {
    let s = store().await;
    let status = ScanStatus::new();
    let model = "m".repeat(64);
    let rows: Vec<_> = (0..60_000_i64)
        .map(|n| {
            (
                String::new(),
                (n % 2_000) * HOUR_MS,
                model.clone(),
                format!("conversation-{n}"),
                String::new(),
                1000 + u64::try_from(n).unwrap(),
            )
        })
        .collect();
    put(&s, rows).await;
    let mut q = query(2_001, Audience::Owner);
    let mut total = 0;
    let mut pages = 0;
    loop {
        let page = read(&s, &status, q.clone()).await;
        let frame = ControlFrame::Response {
            id: u64::MAX,
            outcome: Outcome::Ok(serde_json::to_value(&page).unwrap()),
        };
        let text = serde_json::to_string(&frame).unwrap();
        assert!(
            text.len() <= MAX_CONTROL_FRAME_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PAGE_BYTES);
        total += page.usage.len();
        pages += 1;
        match page.next_before {
            Some(b) => q.before = Some(b),
            None => break,
        }
    }
    assert!(pages > 1);
    assert_eq!(total, 60_000);
}

#[tokio::test]
async fn an_hour_larger_than_a_page_is_partial() {
    let s = store().await;
    let status = ScanStatus::new();
    let mut rows: Vec<_> = (0..10_u64)
        .map(|i| {
            (
                String::new(),
                105 * HOUR_MS,
                "m1".to_owned(),
                format!("c{i}"),
                String::new(),
                1000 + i,
            )
        })
        .collect();
    rows.push((
        String::new(),
        104 * HOUR_MS,
        "m1".to_owned(),
        "c0".to_owned(),
        String::new(),
        1000,
    ));
    put(&s, rows).await;
    let q = query(200, Audience::Owner);
    let full = read(&s, &status, q.clone()).await;
    let budget = base_size(&full) + 3 * row_len(&full);
    let limits = Limits {
        budget,
        wait: Duration::from_secs(2),
    };
    let first = answer_with(&s, &status, q.clone(), limits).await.unwrap();
    assert_eq!(first.partial_hour, Some(105 * HOUR_MS));
    assert_eq!(first.next_before, Some(105 * HOUR_MS));
    let inputs: Vec<u64> = first.usage.iter().map(|r| r.input).collect();
    assert_eq!(inputs, vec![1009, 1008, 1007]);
    let mut q2 = q;
    q2.before = first.next_before;
    let second = answer_with(&s, &status, q2, limits).await.unwrap();
    assert_eq!(second.partial_hour, None);
    assert_eq!(second.next_before, None);
    assert_eq!(second.usage.len(), 1);
    assert_eq!(second.usage[0].hour, 104 * HOUR_MS);
}

#[tokio::test]
async fn a_second_query_is_busy() {
    let s = store().await;
    let status = ScanStatus::new();
    let permit = status.query_gate().acquire().await.unwrap();
    let limits = Limits {
        budget: MAX_PAGE_BYTES,
        wait: Duration::from_millis(50),
    };
    let err = answer_with(&s, &status, query(10, Audience::Owner), limits)
        .await
        .unwrap_err();
    assert_eq!(err.code, "busy");
    drop(permit);
    assert!(
        answer_with(&s, &status, query(10, Audience::Owner), limits)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_bad_query_is_invalid_params() {
    let s = store().await;
    let status = ScanStatus::new();
    let mut q = query(10, Audience::Owner);
    q.since = 5;
    let err = answer_with(&s, &status, q, Limits::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
}
