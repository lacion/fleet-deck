//! Analytics (B29, design `docs/v2/design/analytics.md`): the params and the
//! answer of the link method `station.analytics` (feature `analytics.v1`,
//! deck → station, scope `read`, not mutating) and the deck answer of
//! `GET /api/analytics`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::command::check_native_session_id;
use crate::event::ProviderKind;
use crate::ids::{SessionId, StationId};

/// One hour in ms. `since` and `before` are multiples of it.
pub const HOUR_MS: i64 = 3_600_000;
/// The most bytes of one serialized `AnalyticsAnswer` (`serde_json::to_vec`):
/// the control frame cap (`link::MAX_CONTROL_FRAME_BYTES`, 1 MiB) less
/// 64 KiB for the response envelope.
pub const MAX_PAGE_BYTES: usize = crate::link::MAX_CONTROL_FRAME_BYTES - 64 * 1024;
/// The most pages that the deck reads from one station for one request.
pub const MAX_PAGES: u32 = 16;
/// The most session ids in one query.
pub const MAX_QUERY_SESSIONS: usize = 10_000;

/// Who the answer is for. The deck sets it from the principal (§3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "analytics/")]
pub enum Audience {
    /// Every row, the rows outside a proven run, and the full source facts.
    Owner,
    /// Only the rows of a proven run of a listed session, and the member
    /// form of the sources and the coverage (§3.7 step 7).
    Member,
}

/// `station.analytics` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsQuery {
    /// Unix ms, inclusive; a multiple of `HOUR_MS` (an hour start). 0: all
    /// the history. There is no longest window (revision 2).
    pub since: i64,
    /// Unix ms, exclusive; `until > since`. The hour that holds `until` is in
    /// the answer (`hour < until`).
    pub until: i64,
    /// Paging: only the hours that start before it; `None`: the first page.
    /// A multiple of `HOUR_MS`, `since < before <= until` rounded up to the hour.
    #[serde(default)]
    pub before: Option<i64>,
    /// Only the rows of these Fleet Deck sessions; `None`: every session of
    /// the station. At most `MAX_QUERY_SESSIONS` ids.
    #[serde(default)]
    pub sessions: Option<Vec<SessionId>>,
    pub audience: Audience,
}

/// The transcript usage of one hour, one conversation, one model and one
/// attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct UsageHour {
    /// Unix ms of the start of the UTC hour.
    pub hour: i64,
    pub provider: ProviderKind,
    pub model: String,
    /// 16 hex characters: SHA-256 of `"fd-analytics-conv"`, a NUL, the
    /// provider name, a NUL and the conversation id (Claude `sessionId`, Codex
    /// root thread). Equal on two stations that read one home; never the id.
    pub conversation: String,
    /// The Fleet Deck session whose proven run made these requests (§3.6a);
    /// `None`: not in a proven run (outside Fleet Deck, an imported earlier
    /// history, an ambiguous holder). A member answer has no `None` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<SessionId>,
    /// Index into `AnalyticsAnswer.sources`.
    pub source: u32,
    /// Counted API responses.
    pub requests: u64,
    /// New input: not read from the cache and not written to it (DL-441).
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// The reasoning part of `output` (Codex); never added again.
    pub reasoning: u64,
    /// The estimate at `price_table` prices; `None`: the model has no price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost_usd: Option<f64>,
}

/// The Fleet Deck counters of one hour and one session (`metric` keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct ActivityHour {
    pub hour: i64,
    pub session_id: SessionId,
    pub counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "analytics/")]
pub enum SourceState {
    /// Read; the backfill may still go on (`complete`).
    Ok,
    /// The home does not exist on this station.
    Missing,
    /// The home or a directory cannot be trusted or read. Owner form: a
    /// reason word (`home_not_trusted`, `home_not_absolute`, `too_many_files`,
    /// `read_failed`); member form: always `unavailable`.
    Unavailable { reason: String },
    /// The owner turned the transcript reading off (`analytics.transcripts`).
    Off,
}

/// One provider home of the station. The fields marked "owner form" are
/// `None` in a member answer (the station clears them; the deck clears them
/// again, §3.9 step 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsSource {
    pub provider: ProviderKind,
    /// 16 hex characters: SHA-256 of `/etc/machine-id` (empty when it cannot
    /// be read), a NUL and the canonical home path. Equal keys on two
    /// stations mean one home. Never a path.
    pub key: String,
    #[serde(flatten)]
    pub state: SourceState,
    /// False when the home is on a filesystem or mount that the station cannot
    /// verify (every home on macOS, §3.5 step 1); the home is read anyway.
    pub verified: bool,
    /// True when the last pass ended with no byte left to read and no walk
    /// budget hit (the history of this home is fully counted).
    pub complete: bool,
    /// Unix ms. Owner form: the oldest counted record of this home. Member
    /// form: `max(that, coverage.events_since)`, the first hour that a member
    /// row can have.
    pub first_at: Option<i64>,
    /// Owner form: Unix ms of the end of the last full pass.
    pub scanned_at: Option<i64>,
    /// Owner form: the transcript files of the home.
    pub files: Option<u64>,
    /// Owner form: bytes on disk that no pass read yet (the backfill progress).
    pub pending_bytes: Option<u64>,
    /// Owner form: records that the parser could not read.
    pub malformed: Option<u64>,
    /// Owner form, Claude only: `cleanupPeriodDays` of the home's
    /// `settings.json`, 30 when it is not set (the CLI default).
    pub keeps_days: Option<u32>,
}

