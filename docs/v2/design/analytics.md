# Analytics: one page per station and one for the fleet, backfilled from the transcripts (B29)

- **Status:** Draft for the architect, revision 1 (2026-10-24). Risk: **high** (§5): a station migration, a new link method and a member visibility rule. Adversary DESIGN REVIEW before the build, PRE-PR REVIEW before the merge.
- **Owner item (BACKLOG B29):** an analytics page like the usage page of t3code, per station and for the whole fleet, that shows what the sessions already hold (tokens, cost, turns, tool use, durations, failures, approvals, subagents) and backfills the history where the data exists. Owner scope rule (2026-10-24): the metrics go as far as the data goes; no invented history, no estimate of a missing period.
- **Code base:** branch `v2`, HEAD `d1fa4205`. All file:line references are on this commit.
- **Research:** t3code clone `~/research/t3code` at `d53873b6` (read only, no script run). Live measurements on Lighting (read only, aggregates only, no prompt text): `/home/luismorales/fleetdeck-briefs/backlog2/analytics/measure-lighting.json`, `measure-lighting-db.json`, `measure-buckets.json`; the scripts are in `…/analytics/scratch/`.

## 1. Verified facts

### 1.1 What the provider transcripts hold (Lighting, 2026-10-24)

Claude home `~/.claude` (CLI 2.1.30x), `measure-lighting.json`:

1. 1,973 transcript files, 3.68 GB. 201 are `projects/<enc cwd>/<id>.jsonl`; 1,772 are nested: 925 `projects/<enc>/<id>/subagents/agent-*.jsonl`, 704 `…/subagents/workflows/wf_*/agent-*.jsonl`, 35 `…/workflows/wf_*/journal.jsonl` (no usage).
2. **History reach:** the oldest record is 2026-09-29, about 25 days. `settings.json` has no `cleanupPeriodDays`, so the CLI default applies (30 days): the CLI deletes older transcripts. Claude history on a station is therefore about 30 days deep unless the owner raised that setting.
3. 203,926 `assistant` records carry `message.usage`. The keys: `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` (with `cache_creation.ephemeral_5m_input_tokens` and `ephemeral_1h_input_tokens`), `output_tokens`, `service_tier` (`standard`), `speed` (`standard` or absent), `server_tool_use`, `iterations`. Models: `claude-opus-5-5` 119,528, `claude-sonnet-5-5` 83,476, `claude-haiku-5-5` 391, `claude-haiku-4-5-20251001` 216, `claude-fable-5-1` 201, `<synthetic>` 114 (local API error placeholders).
4. **No record has `costUSD`.** A cost from a Claude transcript must be priced by Fleet Deck.
5. **Duplicates:** the key `message.id:requestId` has 105,652 distinct values for 203,926 records (ratio 1.93). Every record has the key. t3code documents the cause: one record per content block, each with the full usage of its message (`apps/server/src/provider/Drivers/claudeUsage.ts:17-21`), and copies after a resume or fork (`apps/server/src/usage/usageAggregation.ts:98-102`).
6. Nested files carry the parent's `sessionId`: 1,738 of 1,738 nested files checked (`scratch/subagent_ids.py`). All their records are `isSidechain`.
7. Other facts in the transcript: `tool_use` blocks by name (Bash 96,379, Read 7,834, Write 6,076, Edit 4,754, Agent 1,090, MCP tools by server), `tool_result` blocks with `is_error` (4,092 Bash), `system` records with `subtype: turn_duration` (`durationMs`, `messageCount`; 401), `api_error` (101), `compact_boundary` (21), and 238 `cost-state` records (`totalCostUSD`, `modelUsage` per model with `inputTokens`, `outputTokens`, `cacheReadInputTokens`, `cacheCreationInputTokens`, `costUSD`, `totalLinesAdded`, `totalLinesRemoved`).
8. A cold scan in Python of all 3.68 GB took 15.5 s; Codex 2.4 s.

Codex home `~/.codex`:

1. 349 rollout files, 610 MB, `sessions/YYYY/MM/DD/rollout-*.jsonl` (no `archived_sessions` on Lighting). **History reach:** the oldest record is 2026-04-06. Codex deletes nothing.
2. 17,467 `event_msg/token_count` records; 4 have `info: null`. `info` has `total_token_usage` (cumulative per thread), `last_token_usage`, `model_context_window`. Keys: `input_tokens` (includes the cached part), `cached_input_tokens`, `output_tokens`, `reasoning_output_tokens` (part of the output), `total_tokens`, `cache_write_input_tokens` (10,469 of 17,463).
3. **A newer record type** `token_usage_record` (9,665): `payload.thread_id`, `turn_id`, `session_id`, `root_turn_id`, `response_id`, `usage` (this response), `turn_token_usage`, `thread_token_usage` (cumulative). t3code does not read it.
4. Models from `turn_context.payload.model`: `gpt-6.1-sol` 195, `gpt-daybreak-blue-latest` 160, `gpt-5.6-sol` 154, `gpt-6-astra` 74, `gpt-5.4` 45, `gpt-5.6-luna` 39, `gpt-5.5` 22, others 1 each. Only `gpt-5.6-luna` and `gpt-5-codex` have a row in the Fleet Deck price table (`crates/fleetdeck-providers/src/prices.rs:41-60`).
5. `session_meta.source`: `vscode` 343, sub-agent 29, `exec` 4; 28 rollouts are forks (`forked_from_id`). Originators: Claude Code 261, codex-tui 57, fleetdeck 13, others.
6. Tool calls: `custom_tool_call:exec` 11,610, `function_call:exec_command` 3,368, `apply_patch` 1,148, `spawn_agent` 27.

Token buckets (`measure-buckets.json`): 105,915 deduplicated Claude records fall into 176 conversations and 780 (hour, conversation, model) buckets in 25 days. Hour buckets per conversation are small.

### 1.2 Claude cost rates, fitted from the CLI's own records

The CLI writes its own cost in each `cost-state` record (DL-443). A least-squares fit of `costUSD` over the four token counts per model (`scratch/rates.py`, `scratch/costrow.py`) gives exact rates (USD per 1M tokens; input, cache read, cache write, output):

| Model | Rows | Rates | Check |
| --- | --- | --- | --- |
| `claude-opus-5-5` (also `claude-opus-5-5[1m]` in `modelUsage`) | 11 (+110) | 4.0, 0.2, 5.0, 20.0 | the largest row ($845.99, and $2,952.20 for `[1m]`) matches to 0.02 % |
| `claude-sonnet-5-5` | 31 | 2.0, 0.1, 2.5, 10.0 | the largest row ($466.99) matches exactly |
| `claude-haiku-5-5` | 51 | 0.1, 0.01, 0.125, 0.5 | every row matches |
| `claude-haiku-4-5-20251001`, `claude-fable-5-1` | 55, 5 | no exact fit | left unpriced |

Some small rows differ by up to 37 % (probably the 1-hour cache write price; `cost-state` does not split the cache writes). The design labels every cost an estimate (§3.8).

### 1.3 What Fleet Deck's own data holds (Lighting station.db and deck.db, read only)

`measure-lighting-db.json`:

1. `sessions`: 18 rows from 2026-10-12 (17 Claude, 1 Codex), all kept forever (`crates/fleetdeck-station/src/store/migrations.rs:37-52`, comment at `store/log.rs` prune). 17 of 18 summaries hold an old count (B20), 3 count by scheme 2.
2. `events`: 152,085 rows from **2026-10-17** (7 days); `pruned_through` 75,182. The prune deletes acknowledged events older than `retention.event_days` (default 7, `crates/fleetdeck-core/src/config_knobs_station.rs:40-57`; `EVENT_RETENTION_MS` `crates/fleetdeck-station/src/store/mod.rs:155`; `prune` `store/log.rs:353-405`). Top kinds: `message.delta` 65,252, `tool.started` 25,324, `tool.completed` 25,321, `task.updated` 13,617, `file.changed` 2,484, `turn.completed` 727, `run.started` 726, `approval.requested` 24, `error` 6.
3. `turn_checkpoints`: 1,125 rows from 2026-10-12 (1,088 finalized, 37 abandoned), one per app turn, never pruned (`migrations.rs:171-193`).
4. `effects`: 425 rows from 2026-10-12 (kinds `prompt` 232, `resume` 48, `follow_up` 43, `answer` 36, `stop` 23, `start` 17); finished effects are pruned after `retention.history_days` (90, `store/mod.rs:160`, `log.rs:426-432`).
5. `shell_runs` (B8): 1 row; never pruned (`schema_v27.rs:12-28`).
6. The deck: 49 sessions on 4 stations, 8,828 timeline rows (at most 500 per session, `TIMELINE_CAP`).

### 1.4 The code that the design reuses

