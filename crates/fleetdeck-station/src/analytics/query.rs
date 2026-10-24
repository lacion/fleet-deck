//! The analytics query (B29, design section 3.7): the usage and activity rows
//! of a window, in pages of whole hours that fit one control frame, with the
//! owner or the member form of the sources and the coverage.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::analytics::{
    ActivityHour, AnalyticsAnswer, AnalyticsCoverage, AnalyticsQuery, AnalyticsSource, Audience,
    MAX_PAGE_BYTES, SourceState, UsageHour,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::code;
use fleetdeck_providers::prices::{PRICE_TABLE_VERSION, estimate_cost};
use ring::digest;

use super::{HomeStatus, ScanStatus};
use crate::history::{HistoryError, internal};
use crate::store::Store;
use crate::store::analytics::{HomeFacts, UsageRaw};

/// How long a query waits for the one query slot of the station.
const WAIT: Duration = Duration::from_secs(2);

/// The limits of one answer: the page size and the wait for the slot. The
/// tests use smaller ones.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub budget: usize,
    pub wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            budget: MAX_PAGE_BYTES,
            wait: WAIT,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn kind_text(kind: ProviderKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `UsageHour.conversation`: 16 hex characters of the SHA-256 of
/// `"fd-analytics-conv"`, a NUL, the provider name, a NUL and the id.
#[must_use]
pub fn conversation_hash(provider: ProviderKind, conversation: &str) -> String {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(b"fd-analytics-conv");
    ctx.update(&[0]);
    ctx.update(kind_text(provider).as_bytes());
    ctx.update(&[0]);
    ctx.update(conversation.as_bytes());
    hex(&ctx.finish().as_ref()[..8])
}

/// The answer of `station.analytics`: one page.
///
/// # Errors
/// `invalid_params` for a bad query, `busy` when another query holds the
/// slot for more than 2 s, `internal` for a store error or a page that does
/// not fit.
pub async fn answer(
    manager: &crate::manager::SessionManager,
    status: &ScanStatus,
    query: AnalyticsQuery,
) -> Result<AnalyticsAnswer, HistoryError> {
    answer_with(manager.store(), status, query, Limits::default()).await
}

/// One page of the rows of one hour: the usage rows and the activity rows.
struct HourRows {
    hour: i64,
    usage: Vec<UsageHour>,
    activity: Vec<ActivityHour>,
}

/// The serialized size of a row in an array: its bytes and the comma.
fn row_size<T: serde::Serialize>(row: &T) -> usize {
    serde_json::to_vec(row).map_or(0, |v| v.len()) + 1
}

impl HourRows {
    fn size(&self) -> usize {
        self.usage.iter().map(row_size).sum::<usize>()
            + self.activity.iter().map(row_size).sum::<usize>()
    }
}

fn tokens(row: &UsageHour) -> u64 {
    row.input
        .saturating_add(row.cache_read)
        .saturating_add(row.cache_write)
        .saturating_add(row.output)
}

/// The sources of the answer in the owner or the member form, and the index
/// of each home key. A home that the rows hold and the last pass did not see
/// (before the first pass ends) gets a source with the provider of its rows,
/// `verified: false` and `complete: false`.
fn build_sources(
    homes: Vec<HomeStatus>,
    facts: &[HomeFacts],
    usage: &[UsageRaw],
    audience: Audience,
    events_since: Option<i64>,
) -> (Vec<AnalyticsSource>, HashMap<String, u32>) {
    let mut list: Vec<(HomeStatus, Option<u64>)> = homes.into_iter().map(|h| (h, None)).collect();
    for row in usage {
        if !list.iter().any(|(h, _)| h.key == row.home) {
            list.push((
                HomeStatus {
                    provider: row.provider,
                    key: row.home.clone(),
                    state: SourceState::Ok,
                    verified: false,
                    complete: false,
                    pending_bytes: 0,
                    scanned_at: None,
                    keeps_days: None,
                },
                None,
            ));
        }
    }
    let mut index = HashMap::new();
    let mut sources = Vec::new();
    for (home, _) in list {
        let fact = facts.iter().find(|f| f.home == home.key);
        let first_at = fact.and_then(|f| f.first_at);
        let source = match audience {
            Audience::Owner => AnalyticsSource {
                provider: home.provider,
                key: home.key.clone(),
                state: home.state,
                verified: home.verified,
                complete: home.complete,
                first_at,
                scanned_at: home.scanned_at,
                files: Some(fact.map_or(0, |f| f.files)),
                pending_bytes: Some(home.pending_bytes),
                malformed: Some(fact.map_or(0, |f| f.malformed)),
                keeps_days: home.keeps_days,
            },
            Audience::Member => AnalyticsSource {
                provider: home.provider,
                key: home.key.clone(),
                state: match home.state {
                    SourceState::Unavailable { .. } => SourceState::Unavailable {
                        reason: "unavailable".to_owned(),
                    },
                    other => other,
                },
                verified: home.verified,
                complete: home.complete,
                first_at: first_at.zip(events_since).map(|(a, b)| a.max(b)),
                scanned_at: None,
                files: None,
                pending_bytes: None,
                malformed: None,
                keeps_days: None,
            },
        };
        index.insert(home.key, u32::try_from(sources.len()).unwrap_or(u32::MAX));
        sources.push(source);
    }
    (sources, index)
}

/// The rows grouped by hour, newest hour first.
fn group_hours(usage: Vec<UsageHour>, activity: Vec<ActivityHour>) -> Vec<HourRows> {
    let mut by_hour: BTreeMap<i64, HourRows> = BTreeMap::new();
    for row in usage {
        by_hour
            .entry(row.hour)
            .or_insert_with(|| HourRows {
                hour: row.hour,
                usage: Vec::new(),
                activity: Vec::new(),
            })
            .usage
            .push(row);
    }
    for row in activity {
        by_hour
            .entry(row.hour)
            .or_insert_with(|| HourRows {
                hour: row.hour,
                usage: Vec::new(),
                activity: Vec::new(),
            })
            .activity
            .push(row);
    }
    let mut hours: Vec<HourRows> = by_hour.into_values().collect();
    hours.reverse();
    hours
}

/// The activity rows of the store, one per hour and session.
fn activity_hours(rows: Vec<crate::store::analytics::ActivityRaw>) -> Vec<ActivityHour> {
    let mut by_key: BTreeMap<(std::cmp::Reverse<i64>, String), BTreeMap<String, u64>> =
        BTreeMap::new();
    for row in rows {
        let counts = by_key
            .entry((std::cmp::Reverse(row.hour), row.session_id))
            .or_default();
        *counts.entry(row.metric).or_insert(0) += row.value;
    }
    by_key
        .into_iter()
        .map(|((hour, session), counts)| ActivityHour {
            hour: hour.0,
            session_id: SessionId::new(session),
            counts,
        })
        .collect()
}

/// [`answer`] on a store, with the limits of the page.
pub(crate) async fn answer_with(
    store: &Store,
    status: &ScanStatus,
    query: AnalyticsQuery,
    limits: Limits,
) -> Result<AnalyticsAnswer, HistoryError> {
    if let Some(text) = query.check() {
        return Err(HistoryError::new(code::INVALID_PARAMS, text));
    }
    let _permit = match tokio::time::timeout(limits.wait, status.query_gate().acquire()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(e)) => return Err(internal(e)),
        Err(_) => {
            return Err(HistoryError::new(
                code::BUSY,
                "another analytics request is running",
            ));
        }
    };
    let top = query.before.unwrap_or(query.until);
    let sessions = query.sessions.clone();
    let usage_raw = store
        .analytics_usage_rows(query.since, top, query.audience, sessions.clone())
        .await
        .map_err(internal)?;
    let mut activity_raw = store
        .analytics_activity_rows(query.since, top, sessions.clone())
        .await
        .map_err(internal)?;
    if query.audience == Audience::Member {
        // A member never gets a counter outside a session.
        activity_raw.retain(|r| !r.session_id.is_empty());
    }
    let facts = store.analytics_home_facts().await.map_err(internal)?;
    let coverage_sessions = match query.audience {
        Audience::Owner => None,
        Audience::Member => sessions,
    };
    let cov = store
        .analytics_coverage(coverage_sessions)
        .await
        .map_err(internal)?;
    let (sources, index) = build_sources(
        status.homes(),
        &facts,
        &usage_raw,
        query.audience,
        cov.events_since,
    );
    let usage: Vec<UsageHour> = usage_raw
        .into_iter()
        .map(|r| UsageHour {
            hour: r.hour,
            provider: r.provider,
            conversation: conversation_hash(r.provider, &r.conversation),
            session_id: (!r.session_id.is_empty()).then(|| SessionId::new(r.session_id)),
            source: index.get(&r.home).copied().unwrap_or(0),
            requests: r.requests,
            input: r.input,
            cache_read: r.cache_read,
            cache_write: r.cache_write,
            output: r.output,
            reasoning: r.reasoning,
            cost_usd: estimate_cost(
                r.provider,
                &r.model,
                r.input,
                r.cache_read,
                r.cache_write,
                r.output,
            ),
            model: r.model,
        })
        .collect();
    let hours = group_hours(usage, activity_hours(activity_raw));
    let mut page = AnalyticsAnswer {
        read_at: now_ms(),
        price_table: PRICE_TABLE_VERSION.to_owned(),
        sources,
        coverage: AnalyticsCoverage {
            events_since: cov.events_since,
            kept_since: cov.kept_since,
        },
        usage: Vec::new(),
        activity: Vec::new(),
        next_before: Some(i64::MAX),
        partial_hour: Some(i64::MAX),
    };
    let base = serde_json::to_vec(&page).map_err(internal)?.len();
    fill_page(&mut page, hours, base, limits.budget);
    let len = serde_json::to_vec(&page).map_err(internal)?.len();
    if len > limits.budget {
        return Err(internal("the analytics page is larger than the page limit"));
    }
    Ok(page)
}

