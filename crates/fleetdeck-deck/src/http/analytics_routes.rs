//! The analytics of the fleet (B29, `docs/v2/design/analytics.md` 3.9).
//!
//! `GET /api/analytics?since=<ms>&until=<ms>[&station=<name>]` asks the
//! stations at once for `station.analytics`, reads every page of each one
//! (at most [`MAX_PAGES`]) and answers a [`FleetAnalytics`]. The owner gets
//! every row. A member gets the rows of proven runs of the covered sessions
//! only; the deck drops every other row and clears the owner fields of the
//! sources again (defense in depth). Nothing is stored or cached.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::analytics::{
    AnalyticsAnswer, AnalyticsQuery, Audience, FleetAnalytics, MAX_PAGES, SourceState,
    StationAnalytics, StationAnalyticsState,
};
use fleetdeck_proto::history::code;
use fleetdeck_proto::ids::SessionId;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use tokio::sync::Semaphore;

use super::Board;
use super::guard::ApiError;
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal, ShareScope, StationRecord};

/// The link method.
const METHOD: &str = "station.analytics";
/// The audit action.
const ACTION: &str = "analytics.read";
/// The longest wait for one page.
const PAGE_WAIT: Duration = Duration::from_secs(15);
/// The longest wait for all the pages of one station.
const STATION_WAIT: Duration = Duration::from_secs(60);
/// The longest wait for a free slot of the deck.
const SLOT_WAIT: Duration = Duration::from_secs(2);
/// The requests that the deck serves at once.
const AT_ONCE: usize = 4;
/// The longest station name, in characters.
const MAX_STATION_CHARS: usize = 128;
/// The word of a member answer for every unavailable reason.
const UNAVAILABLE: &str = "unavailable";

static SLOTS: Semaphore = Semaphore::const_new(AT_ONCE);

/// The query string.
#[derive(Debug, Deserialize)]
pub struct Params {
    since: i64,
    until: i64,
    #[serde(default)]
    station: Option<String>,
}

fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message)
}

fn unknown_station() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_station", "no such station")
}

/// True when `name` is short and holds no control character.
fn plain_name(name: &str) -> bool {
    name.chars().count() <= MAX_STATION_CHARS && !name.chars().any(char::is_control)
}

/// Why a station gave no answer.
enum Fail {
    Offline,
    Busy,
    Timeout,
    Error,
}

impl Fail {
    fn state(&self) -> StationAnalyticsState {
        match self {
            Self::Offline => StationAnalyticsState::Offline,
            Self::Busy => StationAnalyticsState::Busy,
            Self::Timeout => StationAnalyticsState::Timeout,
            Self::Error => StationAnalyticsState::Error,
        }
    }
}

/// One page of one station.
async fn page(
    board: &Board,
    rec: &StationRecord,
    query: &AnalyticsQuery,
) -> Result<AnalyticsAnswer, Fail> {
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or(Fail::Offline)?;
    let params = serde_json::to_value(query).map_err(|_| Fail::Error)?;
    let outcome = tokio::time::timeout(PAGE_WAIT, control.request(METHOD, None, params))
        .await
        .map_err(|_| Fail::Timeout)?
        .map_err(|_| Fail::Offline)?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            // The decode error can quote the answer: it is not logged.
            tracing::warn!(
                method = METHOD,
                "the station sent an answer that does not parse"
            );
            Fail::Error
        }),
        Outcome::Err(e) if e.code == code::BUSY => Err(Fail::Busy),
        Outcome::Err(e) => {
            if e.code != code::INVALID_PARAMS {
                tracing::warn!(code = %e.code, method = METHOD, "the station failed a read");
            }
            Err(Fail::Error)
        }
    }
}

/// The pages of one station, merged: the rows appended, `sources` and
/// `coverage` of the first page, the `partial_hour` of each page in
/// `partial_hours`, and `truncated_before` when `MAX_PAGES` pages were not
/// enough.
async fn pages(
    board: &Board,
    rec: &StationRecord,
    mut query: AnalyticsQuery,
) -> Result<(AnalyticsAnswer, Option<i64>, Vec<i64>), Fail> {
    let mut merged: Option<AnalyticsAnswer> = None;
    let mut partial = Vec::new();
    let mut truncated = None;
    for n in 1..=MAX_PAGES {
        let mut got = page(board, rec, &query).await?;
        let next = got.next_before.take();
        if let Some(hour) = got.partial_hour.take() {
            partial.push(hour);
        }
        match merged.as_mut() {
            None => merged = Some(got),
            Some(m) => {
                m.usage.append(&mut got.usage);
                m.activity.append(&mut got.activity);
            }
        }
        match next {
            None => break,
            Some(before) if n == MAX_PAGES => truncated = Some(before),
            Some(before) => query.before = Some(before),
        }
    }
    let merged = merged.ok_or(Fail::Error)?;
    Ok((merged, truncated, partial))
}

/// Defense in depth for a member: drops every row without a covered session
/// and clears the owner fields of the sources.
fn confine(answer: &mut AnalyticsAnswer, station_id: &str, scope: &ShareScope) {
    let covered = |id: &Option<SessionId>| {
        id.as_ref()
            .is_some_and(|s| scope.covers(station_id, s.as_str()))
    };
    answer.usage.retain(|u| covered(&u.session_id));
    answer
        .activity
        .retain(|a| scope.covers(station_id, a.session_id.as_str()));
    for source in &mut answer.sources {
        source.scanned_at = None;
        source.files = None;
        source.pending_bytes = None;
        source.malformed = None;
        source.keeps_days = None;
        if let SourceState::Unavailable { reason } = &mut source.state {
            *reason = UNAVAILABLE.to_owned();
        }
    }
}