1. **Provider homes:** `plan_homes(policy)` (`crates/fleetdeck-station/src/history/browse.rs:64-82`) lists the homes of every configured instance through `env_for` and `resolve_home` (`history/locate.rs:54-62`: `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, else `HOME/.claude` or `HOME/.codex`). `trusted_home` (`history/trust.rs:207`) checks the home path. The walk opens directories with no link followed (`history/browse_preview.rs:217-312`, `pub(super)`).
2. **Line reader:** `history::lines::Lines` (`history/lines.rs:67-160`, `pub`) reads records with a line cap and a deadline; `usage_record::codex_usage` (`history/usage_record.rs`) is a forward scan of one rollout with `TokenTotals::from_rollout`.
3. **Prices:** `PRICE_TABLE_VERSION = "2026-10-23"` and `price_of(provider, model)` with Codex rows only (`crates/fleetdeck-providers/src/prices.rs:11`, `:41-60`, DL-444).
4. **Event transaction:** `append_at` folds the summary and then calls `requests::observe`, `effects::observe`, `shell_observe::observe`, `workflow_state::observe` in the same transaction (`store/log.rs:94-195`, the calls at `:190-193`).
5. **Periodic tasks:** `serve.rs` spawns the prune (`PRUNE_FIRST` 60 s, `PRUNE_EVERY` 1 h, `serve.rs:84-112`, spawn at `:406`).
6. **Deck → station read with a feature:** `native_routes.rs::read` (`crates/fleetdeck-deck/src/http/native_routes.rs:125-190`): `find_station`, `link_feature`, `control.request(method, None, params)` with a 15 s timeout, fixed error texts. Station side: `link/intake.rs:514` dispatches to `intake_native.rs`.
7. **Visibility:** a member's cards are `member_sessions(grant, sessions)` = `grant.scope.covers(station, session)` (`http/read_routes.rs:36-47`; `ShareScope::covers`, `store/members.rs:41-46`: `Board` covers every session, `Sessions(set)` the listed ones). `Principal` is `Owner`, `Token`, `Member` or `Display` (`store/auth.rs:85-98`).
8. **Settings knobs:** `[retention]` and the field catalog (`crates/fleetdeck-core/src/settings/fields_knobs.rs:219-240`, `K::Bool` exists, `fields.rs:90`).
9. **Board:** pages are `Route` names (`web/src/app/routes.ts:48-110`), rendered in `App.tsx` (790 lines, at the limit), linked from `Sidebar.tsx:470-500`; a member does not open the routes in `GUEST_HIDDEN_ROUTES` (`web/src/sharing/member.ts:107-120`). Design system: `--fuel` is the one color of usage and cost (`docs/v2/DESIGN-SYSTEM.md:94`), `.fd-meter` (`:207`), the one data graphic so far is the lane sparkline (`:242`). There is no chart primitive.

### 1.5 t3code, what it does and what Fleet Deck takes

1. The usage page (`apps/web/src/components/usage/UsagePage.tsx`): metric Cost / Tokens, period Past 24h / 7 / 30 / 90 days (`usageShortcuts.ts:12-17`), Totals (processed tokens, cached input, uncached input, output, cache savings), tokens and cost by type, a breakdown by model or by day/hour, an environment filter with a coverage notice per environment (`UsageCoverageNotice`, `UsagePage.tsx:1257-1298`).
2. The server reads the transcripts (`usageTranscriptReader.ts`): a byte-offset resume guarded by a hash of the 64 bytes before the offset, the unfinished last line read again next time, a per-format state saved with the offset (`usageScanCache.ts`). The cache keeps the records of deleted transcripts (`usageScanCache.ts:15-18`).
3. Contract (`packages/contracts/src/usage.ts:39-53`): uncached input, cached input and cache creation are disjoint; reasoning is a part of the output and is never added again. Records without a price are "unpriced", never zero (`usageAggregation.ts:318-322`, `usageMerge.ts:41-46`).
4. Claude rule (`claudeUsage.ts`): `assistant` records with `message.usage`, `<synthetic>` dropped, dedupe key `message.id:requestId`, global across files.
5. Codex rule (`codexUsage.ts:150-330`): the model from the last `turn_context`; per-request `last_token_usage`, validated by the cumulative `total_token_usage`; a fork or sub-agent rollout opens with the parent's history copied in one burst (gaps under 1 s), which only sets the baseline; a cumulative that restarts at the request is a resumed process.
6. Not taken: the LiteLLM price download (a network fetch; Fleet Deck keeps one local dated table, DL-444), the speed tiers (the transcripts show `standard` only), the provider rate limits (they are on the ops panel already).

### 1.6 Corrections to the backlog

1. "No analytics page exists": true. The B14 usage glance and All usage exist (ops panel); they show the live session totals, not a history.
2. "the `events` window may not cover the whole session history": confirmed, 7 days (§1.3). The `sessions`, `turn_checkpoints` and `shell_runs` rows are kept forever, `effects` 90 days.
3. "Usage per turn: `Usage { input_tokens, output_tokens, cost_usd, cost_inferred }`": out of date since B14 (`3e8d26e0`): `Usage` has cache reads and writes, `scheme` and the partial marks. This design does not use the session totals for the time series (§2 Q1).
4. "cache tokens (not stored today, B14)": stored since B14.

## 2. Answers to the questions of the item

**Q1. Backfill: how far back does each metric go?**

| Metric | Source | Reach on Lighting |
| --- | --- | --- |
| Tokens by part, by model, by hour; cost estimate | provider transcripts | Claude 2026-09-29 (the CLI keeps 30 days), Codex 2026-04-06 |
| Sessions started, turns recorded, shell commands | `sessions`, `turn_checkpoints`, `shell_runs` (kept forever) | 2026-10-12 (the first session of the station) |
| Turn outcomes and durations, tool calls by kind and outcome, approvals and their wait, errors, files and lines changed, resumes, runs, artifacts, workflows | the station `events` (kept 7 days) and, from the build on, a durable counter table | 2026-10-17 at the build; from then on, every day |

The station keeps the transcript numbers after the CLI deletes a transcript (§3.3): from the first scan on, the Claude history grows beyond 30 days. Nothing older than a source is shown or estimated; each section says its start date.

**Q2. Where to compute?** The station computes hour buckets per conversation (transcripts) and per session (Fleet Deck counters) from its own rows. The deck asks each station, applies the visibility rule and returns the union with the station tag. The board sums the rows into days of the viewer's time zone. Raw events never leave the station for this page.

**Q3. Learnings.** Five learnings, each with a rule, a minimum sample and its source (§3.10). Rejected: the cost per merged change (no durable land count per period), the average turns to finish (no definition of "finished" that the data holds), the time to first answer (not recorded).

**Q4. Privacy.** The owner sees everything, including usage outside Fleet Deck (terminal sessions, other apps). A member sees only the rows of the sessions that the share covers, as the board cards (`member_sessions`); never the usage outside Fleet Deck. No file path, no working folder, no prompt, no tool input and no tool name of an MCP server leaves the station; only numbers, model names, the eight `ToolKind` names, session ids and hours (§4).

**Q5. Cost.** Every cost is "Cost estimate", at API list prices from the Fleet Deck price table, with its date (`Prices of 2026-10-24`). A model without a price counts its tokens and no cost ("no price"), never zero.

**Q6. A test per metric.** Every metric has a fixture test with a known value (§6, the tests of each task).

## 3. Design

### 3.1 Overview

```
provider homes ──scan (every 10 min, incremental)──► station.db: analytics_usage (hour buckets), analytics_seen (dedupe keys), analytics_files (offsets)
station events ──observe (event transaction)──────► station.db: analytics_activity (hour × session counters)
sessions, turn_checkpoints, shell_runs ──read at query time
                                    │
                station.analytics (link, feature analytics.v1)
                                    │
deck GET /api/analytics ── fan out, visibility filter, union ──► board #/analytics, #/analytics/<station>
```

### 3.2 Protocol (`crates/fleetdeck-proto/src/analytics.rs`, new; `pub mod analytics;` in `lib.rs`)

```rust
//! Analytics (B29, design `docs/v2/design/analytics.md`): the params and the
//! answer of the link method `station.analytics` (feature `analytics.v1`,
//! deck → station, scope `read`, not mutating) and the deck answer of
//! `GET /api/analytics`.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use crate::event::ProviderKind;
use crate::ids::{SessionId, StationId};

/// The longest window of one query: 400 days.
pub const MAX_WINDOW_MS: i64 = 400 * 24 * 60 * 60 * 1000;
/// The most usage rows plus activity rows in one answer.
pub const MAX_ROWS: usize = 50_000;

/// `station.analytics` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsQuery {
    /// Unix ms, inclusive; the station uses the hours that start at or after it.
    pub since: i64,
    /// Unix ms, exclusive. `until - since` is 1 ms to `MAX_WINDOW_MS`.
    pub until: i64,
    /// Only the rows of these Fleet Deck sessions; `None`: every session of
    /// the station. At most 10,000 ids.
    #[serde(default)]
    pub sessions: Option<Vec<SessionId>>,
    /// Also the usage of conversations that no Fleet Deck session holds (the
    /// owner only).
    #[serde(default)]
    pub outside: bool,
}

/// The transcript usage of one hour, one conversation and one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct UsageHour {
    /// Unix ms of the start of the UTC hour.
    pub hour: i64,
    pub provider: ProviderKind,
    pub model: String,
    /// The Fleet Deck session of the conversation; `None`: outside Fleet Deck.
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
    /// Read; `pending_bytes` may still be waiting.
    Ok,
    /// The home does not exist on this station.
    Missing,
    /// The home or a directory cannot be trusted or read (a reason word).
    Unavailable { reason: String },
    /// The owner turned the transcript reading off (`analytics.transcripts`).
    Off,
}

/// One provider home of the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsSource {
    pub provider: ProviderKind,
    /// 16 hex characters: SHA-256 of `/etc/machine-id` (empty when it cannot
    /// be read), a NUL and the canonical home path. Equal keys on two
    /// stations mean one home (the board counts it once). Never a path.
    pub key: String,
    #[serde(flatten)]
    pub state: SourceState,
    /// Unix ms of the oldest counted record of this home.
    pub first_at: Option<i64>,
    /// Unix ms of the end of the last full pass.
    pub scanned_at: Option<i64>,
    pub files: u64,
    /// Bytes on disk that no pass read yet (the backfill progress).
    pub pending_bytes: u64,
    /// Records that the parser could not read.
    pub malformed: u64,
    /// Records older than 400 days (not counted).
    pub skipped_old: u64,
    /// Claude only: `cleanupPeriodDays` of the home's `settings.json`, 30
    /// when it is not set (the CLI default); the CLI deletes older transcripts.
    pub keeps_days: Option<u32>,
}

/// Since when the counters of a family hold every event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsCoverage {
    /// The event counters (`metric::EVENT_KEYS`): the time of the oldest
    /// event that the station still had when the counters began (V50).
    pub events_since: Option<i64>,
    /// The kept tables (`metric::KEPT_KEYS`): the first session of the station.
    pub kept_since: Option<i64>,
}