/// Puts whole hours into the page while they fit, and sets `next_before` and
/// `partial_hour`.
fn fill_page(page: &mut AnalyticsAnswer, hours: Vec<HourRows>, base: usize, budget: usize) {
    page.next_before = None;
    page.partial_hour = None;
    let mut size = base;
    let mut taken = 0;
    for hour in &hours {
        let hour_size = hour.size();
        if size + hour_size > budget {
            break;
        }
        size += hour_size;
        taken += 1;
    }
    if taken == 0 {
        let older = hours.len() > 1;
        let Some(first) = hours.into_iter().next() else {
            return;
        };
        let mut usage = first.usage;
        usage.sort_by_key(|r| std::cmp::Reverse(tokens(r)));
        for row in usage {
            let row_len = row_size(&row);
            if size + row_len > budget {
                break;
            }
            size += row_len;
            page.usage.push(row);
        }
        for row in first.activity {
            let row_len = row_size(&row);
            if size + row_len > budget {
                break;
            }
            size += row_len;
            page.activity.push(row);
        }
        page.partial_hour = Some(first.hour);
        page.next_before = older.then_some(first.hour);
        return;
    }
    let remain = hours.len() > taken;
    let mut oldest = 0;
    for hour in hours.into_iter().take(taken) {
        oldest = hour.hour;
        page.usage.extend(hour.usage);
        page.activity.extend(hour.activity);
    }
    page.next_before = remain.then_some(oldest);
}