/// One station: the feature check, the pages, the member filter.
async fn read_station(
    board: &Board,
    rec: &StationRecord,
    query: AnalyticsQuery,
    member: Option<&ShareScope>,
) -> StationAnalytics {
    let mut out = StationAnalytics {
        station_id: rec.station_id.clone(),
        name: rec.name.clone(),
        state: StationAnalyticsState::Ok,
        answer: None,
        truncated_before: None,
        partial_hours: Vec::new(),
    };
    match board
        .store
        .link_feature(&rec.station_id, feature::ANALYTICS)
    {
        LinkFeature::Down => {
            out.state = StationAnalyticsState::Offline;
            return out;
        }
        LinkFeature::Without => {
            out.state = StationAnalyticsState::Unsupported;
            return out;
        }
        LinkFeature::Negotiated => {}
    }
    match tokio::time::timeout(STATION_WAIT, pages(board, rec, query)).await {
        Err(_) => out.state = StationAnalyticsState::Timeout,
        Ok(Err(fail)) => out.state = fail.state(),
        Ok(Ok((mut answer, truncated, partial))) => {
            if let Some(scope) = member {
                confine(&mut answer, &rec.station_id.0, scope);
            }
            out.answer = Some(answer);
            out.truncated_before = truncated;
            out.partial_hours = partial;
        }
    }
    out
}

/// A station to ask and its sessions (`None`: every session).
type Chosen = (StationRecord, Option<Vec<SessionId>>);

/// The stations to ask.
fn choose(
    all: Vec<StationRecord>,
    wanted: Option<&str>,
    principal: &Principal,
) -> Result<Vec<Chosen>, ApiError> {
    let mut out = Vec::new();
    for rec in all {
        if wanted.is_some_and(|name| name != rec.name) {
            continue;
        }
        match principal {
            Principal::Member(_, grant) => match &grant.scope {
                ShareScope::Board => out.push((rec, None)),
                ShareScope::Sessions(set) => {
                    let ids: Vec<SessionId> = set
                        .iter()
                        .filter(|(st, _)| *st == rec.station_id.0)
                        .map(|(_, s)| SessionId::from(s.clone()))
                        .collect();
                    // A station with no covered session is not in the answer.
                    if !ids.is_empty() {
                        out.push((rec, Some(ids)));
                    }
                }
            },
            _ => out.push((rec, None)),
        }
    }
    if wanted.is_some() && out.is_empty() {
        return Err(unknown_station());
    }
    Ok(out)
}

async fn answer(
    board: &Board,
    principal: &Principal,
    query: Result<Query<Params>, QueryRejection>,
) -> Result<FleetAnalytics, ApiError> {
    let (member_scope, audience) = match principal {
        Principal::Owner(_) => (None, Audience::Owner),
        Principal::Member(_, grant) => (Some(&grant.scope), Audience::Member),
        Principal::Token(..) | Principal::Display(_) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "owner_only",
                "Only a board login reads analytics.",
            ));
        }
    };
    let Query(params) = query.map_err(|_| bad("the query is not valid"))?;
    let base = AnalyticsQuery {
        since: params.since,
        until: params.until,
        before: None,
        sessions: None,
        audience,
    };
    if let Some(text) = base.check() {
        return Err(bad(text));
    }
    if let Some(name) = &params.station
        && !plain_name(name)
    {
        return Err(bad("the station name is not valid"));
    }
    let all = board.store.stations().await.map_err(ApiError::internal)?;
    let chosen = choose(all, params.station.as_deref(), principal)?;
    let _slot = match tokio::time::timeout(SLOT_WAIT, SLOTS.acquire()).await {
        Ok(Ok(permit)) => permit,
        _ => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "The deck is busy. Try again in a moment.",
            ));
        }
    };
    let reads = chosen.into_iter().map(|(rec, sessions)| {
        let query = AnalyticsQuery {
            sessions,
            ..base.clone()
        };
        async move {
            if let Some(text) = query.check() {
                tracing::warn!(station = %rec.name, text, "an analytics query is not valid");
                return StationAnalytics {
                    station_id: rec.station_id.clone(),
                    name: rec.name.clone(),
                    state: StationAnalyticsState::Error,
                    answer: None,
                    truncated_before: None,
                    partial_hours: Vec::new(),
                };
            }
            read_station(board, &rec, query, member_scope).await
        }
    });
    let stations = futures_util::future::join_all(reads).await;
    Ok(FleetAnalytics {
        read_at: board.store.now_ms(),
        owner: member_scope.is_none(),
        stations,
    })
}

/// `GET /api/analytics`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only` (a token or a display); 404
/// `unknown_station`; 503 `busy`.
pub async fn read(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<Params>, QueryRejection>,
) -> Result<Json<FleetAnalytics>, ApiError> {
    let result = answer(&board, &principal, query).await;
    if let Err(e) = &result {
        let kind = if matches!(
            e.status,
            StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ) {
            AuditResult::Refused
        } else {
            AuditResult::Failed
        };
        board
            .store
            .audit_or_log(NewAudit::new(principal.actor(), ACTION, kind).detail(e.code))
            .await;
    }
    result.map(Json)
}