/// `station.analytics` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct AnalyticsAnswer {
    pub read_at: i64,
    /// `PRICE_TABLE_VERSION` of the station.
    pub price_table: String,
    pub sources: Vec<AnalyticsSource>,
    pub coverage: AnalyticsCoverage,
    pub usage: Vec<UsageHour>,
    pub activity: Vec<ActivityHour>,
    /// More than `MAX_ROWS` rows: the oldest hours were left out.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "analytics/")]
pub enum StationAnalyticsState { Ok, Offline, Unsupported, Busy, Timeout, Error }

/// One station in the deck answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct StationAnalytics {
    pub station_id: StationId,
    pub name: String,
    pub state: StationAnalyticsState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<AnalyticsAnswer>,
}

/// `GET /api/analytics` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "analytics/")]
pub struct FleetAnalytics {
    pub read_at: i64,
    /// False for a member: no usage outside Fleet Deck is in the answer.
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
    pub const KEPT_KEYS: &[&str] = &[SESSIONS_CREATED, TURNS_RECORDED, SHELL_COMMANDS, SHELL_FAILED];
}

impl AnalyticsQuery {
    /// `invalid_params` text, or `None` when the query is valid.
    pub fn check(&self) -> Option<&'static str>;
    // since >= 0; since < until; until - since <= MAX_WINDOW_MS ("the window is not valid");
    // sessions.len() <= 10_000 ("too many sessions"); every id passes the session id rule.
}
```

Feature: `crates/fleetdeck-proto/src/version.rs`, after `NATIVE_SESSIONS` (`:84`):

```rust
    /// `station.analytics`: the hour buckets of the transcript usage and the
    /// Fleet Deck counters of a station (B29).
    pub const ANALYTICS: &str = "analytics.v1";
```

and `ANALYTICS` in `ALL` after `NATIVE_SESSIONS` (`:327`). Generated TypeScript in `web/src/gen/analytics/` (`make ts-types`).

### 3.3 Station storage (migration V50, `crates/fleetdeck-station/src/store/schema_v50.rs`)

V50 is the number reserved for this track; the merge renumbers if needed. Additive, forward only. Reverse: `DROP TABLE analytics_meta; DROP TABLE analytics_open; DROP TABLE analytics_activity; DROP TABLE analytics_seen; DROP TABLE analytics_usage; DROP TABLE analytics_files;`.

```sql
-- B29: one row per transcript file that the analytics scan read.
--   home      the source key (AnalyticsSource.key)
--   rel_path  the path below the home; never sent on the wire
--   offset    bytes read: the end of the last whole line
--   guard     hex SHA-256 of the 64 bytes before offset ('' at offset 0)
--   state     the JSON parser state at offset (Codex; '{}' for Claude)
--   seen_size the file size at the last pass
CREATE TABLE analytics_files (
    home        TEXT    NOT NULL,
    rel_path    TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    dev         INTEGER NOT NULL,
    ino         INTEGER NOT NULL,
    offset      INTEGER NOT NULL,
    guard       TEXT    NOT NULL,
    state       TEXT    NOT NULL,
    seen_size   INTEGER NOT NULL,
    malformed   INTEGER NOT NULL DEFAULT 0,
    skipped_old INTEGER NOT NULL DEFAULT 0,
    scanned_at  INTEGER NOT NULL,
    PRIMARY KEY (home, rel_path)
) STRICT;

-- B29: the transcript usage per UTC hour, home, provider, model and
-- conversation (Claude sessionId, Codex root thread id). Kept forever.
CREATE TABLE analytics_usage (
    hour         INTEGER NOT NULL,
    home         TEXT    NOT NULL,
    provider     TEXT    NOT NULL,
    model        TEXT    NOT NULL,
    conversation TEXT    NOT NULL,
    requests     INTEGER NOT NULL,
    input        INTEGER NOT NULL,
    cache_read   INTEGER NOT NULL,
    cache_write  INTEGER NOT NULL,
    output       INTEGER NOT NULL,
    reasoning    INTEGER NOT NULL,
    PRIMARY KEY (hour, home, provider, model, conversation)
) STRICT, WITHOUT ROWID;

-- B29: the dedupe keys of the counted records: the first 8 bytes of the
-- SHA-256 of the key text, as a signed integer. Kept 400 days.
CREATE TABLE analytics_seen (
    key INTEGER PRIMARY KEY,
    at  INTEGER NOT NULL
) STRICT;
CREATE INDEX analytics_seen_at ON analytics_seen(at);

-- B29: the Fleet Deck counters per UTC hour and session. Kept forever.
CREATE TABLE analytics_activity (
    hour       INTEGER NOT NULL,
    session_id TEXT    NOT NULL,
    metric     TEXT    NOT NULL,
    value      INTEGER NOT NULL,
    PRIMARY KEY (hour, session_id, metric)
) STRICT, WITHOUT ROWID;

-- B29: the open turns, tool calls and approvals, for their kind and duration.
CREATE TABLE analytics_open (
    session_id TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('turn', 'tool', 'approval')),
    id         TEXT    NOT NULL,
    started_at INTEGER NOT NULL,
    detail     TEXT,   -- the snake_case ToolKind of a tool
    PRIMARY KEY (session_id, kind, id)
) STRICT, WITHOUT ROWID;