/// Since when the counters of a family hold every event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsCoverage {
    /// The event counters (every key outside `metric::KEPT_KEYS`) and the
    /// proven execution intervals (§3.6a): the time of the oldest event that
    /// the station still had at V50. Station-wide in both forms.
    pub events_since: Option<i64>,
    /// The kept tables (`metric::KEPT_KEYS`). Owner form: the first session
    /// of the station. Member form: the first of the listed sessions (every
    /// session for a board share).
    pub kept_since: Option<i64>,
}

/// `station.analytics` answer: one page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsAnswer {
    pub read_at: i64,
    /// `PRICE_TABLE_VERSION` of the station.
    pub price_table: String,
    pub sources: Vec<AnalyticsSource>,
    pub coverage: AnalyticsCoverage,
    /// The rows of whole hours, newest hour first.
    pub usage: Vec<UsageHour>,
    pub activity: Vec<ActivityHour>,
    /// The next page: ask again with `before = next_before`. `None`: the
    /// last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_before: Option<i64>,
    /// An hour whose rows alone are larger than `MAX_PAGE_BYTES`: only the
    /// rows that fit are in the page (largest tokens first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub partial_hour: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "analytics/")]
pub enum StationAnalyticsState {
    Ok,
    Offline,
    Unsupported,
    Busy,
    Timeout,
    Error,
}

/// One station in the deck answer: the pages merged into one answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct StationAnalytics {
    pub station_id: StationId,
    pub name: String,
    pub state: StationAnalyticsState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<AnalyticsAnswer>,
    /// `MAX_PAGES` pages were read and more were left: the hours before this
    /// one are not in `answer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub truncated_before: Option<i64>,
    /// The `partial_hour` of every page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_hours: Vec<i64>,
}

/// `GET /api/analytics` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct FleetAnalytics {
    pub read_at: i64,
    /// False for a member: no row outside a proven run is in the answer.
    pub owner: bool,
    pub stations: Vec<StationAnalytics>,
}

/// The counter keys of `ActivityHour.counts`.
pub mod metric {
    pub const TURNS_DONE: &str = "turns.done";
    pub const TURNS_INTERRUPTED: &str = "turns.interrupted";
    pub const TURNS_FAILED: &str = "turns.failed";
    /// Sum of the durations of the completed turns whose start was counted.
    pub const TURN_MS: &str = "turns.ms";
    /// Completed turns with a duration in `TURN_MS`.
    pub const TURNS_TIMED: &str = "turns.timed";
    /// `tools.<kind>`, `tools.failed.<kind>`, `tools.denied.<kind>`; kind is
    /// the snake_case `ToolKind` (shell, edit, read, search, web, mcp,
    /// subagent, other).
    pub const TOOLS: &str = "tools";
    pub const APPROVALS_ASKED: &str = "approvals.asked";
    /// `approvals.answered.<by>`; by is the snake_case `ResolvedBy`.
    pub const APPROVALS_ANSWERED: &str = "approvals.answered";
    pub const APPROVALS_EXPIRED: &str = "approvals.expired";
    pub const APPROVAL_WAIT_MS: &str = "approvals.wait_ms";
    pub const APPROVALS_WAITED: &str = "approvals.waited";
    /// Every `error` event, and `errors.<code>` (code `^[a-z][a-z0-9_]{0,31}$`, else `errors.other`).
    pub const ERRORS: &str = "errors";
    pub const FILES_CHANGED: &str = "files.changed";
    pub const LINES_ADDED: &str = "lines.added";
    pub const LINES_REMOVED: &str = "lines.removed";
    pub const STARTS: &str = "starts";
    pub const RESUMES: &str = "resumes";
    pub const RUNS: &str = "runs";
    pub const ARTIFACTS: &str = "artifacts";
    pub const WORKFLOWS: &str = "workflows";
    /// Read from the kept tables at query time.
    pub const SESSIONS_CREATED: &str = "sessions.created";
    pub const TURNS_RECORDED: &str = "turns.recorded";
    pub const SHELL_COMMANDS: &str = "shell.commands";
    pub const SHELL_FAILED: &str = "shell.failed";
    /// The keys of the kept tables; every other key is an event counter.
    pub const KEPT_KEYS: &[&str] = &[
        SESSIONS_CREATED,
        TURNS_RECORDED,
        SHELL_COMMANDS,
        SHELL_FAILED,
    ];
}

impl AnalyticsQuery {
    /// `invalid_params` text, or `None` when the query is valid.
    #[must_use]
    pub fn check(&self) -> Option<&'static str> {
        if self.since < 0 || self.since % HOUR_MS != 0 || self.since >= self.until {
            return Some("the window is not valid");
        }
        if let Some(before) = self.before {
            let until_hour = self.until.saturating_add(HOUR_MS - 1) / HOUR_MS * HOUR_MS;
            if before <= self.since || before % HOUR_MS != 0 || before > until_hour {
                return Some("the page cursor is not valid");
            }
        }
        if let Some(sessions) = &self.sessions {
            if sessions.len() > MAX_QUERY_SESSIONS {
                return Some("too many sessions");
            }
            if sessions
                .iter()
                .any(|id| check_native_session_id(id.as_str()).is_err())
            {
                return Some("a session id is not valid");
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "analytics_tests.rs"]
mod tests;