-- B29: the counters hold every event with station_seq > backfilled_through
-- (the observe) and every event of the table at V50 (the backfill).
CREATE TABLE analytics_meta (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    events_since       INTEGER,
    backfilled_through INTEGER NOT NULL,
    backfill_next      INTEGER NOT NULL,
    backfill_done      INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO analytics_meta (id, events_since, backfilled_through, backfill_next)
SELECT 1, (SELECT min(at) FROM events),
       (SELECT COALESCE(max(station_seq), 0) FROM events),
       (SELECT COALESCE(min(station_seq), 1) FROM events);
```

Size: on Lighting, 106 k distinct records in 25 days give about 1.5 M `analytics_seen` rows per year at most (about 30 bytes each, 45 MB); the 400-day prune bounds it. `analytics_usage` had 780 rows for 25 days.

The history prune (`log.rs:353-405`, `prune`) adds in its transaction: `DELETE FROM analytics_seen WHERE at < now - 400 days`. `analytics_usage` and `analytics_activity` are not pruned (a few thousand rows a month).

### 3.4 The transcript parsers (`crates/fleetdeck-station/src/analytics/`, new module, `pub mod analytics;` in `lib.rs`)

`analytics/mod.rs` declares `pub mod claude; pub mod codex; pub mod scan; pub mod query; mod walk;` and:

```rust
/// One counted API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub at: i64,
    pub model: String,
    /// Claude `sessionId`; Codex root thread id.
    pub conversation: String,
    /// The text of the dedupe key; `None`: no key (counted without dedupe).
    pub key: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

/// What a parser makes of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed { Record(UsageRecord), Nothing, Malformed }
```

**Claude** (`analytics/claude.rs`): `pub fn parse_line(line: &[u8]) -> Parsed`. Stateless.

1. A line without the bytes `"usage"` or without `"assistant"`: `Nothing` (cheap gate, no JSON parse).
2. Parse into a typed struct that reads only `type`, `timestamp`, `sessionId`, `requestId`, `message.id`, `message.model`, `message.usage.{input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens}` (every other field `serde::de::IgnoredAny`). A parse error: `Malformed`.
3. `type != "assistant"`, or `message.usage` absent: `Nothing`. `message.model` absent, empty or `<synthetic>`: `Nothing`.
4. `timestamp` not RFC 3339, or `sessionId` absent or empty: `Malformed`.
5. Record: `input = input_tokens`, `cache_read = cache_read_input_tokens`, `cache_write = cache_creation_input_tokens`, `output = output_tokens`, `reasoning = 0`; a missing count is 0. `key = Some(format!("claude:{id}:{request}"))` with `""` for a missing part; `None` when both are missing.

**Codex** (`analytics/codex.rs`): `pub fn parse_line(line: &[u8], state: &mut CodexState) -> Parsed`. The state is saved with the offset (`analytics_files.state`, JSON).

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexState {
    /// The model of the last `turn_context`.
    pub model: String,
    /// The id of the first `session_meta` (this file's thread).
    pub thread: String,
    /// `session_meta.payload.source.subagent.thread_spawn.parent_thread_id`.
    pub parent: Option<String>,
    pub seen_meta: bool,
    /// The fork or sub-agent copy burst: copies set the baseline only.
    pub copying: bool,
    pub copy_anchor_ms: i64,
    /// The last cumulative totals of the thread.
    pub baseline: Option<Totals>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals { pub input: u64, pub cached: u64, pub cache_write: u64, pub output: u64, pub reasoning: u64 }
```

1. Gate: only lines that contain one of `"token_count"`, `"token_usage_record"`, `"turn_context"`, `"session_meta"`.
2. `session_meta`: only the first one (`seen_meta`) sets `thread` (`payload.id`, else `payload.session_id`) and `parent`; when `payload.forked_from_id` is a string or `parent` is set, and the record time parses: `copying = true`, `copy_anchor_ms = time`. `Nothing`.
3. `turn_context` with a string `payload.model`: `model = it`. `Nothing`.
4. A usage line gives `(time, cumulative C, request L)`:
   - `event_msg` with `payload.type == "token_count"`: `C = payload.info.total_token_usage`, `L = payload.info.last_token_usage`; `info` null or either object missing: `Nothing`.
   - `token_usage_record`: `C = payload.thread_token_usage`, `L = payload.usage`.
   - A totals object needs `input_tokens` and `output_tokens`; the other keys default to 0. Missing: `Malformed`. A time that does not parse: `Malformed`.
5. Rules, in order (from t3code `codexUsage.ts:216-300`, with the cumulative as the count; DL-562):
   1. `copying` and `time - copy_anchor_ms < 1000`: `copy_anchor_ms = time`, `baseline = Some(C)`, `Nothing`. Else `copying = false`.
   2. `baseline == Some(C)`: `Nothing` (the same request reported twice: a `token_count` and its `token_usage_record`, or a replay).
   3. `L.input_tokens + L.output_tokens == 0`: `baseline = Some(C)`, `Nothing` (a context-size report).
   4. `baseline` is `None`, or some field of `C` is below the baseline (a new process restarted its counters): the count is `L`.
   5. Else the count is `C - baseline` field by field.
   6. `baseline = Some(C)`. A count whose four token parts are all 0: `Nothing`.
6. Record: `input = count.input - count.cached - count.cache_write` (saturating; Codex input includes the cache), `cache_read = count.cached`, `cache_write = count.cache_write`, `output = count.output`, `reasoning = count.reasoning`; `model` = `state.model`, `"unknown"` when empty; `conversation = parent.unwrap_or(thread)` (`"unknown"` when both are empty); `key = Some(format!("codex:{thread}:{}", C.input + C.output))` (the same rollout under `sessions/` and `archived_sessions/` counts once).

### 3.5 The scan (`analytics/walk.rs`, `analytics/scan.rs`)

1. **Homes:** the homes of `plan_homes(policy)` (made `pub(crate)` in `history/browse.rs`), each through `trusted_home(path, uid, &Mounts::load())`. The source key is §3.2 `AnalyticsSource.key`. A home that does not exist: `Missing`; a failed trust check: `Unavailable { reason }` with the `PathCheck` word.
2. **Walk** (`walk.rs`, a new walker with the primitives of `history/browse_preview.rs`: `descend`, `statat` with `SYMLINK_NOFOLLOW`, regular files only, links never followed). Patterns:
   - Claude: `projects/*/*.jsonl`, `projects/*/*/subagents/*.jsonl`, `projects/*/*/subagents/workflows/*/*.jsonl` without the name `journal.jsonl`.
   - Codex: `sessions/*/*/*/rollout-*.jsonl`, `archived_sessions/rollout-*.jsonl`.
   - Budget per pass: 20,000 directories, 100,000 files; over it, the pass stops the walk, logs `analytics walk truncated` and counts the home as `Unavailable { reason: "too_many_files" }` for this pass.
3. **Read** each candidate whose `(dev, ino, size)` differs from its row or that has no row:
   - Open with `O_NOFOLLOW`; `fstat`: a regular file of the station user, else skip it (no row).
   - Resume at `offset` only when the row has the same `dev` and `ino`, `size >= offset`, and the SHA-256 of the 64 bytes before `offset` equals `guard`; else start at 0 with a fresh state (the dedupe keys make a second read harmless).
   - Read whole lines with `history::lines::Lines` (`line_max` 16 MiB, a longer line counts as malformed); a last line without its newline stays for the next pass.
   - Records with `at < now - 400 days`: `skipped_old += 1`, not counted.
   - **One transaction per file** (station store, `store/analytics.rs`): for each record, `INSERT OR IGNORE INTO analytics_seen(key, at)`; when it inserted a row (or the record has no key), upsert the hour bucket (`hour = at - at.rem_euclid(3_600_000)`); then update the file row (offset, guard, state, sizes, counters, `scanned_at`). The offset and the counts commit together.
4. **Schedule** (`scan.rs`, `pub async fn run(manager: SessionManager, enabled: bool, shutdown: watch::Receiver<bool>)`, spawned in `serve.rs` next to the prune at `:406`): the first pass 60 s after the start, then one every 10 min. A pass reads at most 2 GiB or runs at most 5 min; when it stops with bytes left, the next pass comes after 30 s (catch-up). The reading runs on one blocking thread (`spawn_blocking`); the store writes go through the store as usual. A pass that fails logs `analytics scan failed` with the error kind (never a path) and the loop goes on. The `Refresh` of the board does not start a pass (it reads the store).
5. **Off:** with `analytics.transcripts = false` the task does not read; the stored rows stay; every source has `state: off`.
6. `pending_bytes` = sum over the rows of the last walk of `max(0, size on disk - offset)`, plus the size of files without a row; kept in memory by the scanner (a `Mutex` in a shared `ScanStatus`, read by the query) with `scanned_at` and the per-home state.
7. `keeps_days` (Claude): `cleanupPeriodDays` of `<home>/settings.json` when it is an integer 1 to 36,500, else 30. Read at each pass; a read error gives 30.

### 3.6 The Fleet Deck counters (`crates/fleetdeck-station/src/store/analytics_observe.rs`)

`pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope, now: i64) -> Result<()>`, called in `append_at` **before** `workflow_state::observe` (`log.rs:193`), so that a new workflow is still unknown to `workflow_runs`. It does nothing for an event with `station_seq <= analytics_meta.backfilled_through` (the backfill counts those). `hour` is the event `at` floored to the hour. `add(metric, n)` upserts `analytics_activity(hour, session, metric) += n`.

| Event | Counts |
| --- | --- |
| `session.started` | `resumed == Some(true)`: `resumes`, else `starts` |
| `run.started` | `runs` |
| `turn.started` | insert `analytics_open(turn, turn_id, at)` (replace) |
| `turn.completed` | `turns.done` / `turns.interrupted` / `turns.failed` by `status`; when an open `turn` row exists: `turns.ms += at - started_at`, `turns.timed += 1`, delete it |
| `tool.started` | `tools.<kind>`; insert `analytics_open(tool, node_id, at, kind)` |
| `tool.completed` | status `failed` or `denied`: `tools.failed.<kind>` or `tools.denied.<kind>`, kind from the open row (`other` without one); delete the open row |
| `approval.requested` | `approvals.asked`; insert `analytics_open(approval, request_id, at)` |
| `approval.resolved` | `approvals.answered.<by>`; with an open row: `approvals.wait_ms += at - started_at`, `approvals.waited += 1`; delete it |
| `approval.expired` | `approvals.expired`; delete the open row |
| `error` | `errors`, and `errors.<code>` (or `errors.other`) |
| `turn.finalized` with `diff_summary` | `files.changed += files_changed`, `lines.added += insertions`, `lines.removed += deletions` |
| `artifact.published` | `artifacts` |
| `workflow.updated` | `workflows` when no `workflow_runs` row has `(session_id, task_id)` |
| `run.completed`, `session.ended`, `session.state` to `exited` or `lost` | delete the session's open `tool` rows (a run that ended runs no tool) |
| `session.ended`, `session.state` to `exited` or `lost` | delete all the session's open rows |

Every other kind: nothing (a `match` on the kind name first, so `message.delta` costs no JSON work).

**Backfill** (`analytics_observe.rs`, `pub fn backfill_step(conn: &mut Connection, limit: u32) -> Result<bool>`): in one transaction, read the events with `backfill_next <= station_seq <= backfilled_through` in `station_seq` order, at most `limit` (2,000), apply the same table through `observe_one` (the body of `observe` without the `backfilled_through` check), set `backfill_next` past the last one, and set `backfill_done = 1` at the end. Returns `true` while work is left. The scanner task (§3.5) runs `backfill_step` until done before its first scan pass (on Lighting about 77,000 events, 39 steps). A turn or tool that started before the first stored event has no open row: its outcome counts, its duration does not.

### 3.7 The query (`crates/fleetdeck-station/src/analytics/query.rs`, `store/analytics.rs`)

`pub async fn answer(manager: &SessionManager, status: &ScanStatus, query: AnalyticsQuery) -> Result<AnalyticsAnswer, HistoryError>`:

1. `query.check()`; a refusal is `invalid_params` with the text.
2. One query at a time per station (a `Semaphore(1)` in `ScanStatus`, wait 2 s, else `busy` "another analytics request is running").
3. **Conversations → sessions:** `SELECT provider, native_session_id, session_id, created_at FROM sessions WHERE native_session_id IS NOT NULL`. A conversation of more than one session belongs to the session that was created first (the one that began it). A conversation of no session is outside Fleet Deck.
4. **Usage rows:** `analytics_usage` with `hour >= floor_hour(since) AND hour < until`, grouped as stored; each row maps to its session (step 3). Kept: rows of a session in `query.sessions` (all sessions when `None`), and rows outside Fleet Deck when `query.outside`. `source` = the index of `home` in `sources`. `cost_usd` = `estimate_cost(provider, model, input, cache_read, cache_write, output)` (§3.8).
5. **Activity rows:** `analytics_activity` in the window, plus the kept tables grouped by hour and session: `sessions.created` (`sessions.created_at`), `turns.recorded` (`turn_checkpoints` with `state IN ('finalized', 'abandoned')`, `created_at`), `shell.commands` and `shell.failed` (`shell_runs.created_at`; failed: `exit_code IS NOT NULL AND exit_code <> 0`). Filtered by `query.sessions`.
6. `coverage.events_since` = `analytics_meta.events_since` (the time of the oldest event at V50; for a station with no event at V50, the time of the first event after it, set by `observe` when the field is NULL); `coverage.kept_since` = `min(sessions.created_at)`.
7. More than `MAX_ROWS` rows: drop the oldest hours until it fits, `truncated = true`.
8. `sources` from `ScanStatus` and `analytics_files` (`files` = rows of the home, `malformed`, `skipped_old` sums) and `first_at` = `min(hour)` of the home in `analytics_usage`.

Link: `crates/fleetdeck-station/src/link/intake_analytics.rs` (`pub(super) async fn analytics(ctx, params) -> Outcome`, in the style of `intake_native.rs`), dispatched at `link/intake.rs:514` as `"station.analytics" => intake_analytics::analytics(ctx, req.params).await,`. Error codes: `invalid_params`, `busy`, `internal`. The station checks the params again; it trusts the deck for `sessions` and `outside` (the deck is the authority on members, as for every member read).

### 3.8 Prices (`crates/fleetdeck-providers/src/prices.rs`)

1. `PRICE_TABLE_VERSION = "2026-10-24"`.
2. New, beside `price_of` (which keeps its Codex-only rule for the live counts, DL-444):

```rust
/// The price of a model for an analytics estimate (B29): the Codex rows of
/// `price_of` and the Claude rows fitted from the CLI's own `cost-state`
/// records (design `analytics.md` §1.2). `None`: no price.
#[must_use]
pub fn estimate_price_of(provider: ProviderKind, model: &str) -> Option<Price>;
// Claude rows (after `without_date`): "claude-opus-5-5" → (4.0, 0.2, 5.0, 20.0);
// "claude-sonnet-5-5" → (2.0, 0.1, 2.5, 10.0); "claude-haiku-5-5" → (0.1, 0.01, 0.125, 0.5).
// A trailing "[1m]" is removed before the match. Codex: `price_of`.

/// The estimate of one bucket; `None` when the model has no price.
#[must_use]
pub fn estimate_cost(provider: ProviderKind, model: &str, new_input: u64, cache_read: u64, cache_write: u64, output: u64) -> Option<f64>;
```

The module doc gets one line: "Claude rows serve the analytics estimate only; the live Claude cost stays the CLI's own."

### 3.9 Deck (`crates/fleetdeck-deck/src/http/analytics_routes.rs`, new)

`GET /api/analytics?since=<ms>&until=<ms>[&station=<name>]` → `FleetAnalytics`.

1. Methods table (`methods_table_rest.rs`, after `stations.native_preview` at `:581`): `m("analytics.read", Method::GET, "/api/analytics", Some(Scope::Read), false).member(),` with the comment `// The analytics page (B29): the owner and members; a member gets the rows of the covered sessions only.` Route in `http/mod.rs` next to `:605`.
2. Principal: `Owner` → every station (or the named one), `sessions: None`, `outside: true`, `owner: true`. `Member` with `ShareScope::Board` → `sessions: None`, `outside: false`. `Member` with `ShareScope::Sessions(set)` → only the stations in the set, each with `sessions: Some(<its ids>)`, `outside: false`. `Token` and `Display` → 403 `owner_only` ("Only a board login reads analytics.").
3. Params: `since` and `until` integers with the rule of `AnalyticsQuery::check` (400 `invalid_params` with its text); `station` a plain name (`plain_name` rule of `native_routes.rs`); an unknown name: 404 `unknown_station`; a member asking a station with no covered session: 404 `unknown_station` (no hint that it exists).
4. Fan out to the chosen stations at once (`futures::future::join_all`), each with the `native_routes.rs::read` steps: `link_feature(…, feature::ANALYTICS)` → `Down`: `offline`, `Without`: `unsupported`; the call `station.analytics` with a 15 s timeout → `timeout`; station `busy` → `busy`; another error or an answer that does not parse → `error` (a warning without the text). One request per principal at a time: a deck-wide `Semaphore(4)`; when none is free in 2 s, 503 `busy` "The deck is busy. Try again in a moment.".
5. **Defense in depth:** for a member, the deck drops every `UsageHour` without a `session_id` or with a session that `grant.scope.covers` does not cover, and every such `ActivityHour`, before it answers.
6. Stations in the answer: by name. Nothing is stored or cached. A failure writes an audit row as `native_routes.rs::audit` (action `analytics.read`, the code only); a success writes none.

### 3.10 Board

Files under `web/src/analytics/` (new): `api.ts` (fetch), `merge.ts` (sums and local days), `learnings.ts`, `AnalyticsPage.tsx`, `Bars.tsx`, `analytics.css`.

**Routes** (`routes.ts`): `{ name: 'analytics'; station: string | null }`, `#/analytics` (fleet) and `#/analytics/<station name>` (URI-encoded). Sidebar: a nav row "Analytics" (Lucide `chart-column`) after Activity, for the owner and for members (not in `GUEST_HIDDEN_ROUTES`). Layout: in `BAR_ROUTES` and `MORE_TAB`; `PHONE_TITLES.analytics = 'Analytics'`; a More row on the phone. `App.tsx` (790 lines): the case is one line, `case 'analytics': return <AnalyticsPage route={route} stations={stations} sessions={sessions} />;` (any extra line moves the `case 'more'` body into `app/MoreRoute.tsx`).

**Merge** (`merge.ts`, pure functions, tested):

```ts
export type Period = 1 | 7 | 30 | 90 | 365; // days; 1 = the past 24 hours
export type Totals = {
  readonly input: number; readonly cacheRead: number; readonly cacheWrite: number;
  readonly output: number; readonly reasoning: number; readonly requests: number;
  /** Sum of the priced buckets. */ readonly cost: number;
  /** Tokens of buckets without a price. */ readonly unpricedTokens: number;
  readonly unpricedModels: readonly string[];
};
export type Day = { readonly key: string /* YYYY-MM-DD local */; readonly start: number; readonly totals: Totals };
export type Merged = {
  readonly total: Totals; readonly outside: Totals | null; // null for a member
  readonly days: readonly Day[]; // every local day of the window, oldest first
  readonly models: readonly { provider: string; model: string; totals: Totals }[]; // by tokens, largest first
  readonly stations: readonly { id: string; name: string; state: string; totals: Totals | null; pending: number }[];
  /** Local midnight of the earliest `first_at` of the counted sources; days before it have no data. */
  readonly usageSince: number | null;
  readonly counts: ReadonlyMap<string, number>; // metric key → sum
  readonly eventsSince: number | null; // the latest `events_since` of the stations that answered
  readonly keptSince: number | null; // the earliest `kept_since`
  readonly priceTables: readonly string[];
  readonly duplicateSources: readonly string[]; // "Claude on thunder (same home as Lighting)"
};
export function merge(answer: FleetAnalytics, since: number, until: number, zone: string): Merged;
```

Rules: tokens = `input + cacheRead + cacheWrite + output` (reasoning is a part of output, never added). Days are local days of the browser's zone (`Intl.DateTimeFormat(undefined).resolvedOptions().timeZone`) through `Intl`; an hour row counts on the local day of its start. Only stations with `state: ok` count. A source key seen on two stations counts on the first by station name only; the second goes to `duplicateSources`. The window: `until = now`, `since = now - period days` (the past 24 hours: `now - 24 h`).

**Page** (`AnalyticsPage.tsx`, `Page` with `wide`):

- Header: title `Analytics`; description `Tokens and cost from the provider transcripts of each station, and the work of the Fleet Deck sessions.`; for a member `Your shared sessions only.` Filters: station `select.fd-ghost fd-sm` (`All stations`, then the station names; it navigates), period `select.fd-ghost fd-sm` (`Past 24 hours`, `7 days`, `30 days` (default), `90 days`, `365 days`), chart metric `.fd-seg` (`Cost`, `Tokens`; default `Cost`), Refresh icon button (`aria-label="Refresh analytics"`). The page reads once on open, on a filter change and on Refresh; no polling.
- **Totals** (stat values over one-line labels, the ops glance look): `Tokens`, `New input`, `Cache read`, `Cache write`, `Output`, `Cost estimate` (`--fuel`; `title`: `At API list prices from the Fleet Deck price table of <date>. A subscription does not bill it. Models without a price are not in it.`). Under them, when `unpricedTokens > 0`: `.fd-note` `<n> tokens of <model list> have no price and are not in the cost.` For the owner, one line `Outside Fleet Deck (terminal and other apps): <tokens> tokens · <cost>` when `outside` has tokens.
- **By day** (`Bars.tsx`, §3.11): one bar per local day of the window (24 hour bars for the past 24 hours), cost or tokens. Under the chart, one line per provider with data: `Claude transcripts on <station> start on <Mon D> (the CLI keeps them <n> days).` / `Codex transcripts on <station> start on <Mon D>.`; while `pending_bytes > 0`: `Reading the transcripts of <station>: <pct> % done.`
- **By model**: a table: Model, Provider, Tokens, a `.fd-meter` of its share of the tokens, Cost estimate (or `no price`).
- **By station** (fleet page only, owner): Station, Tokens, Cost estimate, then the state text: `offline` → `Offline: not in the totals.`; `unsupported` → `Update this station to see its analytics.`; `busy` → `Busy: try again.`; `timeout` → `No answer in time.`; `error` → `The station could not answer.`; a source `off` → `Transcript reading is off (Settings → station).`
- **Fleet Deck work**: rows with a label and a count. Kept counters first, with the line `Since <keptSince date>.`: `Sessions started`, `Turns`, `Shell commands` (`, <n> failed`). Then the event counters with the line `Since <eventsSince date>. The station keeps its events 7 days; Fleet Deck counts them from then on.`: `Turns that ended` (`<done> done · <interrupted> interrupted · <failed> failed`), `Average turn` (duration, from `turns.ms / turns.timed`), `Tool calls` with one row per kind (label `Shell`, `Edit`, `Read`, `Search`, `Web`, `MCP`, `Subagent`, `Other`; the count; a neutral `.fd-meter` of its share; `<n> failed`, `<n> denied` when > 0), `Approvals` (`<asked> asked · <answered> answered · <expired> expired`), `Average wait for an answer`, `Resumes`, `Files changed`, `Lines` (`+<added> −<removed>`), `Artifacts published`, `Workflows`, `Errors`. A row whose count is 0 shows `0`; a section whose coverage starts after the window end shows `No data in this period.`
- **Learnings** (§3.12): a list; each item a sentence and a `title` with its rule. No item passes: the section is absent.
- **Sources** (a fold, closed): one line per station and home: `<station> · <Claude|Codex>: <files> files, since <date>, read <age>` (or the state text); `Prices of <date>` (all price tables when they differ: `Prices of 2026-10-24 (Lighting, dev1) and 2026-10-23 (thunder)`); `Counted once: <duplicateSources>`; `Some old hours are not shown: the answer of <station> was too large.` when `truncated`.
- Empty: no station answered → the `.fd-empty` state `No station answered. Analytics needs a station that is online and up to date.`; the window has no rows → `No usage in this period.`

### 3.11 Chart and the design system

The design system has no chart (§1.4 item 9). The page needs one series over time, used also by a later station page: an expansion by the skill rule (two places or more). Added to `DESIGN-SYSTEM.md` §3 and the §7 log:

- `.fd-bars`: a bar chart of one series. An inline SVG, `width: 100%`, height 160px (phone 120px). Bars `--fuel` (usage and cost) or `--dim` with `data-tone="neutral"`; a 1px `--border-soft` zero baseline; the gap between bars 2px (1px when there are more than 90); the y scale starts at 0; one top grid line at the rounded maximum with its value in 12px `--faint` (tabular figures); x labels: the first, the middle and the last day, 12px `--faint`. A day with no data inside the coverage is a zero (no bar, the baseline shows). Days before the coverage start are not drawn as zero: they get a 1px dashed `--border` vertical line at the start and the label `No data before <Mon D>` in 12px `--faint`. Hover or keyboard focus of a bar (each bar is a focusable `<rect tabIndex=0>` with `aria-label` `<date>: <value>`): a 2px `--focus` outline and the readout line above the chart (`<date> · <value>`, 13px `--text`); without a focus the readout shows the window total. The SVG has `role="group"` and `aria-label` `<metric> by day`; a visually hidden `<table>` repeats the values. Forced colors: bars `CanvasText`, focus `Highlight`. No animation.
- The other parts use existing primitives: `.fd-meter` (shares), `.fd-note`, `.fd-empty`, `select.fd-ghost`, `.fd-seg`, tables of the settings look.

### 3.12 Learnings (`learnings.ts`)

Each learning is computed over the merged rows of the window, only from the part covered by its source; when the coverage starts after the window start, the sentence ends with ` (since <Mon D>)`.

| Id | Shown when | Sentence | Rule (`title`) |
| --- | --- | --- | --- |
| `cache` | `requests >= 100` | `<p> % of the input came from the prompt cache.` | `Cache read tokens divided by all input tokens (new input, cache read and cache write), from the provider transcripts.` |
| `tool_failures` | a kind with `tools.<kind> >= 20` and failed share `>= 5 %`; the kind with the highest share | `<Kind> calls fail most often: <p> % of <n> failed.` | `Failed tool calls divided by all tool calls of each kind, kinds with 20 calls or more.` |
| `approval_wait` | `approvals.waited >= 5` | `An approval waited <duration> on average for its answer (<n> answered).` | `The time from the request to the answer, averaged over the answered approvals.` |
| `cost_per_turn` | Fleet Deck `turns.recorded >= 20` and the Fleet Deck sessions' usage in the window has no unpriced tokens | `A turn cost about <usd> at API prices (estimate, <n> turns).` | `The cost estimate of the Fleet Deck sessions divided by their turns, in this period.` |
| `unfinished_turns` | `done + interrupted + failed >= 20` | `<p> % of <n> turns ended interrupted or failed.` | `Interrupted and failed turns divided by all turns that ended.` |

Percentages are whole numbers (`Math.round`); `cost_per_turn` uses `usdText`.

### 3.13 Settings

`station.toml` table `[analytics]`, key `transcripts` (bool, default `true`): `crates/fleetdeck-core/src/config_knobs_station.rs` (`StationAnalytics { transcripts: bool }`), `config_raw.rs`, `config.rs` (the same `match raw.analytics` as `retention`, `config.rs:458`), and the field (`settings/fields_knobs.rs`, after `retention.raw_log_mib`):

```rust
field(
    "analytics.transcripts",
    "Read provider transcripts for Analytics",
    K::Bool,
    Restart,
    "The station reads the token counts of the Claude and Codex transcripts in its provider homes. It stores numbers and model names, never text. Default on.",
),
```

### 3.14 Compatibility

1. **Older station** (no `analytics.v1`): `unsupported`, `Update this station to see its analytics.` Nothing breaks.
2. **Older deck with a newer station:** the station answers nothing that the deck asks; the scanner and the counters run anyway (cheap).
3. **Board and deck** ship in one binary. A board loaded before a deck upgrade has no Analytics link until it reloads.
4. **Migration V50** is additive. A station downgraded after V50 refuses the newer schema as it does for every migration (`rusqlite_migration` checks the user version).
5. **Price table date** changes to `2026-10-24`: new live Codex usage events carry the new date; the costs do not change (no Codex row changes).
6. **Feature flag:** `analytics.v1`, negotiated per link like `native_sessions.v1`.
7. No change to `Usage`, `SessionSummary`, the fold or any existing event.

### 3.15 Errors and edge cases

| Case | Result |
| --- | --- |
| A transcript is rotated or rewritten in place | the guard or the inode differs: read from 0; the dedupe keys skip the counted records |
| The CLI deletes a transcript | its row stays (counted numbers stay); the next walk does not find it; nothing is subtracted |
| A Claude fork copies messages into a new file | the same `message.id:requestId`: counted once |
| A Codex rollout under `sessions/` and `archived_sessions/` | the same `codex:<thread>:<cumulative>` keys: counted once |
| A Codex fork or sub-agent file opens with copied history | copies within 1 s of each other set the baseline only |
| A Codex thread resumed in a new process restarts its cumulative | rule 5.4: the request's own usage counts |
| A line longer than 16 MiB | malformed, skipped |
| The station restarts in the middle of a file | offset and counts commit together: no double count, no loss |
| Two stations read one home (same machine id and path) | the board counts it once and says so |
| Two Fleet Deck sessions hold one conversation (an import of a station session) | the rows go to the session created first |
| A member's share changes | the deck reads the grant on each request (`ShareGrant` is loaded per request) |
| A station's clock is wrong | hours are on the station clock; the board does not correct them |
| A record older than 400 days | not counted, `skipped_old` |
| A model without a price | tokens count, `cost_usd: None`, `no price` |
| `turn.completed` without its `turn.started` (backfill edge) | the outcome counts, the duration does not |

### 3.16 Out of scope

- Speed tiers, the long-context premium, the 1-hour cache write price (the fit of §1.2 prices all cache writes at one rate).
- A price editor on the board (t3code `UsagePriceOverrides`); the table changes with a release.
- Export (CSV), alerts, budgets.
- Per-member attribution of usage (who typed); the counters are per session.
- The MCP `fleet_view` and the CLI do not get analytics.
- Other providers than Claude and Codex.

## 4. Threat notes

1. **Transcript text.** The station reads files that hold prompts, tool output and secrets. The parsers keep only numbers, the model name, the conversation id and the time; the typed Claude struct ignores every other field without storing it; nothing of a line is logged. `analytics_files.rel_path` holds the encoded working folder: station-local only, never in an answer or a log.
2. **What leaves the station:** numbers, model names, the eight tool kinds, the error codes that match `^[a-z][a-z0-9_]{0,31}$`, session ids, hours, the source key (a hash, not a path), the price table date. No path, no cwd, no MCP server name, no prompt, no tool input.
3. **Members.** The deck computes the station params from the member's share (§3.9 step 2) and filters the rows again (step 5). A member never gets usage outside Fleet Deck, never a station outside the share (404 as for an unknown name), never another session's counters. The station trusts the deck for this, as for every member read over the link.
4. **Tokens and walls** get 403: analytics is a board login read.
5. **Reading the home.** The walk follows no link; a file must be a regular file of the station user; the home passes `trusted_home`. A process of the owner can write a transcript with false numbers; that changes only the owner's own page (no privilege boundary).
6. **Load.** One scan thread, at most 2 GiB or 5 min per pass; one query at a time per station; a deck-wide limit of 4; answers at most 50,000 rows. A cold Lighting backfill reads 4.3 GB once.
7. **Storage.** The seen-key table is bounded by the 400-day prune (about 45 MB on the busiest station).

## 5. Risk class

**High.** A station migration (V50), a new link method and route, and a member visibility rule over data of other sessions. Adversary DESIGN REVIEW of this document before the build; PRE-PR REVIEW of the final branch. Test focus of the review: the member filter (deck params and the second filter), the dedupe and resume rules (no double count), the parsers on hostile input (huge lines, missing fields), and the migration.

## 6. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Workers compile only (`cargo check -p <crate> --tests`, `bun run typecheck`, `bun run lint`) and keep every file under 800 lines; the coordinator runs the tests at the merge (owner rule 2026-10-23). At most two workers at a time (machine rule).

Order: A1 ∥ A2 → A3 → A4 ∥ A5 → A6 → A7 ∥ A8 → A9.

### A1 (backend): protocol and prices (≈ 330 lines)

- **Owned files:** `crates/fleetdeck-proto/src/analytics.rs` (new), `crates/fleetdeck-proto/src/analytics_tests.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-providers/src/prices.rs`, `crates/fleetdeck-providers/src/prices_tests.rs` (new or existing test module of `prices.rs`), `web/src/gen/analytics/*` (generated), the ts-drift list if the proto crate keeps one.
- **Changes:** §3.2 (types, `metric`, `check`, feature), §3.8.
- **Tests:** `analytics_query_check_refuses_a_bad_window` (since < 0, since ≥ until, over 400 days, 10,001 sessions); `analytics_answer_round_trips_and_omits_empty_options` (JSON of a `UsageHour` without session and cost has neither key); `source_state_is_flattened` (`{"state":"unavailable","reason":"owner",…}`); `estimate_price_of_has_the_fitted_claude_rows` (the three rows, `[1m]` and a date suffix removed, haiku 4.5 and fable `None`); `estimate_cost_matches_the_recorded_cost_states`: opus (18,601 input, 1,039,922,797 cache read, 119,025,395 cache write, 2,140,297 output) → 845.9919 ± 0.01; sonnet (21,998; 1,734,626,170; 68,605,469; 12,197,000) → 466.9903 ± 0.01; haiku (18; 114,047; 127,790; 2,069) → 0.01815 ± 0.00001; `price_of_still_has_no_claude_row`.
- **depends_on:** none.

### A2 (backend): transcript parsers (≈ 550 lines with fixtures)

- **Owned files:** `crates/fleetdeck-station/src/analytics/mod.rs` (new: §3.4 types, module list; `scan` and `query` and `walk` declared by A4/A5/A6: A2 declares only `pub mod claude; pub mod codex;`), `analytics/claude.rs`, `analytics/codex.rs`, `analytics/claude_tests.rs`, `analytics/codex_tests.rs`, `analytics/fixtures/*.jsonl` (new), `crates/fleetdeck-station/src/lib.rs` (`pub mod analytics;`).
- **Changes:** §3.4. Fixtures are hand-written with the field names of §1.1 and no prompt text (content arrays hold `"x"`).
- **Tests (each compares with a known value):**
  - Claude: `a_message_counted_once_across_its_content_blocks` (3 records with one `message.id`/`requestId`: 3 `Record`s with one key; the store dedupe is A3); `synthetic_and_usage_free_records_are_nothing`; `a_bad_timestamp_is_malformed`; `the_four_parts_map_to_new_input_cache_read_cache_write_output` (input 3, cache read 1,000, cache creation 200, output 50).
  - Codex: `token_count_deltas_follow_the_cumulative` (totals 100/0→250/50→400/50 output 10/20/30: records 100,150,150 input); `a_token_usage_record_of_the_same_request_is_not_counted_again`; `fork_copies_set_the_baseline_only` (meta with `forked_from_id` at t, copies at t+10, t+20 ms, a real record at t+5 s: one record of the delta); `a_restarted_cumulative_counts_the_request` ; `a_subagent_rollout_belongs_to_the_parent_thread`; `cached_input_is_taken_out_of_new_input` (input 1,000, cached 800, write 100 → input 100); `the_model_comes_from_the_last_turn_context`; `state_round_trips_through_json`.
- **depends_on:** none.

### A3 (backend): migration and the usage store (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-station/src/store/schema_v50.rs` (new), `store/migrations.rs` (one `M::up` line after `:659`), `store/analytics.rs` (new: file rows, the per-file transaction, seen keys, buckets, the reads of §3.7 steps 3-5 and 8), `store/analytics_tests.rs` (new), `store/mod.rs` (module line; the `Store` methods that wrap `analytics.rs`), `store/log.rs` (the `analytics_seen` prune line in `prune`).
- **Changes:** §3.3, §3.5 step 3 (the transaction), §3.7 steps 3-5.
- **Tests:** `v50_creates_the_tables_and_the_meta_row` (with events seq 5..9 at times 1,000..: `events_since` 1,000, `backfilled_through` 9, `backfill_next` 5); `a_file_commit_counts_each_key_once` (two commits with overlapping keys: one bucket, `requests` = distinct keys); `records_without_a_key_always_count`; `hour_buckets_floor_to_the_utc_hour`; `seen_keys_older_than_400_days_are_pruned`; `the_conversation_of_two_sessions_belongs_to_the_first`; `usage_outside_fleet_deck_only_with_outside`; `kept_counters_come_from_sessions_checkpoints_and_shell_runs`.
- **depends_on:** A1 (types), A2 (`UsageRecord`).

### A4 (backend): the event counters and their backfill (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-station/src/store/analytics_observe.rs` (new), `store/analytics_observe_tests.rs` (new), `store/log.rs` (the `observe` call before `workflow_state::observe`). Coordinate `log.rs` with A3: A4 starts after A3 merged into the track branch.
- **Changes:** §3.6.
- **Tests:** one per row of the §3.6 table with its exact counter (`a_turn_counts_its_status_and_duration`: started at 1,000, completed `failed` at 4,000 → `turns.failed` 1, `turns.ms` 3,000, `turns.timed` 1; `a_tool_failure_counts_under_its_kind`; `an_approval_counts_who_answered_and_the_wait`; `an_error_code_that_does_not_match_counts_as_other`; `a_finalized_turn_adds_files_and_lines`; `a_resume_is_not_a_start`; `a_new_workflow_counts_once`; `the_end_of_a_session_clears_its_open_rows`; `message_deltas_write_nothing`); `the_backfill_counts_the_old_events_once_and_the_observe_the_new` (events 1..10 before V50, 11..12 after: every counter once); `the_backfill_runs_in_steps` (limit 3 over 10 events: 4 calls, `backfill_done`).
- **depends_on:** A3.

### A5 (backend): the settings knob (≈ 120 lines)

- **Owned files:** `crates/fleetdeck-core/src/config_knobs_station.rs`, `config_raw.rs`, `config.rs`, `settings/fields_knobs.rs`, `config_knobs_tests.rs`.
- **Changes:** §3.13.
- **Tests:** `analytics_transcripts_defaults_to_true`; `analytics_transcripts_false_parses`; `the_field_is_listed_with_restart`.
- **depends_on:** none (runs in parallel with A4).

### A6 (backend): the scanner, the query and the link method (≈ 600 lines)

- **Owned files:** `crates/fleetdeck-station/src/analytics/mod.rs` (add `pub mod scan; pub mod query; mod walk;` and `ScanStatus`), `analytics/walk.rs`, `analytics/scan.rs`, `analytics/query.rs`, `analytics/scan_tests.rs`, `analytics/query_tests.rs`, `crates/fleetdeck-station/src/history/browse.rs` (`plan_homes` and `HomePlan` to `pub(crate)` only), `crates/fleetdeck-station/src/serve.rs` (spawn, and the shutdown join), `crates/fleetdeck-station/src/link/intake_analytics.rs` (new), `link/intake.rs` (one dispatch line), `link/mod.rs` (module line), the station feature advertisement if it lists features separately from `feature::ALL`.
- **Changes:** §3.5, §3.6 backfill driving, §3.7, §3.2 sources.
- **Tests:** `a_pass_reads_new_lines_only` (append to a fixture file between passes: the second pass reads from the offset); `a_rewritten_file_is_read_again_without_double_counts`; `an_unfinished_last_line_waits_for_the_next_pass`; `links_and_files_of_other_users_are_not_read` (a symlinked transcript is skipped); `nested_subagent_and_workflow_files_are_read_and_journal_is_not`; `off_reads_nothing_and_reports_off`; `keeps_days_reads_cleanup_period_days`; `a_pass_stops_at_its_byte_budget_and_reports_pending_bytes`; `the_answer_maps_conversations_to_sessions_and_prices_the_buckets` (fixture home → one Claude bucket of 1 opus request with the A1 price); `the_answer_filters_by_sessions` ; `the_answer_is_truncated_from_the_oldest_hour`; `a_second_query_is_busy`; link: `station_analytics_refuses_bad_params`.
- **depends_on:** A3, A4, A5.

### A7 (backend): the deck route (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-deck/src/http/analytics_routes.rs` (new), `http/methods_table_rest.rs`, `http/mod.rs`, `crates/fleetdeck-deck/tests/analytics_route.rs` (new), the methods-table count test if it pins the number of methods.
- **Changes:** §3.9.
- **Tests:** `the_owner_gets_every_station_with_outside`; `a_member_with_sessions_gets_only_their_stations_and_rows` (a fake station that answers rows of two sessions and outside rows: the member answer has one session's rows only, `owner: false`); `a_board_share_member_gets_no_outside_rows`; `a_member_asking_an_uncovered_station_gets_404`; `a_token_and_a_display_get_403`; `an_offline_and_an_old_station_are_states_not_errors`; `a_station_timeout_is_timeout`; `bad_params_are_400`.
- **depends_on:** A1, A6 (the station side for the integration test with a real station; with a fake station the test needs only A1).

### A8 (ux): data functions (≈ 450 lines)

- **Owned files:** `web/src/analytics/api.ts`, `web/src/analytics/merge.ts`, `web/src/analytics/learnings.ts`, `web/src/analytics/merge.test.ts`, `web/src/analytics/learnings.test.ts` (all new).
- **Changes:** §3.10 Merge, §3.12.
- **Tests:** `merge_sums_the_four_parts_and_never_adds_reasoning`; `merge_groups_hours_into_local_days` (zone `America/New_York`, an hour at 03:00 UTC goes to the day before); `merge_counts_a_duplicate_source_once`; `merge_keeps_unpriced_tokens_out_of_the_cost`; `merge_skips_stations_that_did_not_answer`; `merge_has_no_outside_for_a_member`; one test per learning: shown at its threshold, hidden one under it, the `(since …)` suffix.
- **depends_on:** A1 (generated types).

### A9 (ux): the page, the chart and the routes (≈ 600 lines)

- **Owned files:** `web/src/analytics/AnalyticsPage.tsx`, `web/src/analytics/Bars.tsx`, `web/src/analytics/analytics.css`, `web/src/analytics/AnalyticsPage.test.tsx`, `web/src/analytics/Bars.test.tsx` (new), `web/src/app/routes.ts`, `web/src/app/routes.test.ts`, `web/src/App.tsx`, `web/src/app/Sidebar.tsx`, `web/src/app/Layout.tsx`, the phone More page file that lists its rows, `docs/v2/DESIGN-SYSTEM.md` (§3 `.fd-bars`, §7 log line), the gallery entry for the new page (`web/e2e` screenshot list) if the gallery lists pages.
- **Changes:** §3.10 page and routes, §3.11. Use the fleet-design-system and dataviz skills.
- **Tests:** `routes_parse_analytics_and_a_station`; `the_page_shows_totals_and_the_price_date`; `days_before_the_transcripts_are_not_zero` (the dashed start line and `No data before …`); `a_member_page_has_no_outside_line_and_no_station_table`; `an_old_station_says_update`; `the_bars_are_focusable_with_their_values`; `the_hidden_table_repeats_the_values`.
- **depends_on:** A8.
- **Acceptance for A9 also:** `sh tools/design-check.sh`; gallery shots of the page in both themes, desktop and phone.

### At the merge (coordinator)

- `cargo nextest run -p fleetdeck-proto -p fleetdeck-providers -p fleetdeck-core --no-fail-fast -E 'test(analytics) | test(estimate) | test(price_of) | binary(ts_drift)'`
- `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(analytics) | test(v50) | test(retention) | test(prune)'`
- `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(analytics_route) | test(methods_table)'`
- `cd web && bun run typecheck && bun run lint && bun run test src/analytics src/app`
- The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
- `make check`.
- A live check on Lighting (read only of the transcripts): the station's first full pass ends; the page for 30 days shows Claude from 2026-09-29 and Codex from 2026-10 activity; the Claude tokens of one known conversation equal the sum of `scratch/buckets.py` restricted to it.
- PRE-PR REVIEW (adversary).

## 7. Decisions made in this draft

1. **The time series of tokens and cost comes from the provider transcripts only**, with one pricing rule for every row. Rejected: the session totals of B14 (no time series, events gone after 7 days, 17 of 18 Lighting sessions hold an old count) and a mix of both (two numbers for one session).
2. **The station keeps hour buckets and dedupe keys, not records.** Rejected: one row per record (about 1.5 M rows a year on Lighting); per-file buckets without keys (no dedupe across files; Claude copies messages into forks).
3. **The dedupe keys are kept 400 days and the page window is at most 365 days;** older records are not counted. Rejected: keys forever (unbounded), a 40-day key window (a late copy of an older record would count twice).
4. **Codex counts the growth of the thread's cumulative total** (both `token_count` and `token_usage_record`), with the t3code fork-copy and restart rules. Rejected: `last_token_usage` alone (the pair of records for one request would count twice); `token_usage_record` alone (older rollouts have only `token_count`).
5. **Claude rows enter the price table for the estimate only**, fitted from the CLI's own `cost-state`. Rejected: no Claude cost (the main provider would have no cost); spreading the `cost-state` totals over time (invented history); a network price list (DL-444).
6. **A conversation belongs to the Fleet Deck session that was created first.** Rejected: count it in every session (double counts in the fleet totals).
7. **The deck returns the union of the station answers; the board sums into local days.** Rejected: deck-side sums (the days must follow the viewer's time zone; the station boundary of a day would differ per station).
8. **Fleet Deck counters are written in the event transaction and backfilled from the retained events;** the kept tables (`sessions`, `turn_checkpoints`, `shell_runs`) are read at query time for all history. Rejected: a fold over the deck timeline (500 rows per session, not complete); counting from the migration only (loses 7 days that exist).
9. **Each section shows its own start date;** no metric is drawn before its source starts. Rejected: one page-wide start date (it would hide the Codex history or invent Claude history).
10. **Members see their covered sessions; usage outside Fleet Deck is owner only;** tokens and walls get 403. Rejected: owner only (the guidance asks for the fleet-view rule); outside usage for board-share members (it is the owner's private terminal work).
11. **One scan thread, every 10 min, 2 GiB or 5 min per pass;** the Refresh button does not start a scan. Rejected: a scan per page read (a cold read of 4 GB on a click).
12. **A station knob `analytics.transcripts`, default on.** Rejected: no switch (the owner must be able to stop the reading of private files); default off (the page would be empty).
13. **A new `.fd-bars` primitive** in the design system, one series, `--fuel`. Rejected: a chart library (a new dependency and a second visual language); a stacked chart by provider (needs new categorical colors; the model table shows the split).
14. **Five learnings with thresholds;** the others in Q3 are rejected for lack of a rule that the data holds.
15. **No deck migration, no deck cache.** The answers are small (a few thousand rows) and the page reads on open only.

## 8. Owner questions (the build proceeds with the defaults)

1. **Codex prices.** `gpt-6.1-sol`, `gpt-daybreak-blue-latest`, `gpt-5.6-sol`, `gpt-6-astra`, `gpt-5.4` and `gpt-5.5` have no row. Their tokens show as "no price". Default: leave them unpriced until the owner gives the rates.
2. **Claude rates fitted from the CLI's own records** (§1.2). Default: use them for the estimate, labelled "Cost estimate" with the table date.
3. **Keep the numbers after the CLI deletes a transcript** (Claude keeps 30 days). Default: yes; only numbers and model names are kept.
4. **Usage outside Fleet Deck** (terminal, VS Code, other apps that use the same provider home) on the owner's page. Default: yes, as its own line and in the totals.
5. **Default period of the page.** Default: 30 days (Claude's reach).
6. **Members see analytics of their shared sessions.** Default: yes (the visibility of the board cards).

## 9. Recordings and sessions

- `measure-lighting.json`, `measure-lighting-db.json`, `measure-buckets.json` in `/home/luismorales/fleetdeck-briefs/backlog2/analytics/` (counts, key names, model and tool names, dates; no record text). Scripts in `scratch/` (`measure.py`, `stationdb.py`, `buckets.py`, `rates.py`, `costrow.py`, `subagent_ids.py`), run with `python3 -I` on Lighting.
- The other stations (thunder, dev1, mac-m4) were not measured: the architect's fleet grant answered "The session token is not valid." No Fleet Deck session was started. The page itself shows the reach of each station's transcripts (the Sources fold), so the first live run measures them.

## Decision log entries (to append)

## DL-560: B29 analytics: tokens and cost by time come from the provider transcripts (design agent, 2026-10-24)

- Context: B29; design `docs/v2/design/analytics.md` §1-§3. The station events are kept 7 days and the session totals have no time; the transcripts hold every API response with its time (Claude 30 days by the CLI default, Codex since install).
- Decision: each station scans its provider homes every 10 minutes, incrementally by byte offset with a hash guard, and stores hour buckets per home, provider, model and conversation, with 400-day dedupe keys (Claude `message.id:requestId`, Codex thread and cumulative total). It keeps the numbers after the CLI deletes a transcript. A station knob `analytics.transcripts` (default on) stops the reading.
- Rejected: the B14 session totals as a time series; one stored row per record; a scan on each page read.

## DL-561: B29 Claude transcript records are deduplicated globally by `message.id:requestId` (design agent, 2026-10-24)

- Evidence: 203,926 records, 105,652 keys on Lighting (one record per content block, copies in forks).
- Decision: a record counts only when its key is new in `analytics_seen`; the bucket and the key commit in one transaction with the file offset.

## DL-562: B29 Codex usage counts the growth of the thread's cumulative total (design agent, 2026-10-24)

- Decision: `token_count.total_token_usage` and `token_usage_record.thread_token_usage` feed one baseline per file; an equal total is the same request; the first total of a file, or a total below the baseline, counts the request's own usage; records of a fork or sub-agent copy burst (gaps under 1 s after `session_meta`) set the baseline only; a sub-agent counts in its parent thread's conversation.
- Rejected: `last_token_usage` alone (double counts with `token_usage_record`); `token_usage_record` alone (older rollouts lack it).

## DL-563: B29 Claude rows in the price table, for the analytics estimate only (design agent, 2026-10-24)

- Decision: `estimate_price_of` adds `claude-opus-5-5` (4.0 / 0.2 / 5.0 / 20.0), `claude-sonnet-5-5` (2.0 / 0.1 / 2.5 / 10.0) and `claude-haiku-5-5` (0.1 / 0.01 / 0.125 / 0.5) USD per 1M (input, cache read, cache write, output), fitted exactly from the CLI's own `cost-state` records; `PRICE_TABLE_VERSION` becomes `2026-10-24`. `price_of` keeps its Codex-only rule (DL-444) for the live counts. Every analytics cost is "Cost estimate" with the table date; a model without a row is "no price", never zero.

## DL-564: B29 Fleet Deck counters per hour and session, in the event transaction (design agent, 2026-10-24)

- Decision: `analytics_observe` counts turns by outcome and duration, tool calls by kind and outcome, approvals by who answered and their wait, errors by code, files and lines changed, starts, resumes, runs, artifacts and workflows into `analytics_activity`; a one-time backfill counts the events that the station still holds at V50; sessions started, turns recorded and shell commands come from the kept tables at query time. Each family reports its start date (`events_since`, `kept_since`).

## DL-565: B29 the link method `station.analytics`, feature `analytics.v1` (design agent, 2026-10-24)

- Decision: params `since`, `until` (at most 400 days), `sessions` (optional filter), `outside`; the answer has the usage and activity hour rows, the sources with their reach and backfill progress, the coverage and the price table date; at most 50,000 rows (oldest hours dropped, `truncated`). One query at a time per station.

## DL-566: B29 analytics visibility: members see their covered sessions, outside usage is owner only (design agent, 2026-10-24)

- Decision: `GET /api/analytics` serves the owner (every station, outside usage included) and members (the stations and sessions of `ShareScope::covers`, no outside usage, an uncovered station is 404); the deck filters the station rows again; tokens and wall displays get 403. A conversation belongs to the Fleet Deck session created first.

## DL-567: B29 the deck returns the union of the station answers; the board sums into local days (design agent, 2026-10-24)

- Decision: the deck fans out with a 15 s timeout per station and returns each station's state and answer; the board merges, groups hour rows into days of the browser's time zone, and counts a provider home that two stations report (same source key) once.

## DL-568: B29 no invented history on the analytics page (owner scope rule; design agent, 2026-10-24)

- Decision: a day before a source's first record is not a zero: the chart marks the start (`No data before <date>`) and each section says its start date; learnings computed over a part of the window say `(since <date>)`; five learnings only, each with a rule and a minimum sample.

## DL-569: B29 design system: `.fd-bars`, a one-series bar chart (design agent, 2026-10-24)

- Decision: an inline SVG bar chart with `--fuel` bars (or neutral `--dim`), a zero baseline, a top value line, three x labels, focusable bars with a readout line and a hidden value table, a dashed start line for missing history; listed in `DESIGN-SYSTEM.md` §3 and §7.
- Rejected: a chart library; stacked categorical colors.
