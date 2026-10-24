# Analytics: one page per station and one for the fleet, backfilled from the transcripts (B29)

- **Status:** Draft for the architect, revision 2 (2026-10-24), after the adversary DESIGN REVIEW round 1 (`/home/luismorales/fleetdeck-briefs/backlog2/analytics/adv-design.md`, FAIL, 5 blockers, 11 majors, 1 minor). The changes are listed in "Revision 2 (design review)" at the end. Risk: **high** (§5): a station migration, a new link method and a member visibility rule. Adversary RE-REVIEW of this revision before the build, PRE-PR REVIEW before the merge.
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
8. **Link methods:** a control request is checked by `methods::authorize` (`crates/fleetdeck-link/src/control.rs:280`, `methods.rs:79-100`) before it is sent: a method without a row in `crates/fleetdeck-link/src/methods_table.rs` (678 lines) is `unknown_method`. The rows mirror ADR-004 §9.6 "Method table v1 (Revision 2)", and `crates/fleetdeck-link/tests/method_table.rs` checks that the two agree. A control frame is at most `MAX_CONTROL_FRAME_BYTES` = 1 MiB (`crates/fleetdeck-proto/src/link.rs:17`); `encode` refuses a larger frame (`control.rs:232`).
9. **Several sessions can hold one conversation.** `store/resumes.rs:229-300` (`exclusion`) lists every session with the same `(provider, native_session_id)` and blocks only a holder that is busy: after a session ended, an import or a resume in another session takes the same conversation. `manager/usage_recount.rs:321-326` refuses a recount for this reason ("The provider record of this session can hold usage of another session or station."). The owner can also resume a Fleet Deck conversation in a terminal.
10. **Imported history is a separate permission.** `ShareGrant.history` (`crates/fleetdeck-deck/src/store/members.rs:61`, DL-189); `http/history_routes.rs:135-143` answers 403 `history_not_shared` to a member without it.
11. **Trust on other systems:** `trusted_path` returns `Unverifiable("fs_unsupported")` outside Linux (`history/trust.rs:187-197`), and on Linux for a filesystem or mount that it cannot verify. The browse reads such a home anyway, from its canonical path, marked not verified (`history/browse.rs:110-150`, `resolve_plan`); only `Untrusted` makes a home unavailable.
12. **Codex parent threads** come in two encodings: `session_meta.payload.source.subagent.thread_spawn.parent_thread_id` (t3code `codexUsage.ts` select fields) and `session_meta.payload.parent_thread_id` (`history/trust.rs:494-502`, `history/import_check.rs:213`). A sub-agent can start a sub-agent: a chain has more than one level.
13. **The workflow projection is folded and pruned:** `workflow_state::observe` inserts a `workflow_runs` row at the first update (`store/workflow_state.rs:63-90`) and keeps at most 32 ended runs per session (`:251`).
14. **Prune:** `prune_history` runs 60 s after the start, then every hour (`serve.rs:84-112`); `prune` deletes every acknowledged event older than the window (`store/log.rs:353-405`, the `DELETE` at `:388`). The store runs every query on one database thread (`store/mod.rs:325`, "Clones share one database thread").
15. **Settings knobs:** `[retention]` and the field catalog (`crates/fleetdeck-core/src/settings/fields_knobs.rs:219-240`, `K::Bool` exists, `fields.rs:90`).
16. **Board:** pages are `Route` names (`web/src/app/routes.ts:48-110`), rendered in `App.tsx` (790 lines, at the limit), linked from `Sidebar.tsx:470-500`; a member does not open the routes in `GUEST_HIDDEN_ROUTES` (`web/src/sharing/member.ts:107-120`). Design system: `--fuel` is the one color of usage and cost (`docs/v2/DESIGN-SYSTEM.md:94`), `.fd-meter` (`:207`), the one data graphic so far is the lane sparkline (`:242`). There is no chart primitive.

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

The station keeps the transcript numbers after the CLI deletes a transcript (§3.3): from the first scan on, the Claude history grows beyond 30 days. There is no age limit on the ingestion: every record that a transcript on disk still holds is counted (revision 2; the Codex history on Lighting is 200 days deep). Nothing older than a source is shown or estimated; each section says its start date.

A usage row belongs to a Fleet Deck session only inside a proven execution of that session (§3.6a): from the retained events on (2026-10-17 on Lighting), not for older transcript records. The owner sees every row; the older rows count as "not in a Fleet Deck run".

**Q2. Where to compute?** The station computes hour buckets per conversation (transcripts) and per session (Fleet Deck counters) from its own rows. The deck asks each station in pages of at most 960 KiB (§3.2), applies the visibility rule and returns the union with the station tag. The board sums the rows into days of the viewer's time zone. Raw events never leave the station for this page.

**Q3. Learnings.** Five learnings, each with a rule, a minimum sample and its source (§3.10). Rejected: the cost per merged change (no durable land count per period), the average turns to finish (no definition of "finished" that the data holds), the time to first answer (not recorded).

**Q4. Privacy.** The owner sees everything, including usage outside Fleet Deck (terminal sessions, other apps). A member sees only the usage rows that the station proved to come from a run of a session that the share covers (§3.6a), and the counters of those sessions; never the usage outside a proven run (terminal work, the imported earlier history of a session, a conversation that two sessions held at one time), and none of the station-wide source facts (file counts, bytes, reasons, history dates) (§3.7 step 7). No file path, no working folder, no prompt, no tool input and no tool name of an MCP server leaves the station; only numbers, model names, the eight `ToolKind` names, session ids and hours (§4).

**Q5. Cost.** Every cost is "Cost estimate", at API list prices from the Fleet Deck price table, with its date (`Prices of 2026-10-24`). A model without a price counts its tokens and no cost ("no price"), never zero.

**Q6. A test per metric.** Every metric has a fixture test with a known value (§6, the tests of each task).

## 3. Design

### 3.1 Overview

```
provider homes ──scan (every 10 min, incremental)──► station.db: analytics_usage (hour buckets, attributed per record),
                                                      analytics_seen (dedupe keys), analytics_files (offsets), analytics_threads (Codex ancestry)
station events ──one ordered cursor (inline when caught up)──► station.db: analytics_activity (hour × session counters),
                                                      analytics_exec (proven execution intervals), analytics_workflows
sessions, turn_checkpoints, shell_runs ──read at query time
                                    │
                station.analytics (link, feature analytics.v1, pages ≤ 960 KiB)
                                    │
deck GET /api/analytics ── fan out, page loop, visibility filter, union ──► board #/analytics, #/analytics/<station>
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
pub enum StationAnalyticsState { Ok, Offline, Unsupported, Busy, Timeout, Error }

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
    pub const KEPT_KEYS: &[&str] = &[SESSIONS_CREATED, TURNS_RECORDED, SHELL_COMMANDS, SHELL_FAILED];
}

impl AnalyticsQuery {
    /// `invalid_params` text, or `None` when the query is valid.
    pub fn check(&self) -> Option<&'static str>;
    // since >= 0, since % HOUR_MS == 0, since < until ("the window is not valid");
    // before: since < before, before % HOUR_MS == 0, before <= until rounded up to the hour
    //   ("the page cursor is not valid");
    // sessions.len() <= MAX_QUERY_SESSIONS ("too many sessions"); every id passes the session id rule.
}
```

Feature: `crates/fleetdeck-proto/src/version.rs`, after `NATIVE_SESSIONS` (`:84`):

```rust
    /// `station.analytics`: the hour buckets of the transcript usage and the
    /// Fleet Deck counters of a station (B29).
    pub const ANALYTICS: &str = "analytics.v1";
```

and `ANALYTICS` in `ALL` after `NATIVE_SESSIONS` (`:327`). Generated TypeScript in `web/src/gen/analytics/` (`make ts-types`).

**Link method row** (revision 2; without it every call is `unknown_method`, §1.4 item 8). `crates/fleetdeck-link/src/methods_table.rs`, after `station.native_preview` (`:316-322`):

```rust
    // The analytics of a station: usage hour buckets and Fleet Deck counters (B29).
    m(
        "station.analytics",
        DeckToStation,
        Scope::Read,
        Some(feature::ANALYTICS),
        false,
    ),
```

ADR-004: a row in the feature table after `native_sessions.v1` (`:122`): ``| `analytics.v1` | `station.analytics` (B29; the usage hour buckets of the station's provider transcripts and the Fleet Deck counters, in pages) |``; a row in §9.6 "Method table v1 (Revision 2)" after `station.native_preview` (`:449`): ``| `station.analytics` | deck → station | read | `analytics.v1` | no |``. `crates/fleetdeck-link/tests/method_table.rs` checks that the code and the ADR agree; it needs no change.

### 3.3 Station storage (migration V50, `crates/fleetdeck-station/src/store/schema_v50.rs`)

V50 is the number reserved for this track; the merge renumbers if needed. Additive, forward only. Reverse: `DROP TABLE analytics_meta; DROP TABLE analytics_workflows; DROP TABLE analytics_exec; DROP TABLE analytics_open; DROP TABLE analytics_activity; DROP TABLE analytics_seen; DROP TABLE analytics_usage; DROP TABLE analytics_threads; DROP TABLE analytics_files;`.

```sql
-- B29: one row per transcript file that the analytics scan read.
--   home      the source key (AnalyticsSource.key)
--   rel_path  the path below the home; never sent on the wire
--   mtime_ns  st_mtime of the file in ns at the last read
--   offset    bytes read: the end of the last whole line
--   guard     hex SHA-256 of the 64 bytes before offset ('' at offset 0)
--   state     the JSON parser state at offset (Codex; '{}' for Claude)
--   seen_size the file size at the last read
CREATE TABLE analytics_files (
    home        TEXT    NOT NULL,
    rel_path    TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    dev         INTEGER NOT NULL,
    ino         INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    offset      INTEGER NOT NULL,
    guard       TEXT    NOT NULL,
    state       TEXT    NOT NULL,
    seen_size   INTEGER NOT NULL,
    malformed   INTEGER NOT NULL DEFAULT 0,
    scanned_at  INTEGER NOT NULL,
    PRIMARY KEY (home, rel_path)
) STRICT;

-- B29: the Codex thread ancestry, from the first session_meta of each
-- rollout. parent NULL: a root thread. Kept forever.
CREATE TABLE analytics_threads (
    home   TEXT NOT NULL,
    thread TEXT NOT NULL,
    parent TEXT,
    PRIMARY KEY (home, thread)
) STRICT, WITHOUT ROWID;

-- B29: the transcript usage per UTC hour, home, provider, model,
-- conversation (Claude sessionId, Codex root thread id) and attribution
-- (session_id: the Fleet Deck session of the proven run, '' for none;
-- decided per record at ingestion, §3.6a). Kept forever.
CREATE TABLE analytics_usage (
    hour         INTEGER NOT NULL,
    home         TEXT    NOT NULL,
    provider     TEXT    NOT NULL,
    model        TEXT    NOT NULL,
    conversation TEXT    NOT NULL,
    session_id   TEXT    NOT NULL,
    requests     INTEGER NOT NULL,
    input        INTEGER NOT NULL,
    cache_read   INTEGER NOT NULL,
    cache_write  INTEGER NOT NULL,
    output       INTEGER NOT NULL,
    reasoning    INTEGER NOT NULL,
    PRIMARY KEY (hour, home, provider, model, conversation, session_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX analytics_usage_session ON analytics_usage(session_id, hour);

-- B29: the dedupe keys of the counted records: the first 8 bytes of the
-- SHA-256 of the key text, as a signed integer. Kept forever (revision 2).
CREATE TABLE analytics_seen (
    key INTEGER PRIMARY KEY
) STRICT;

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

-- B29: the proven execution intervals of the Fleet Deck sessions (§3.6a):
-- from a session.started to the end of that process. ended_at NULL: open.
-- Kept forever.
CREATE TABLE analytics_exec (
    session_id TEXT    NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    PRIMARY KEY (session_id, started_at)
) STRICT, WITHOUT ROWID;

-- B29: the workflows that the counters saw, owned by analytics (not the
-- workflow_runs projection). Kept forever.
CREATE TABLE analytics_workflows (
    session_id TEXT NOT NULL,
    task_id    TEXT NOT NULL,
    PRIMARY KEY (session_id, task_id)
) STRICT, WITHOUT ROWID;

-- B29: the one ordered event cursor (§3.6). Every event with
-- station_seq < next_seq is counted, exactly once, in station_seq order.
CREATE TABLE analytics_meta (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    events_since INTEGER,
    next_seq     INTEGER NOT NULL
) STRICT;
INSERT INTO analytics_meta (id, events_since, next_seq)
SELECT 1, (SELECT min(at) FROM events),
       COALESCE((SELECT min(station_seq) FROM events),
                (SELECT station_seq_next FROM meta WHERE id = 1));
```

Size: on Lighting, 106 k distinct records in 25 days give about 1.5 M `analytics_seen` rows a year (an integer primary key only: about 12 bytes each, about 20 MB a year on the busiest station). The keys are kept forever, because the ingestion has no age limit (revision 2): a fork or an archive copy of a record of any age must still find its key. `analytics_usage` had 780 rows for 25 days (more with the attribution column at run edges, at most twice). `analytics_exec` has one row per process start (726 runs in 7 days on Lighting is an upper bound).

**The prune fence** (revision 2). The history prune (`log.rs:353-405`, `prune`) adds one rule in its transaction, after `through` is computed (`:381`) and before the `DELETE` (`:388`): `through = through.min(next_seq - 1)` with `next_seq` from `analytics_meta`. An event that the analytics cursor did not count yet is never deleted. When the cursor is caught up (`next_seq = head + 1`, the normal state), the fence changes nothing. Nothing of `analytics_*` is pruned.

### 3.4 The transcript parsers (`crates/fleetdeck-station/src/analytics/`, new module, `pub mod analytics;` in `lib.rs`)

`analytics/mod.rs` declares `pub mod claude; pub mod codex; pub mod scan; pub mod query; mod walk;` and:

```rust
/// One counted API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub at: i64,
    pub model: String,
    /// Claude `sessionId`; Codex: the thread of the file (the store maps it
    /// to the root thread, §3.5 step 4).
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

**Codex** (`analytics/codex.rs`): `pub fn parse_line(line: &[u8], state: &mut CodexState) -> Parsed` and `pub fn parse_meta(line: &[u8]) -> Option<ThreadMeta>`. The state is saved with the offset (`analytics_files.state`, JSON).

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexState {
    /// The model of the last `turn_context`.
    pub model: String,
    /// The id of the first `session_meta` (this file's thread).
    pub thread: String,
    pub seen_meta: bool,
    /// The fork or sub-agent copy burst: copies set the baseline only.
    pub copying: bool,
    pub copy_anchor_ms: i64,
    /// The last cumulative totals of the thread.
    pub baseline: Option<Totals>,
    /// The counter epoch: the time of the first `session_meta` (else of the
    /// first usage line), and the time of the usage line of each restart
    /// (rules 5.4 and 5.7). Replay-stable: a read of the same bytes from 0
    /// gives the same epochs.
    pub epoch_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals { pub input: u64, pub cached: u64, pub cache_write: u64, pub output: u64, pub reasoning: u64 }

/// The ancestry facts of the first `session_meta` of a rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMeta { pub thread: String, pub parent: Option<String> }
```

1. Gate: only lines that contain one of `"token_count"`, `"token_usage_record"`, `"turn_context"`, `"session_meta"`.
2. `session_meta`: only the first one (`seen_meta`) sets `thread` (`payload.id`, else `payload.session_id`) and `epoch_ms` (its time, when it parses). The parent is one of the two encodings (§1.4 item 12), in this order: `payload.source.subagent.thread_spawn.parent_thread_id`, then `payload.parent_thread_id`; a value that is not a non-empty string is no parent. When `payload.forked_from_id` is a string or a parent is set, and the record time parses: `copying = true`, `copy_anchor_ms = time`. `Nothing`. `parse_meta` applies the same field rules to one line and returns `ThreadMeta` (the scan, §3.5 step 3, calls it on the first record of each new rollout).
3. `turn_context` with a string `payload.model`: `model = it`. `Nothing`.
4. A usage line gives `(time, cumulative C, request L, response id R)`:
   - `event_msg` with `payload.type == "token_count"`: `C = payload.info.total_token_usage`, `L = payload.info.last_token_usage`, `R = None`; `info` null or either object missing: `Nothing`.
   - `token_usage_record`: `C = payload.thread_token_usage`, `L = payload.usage`, `R = payload.response_id` when it is a non-empty string of at most 128 bytes.
   - A totals object needs `input_tokens` and `output_tokens`; the other keys default to 0. Missing: `Malformed`. A time that does not parse: `Malformed`. When `epoch_ms` is still 0: `epoch_ms = time`.
5. Rules, in order (from t3code `codexUsage.ts:198-268`; revision 2 adds the restart check of `:245` before any subtraction; DL-562):
   1. `copying` and `time - copy_anchor_ms < 1000`: `copy_anchor_ms = time`, `baseline = Some(C)`, `Nothing`. Else `copying = false`.
   2. `baseline == Some(C)`: `Nothing` (the same request reported twice: a `token_count` and its `token_usage_record`, or a replay).
   3. `L.input_tokens + L.output_tokens == 0`: `baseline = Some(C)`, `Nothing` (a context-size report).
   4. **Restart:** every field of `C` equals the same field of `L`, and `baseline` is `Some` and not zero: a new process restarted its counters at this request. The count is `L`; `epoch_ms = time`. (Example: baseline (100, 10), then `C = L = (500, 50)`: the count is (500, 50), not (400, 40).)
   5. `baseline` is `None`: the count is `L` when every field of `C` is at least the field of `L` (the file continues an earlier history, only this request is new); else `Malformed` (and `baseline = Some(C)`).
   6. Every field of `C` is at least the field of `baseline`: the count is `C - baseline` field by field (equal to `L` when the reports reconcile; larger when a report is missing from the file, so nothing is lost).
   7. Else (a field of `C` is below the baseline, and rule 4 did not apply): a restart whose first report already holds more than one request. The count is `L`; `epoch_ms = time`.
   8. `baseline = Some(C)`. A count whose four token parts are all 0: `Nothing`.
6. Record: `input = count.input - count.cached - count.cache_write` (saturating; Codex input includes the cache), `cache_read = count.cached`, `cache_write = count.cache_write`, `output = count.output`, `reasoning = count.reasoning`; `model` = `state.model`, `"unknown"` when empty; `conversation = state.thread` (`"unknown"` when empty; the store maps it to its root thread, §3.5 step 4); key (revision 2):
   - `R` is `Some`: `codex:resp:<R>`.
   - Else: `codex:<thread>:<epoch_ms>:<C.input>:<C.output>`. The epoch separates the counter runs of one thread: cumulative (100, 10), (300, 30), a restart, (100, 10) give three different keys. The same rollout under `sessions/` and `archived_sessions/` replays the same epochs and counts once.
   - A `token_count` and its `token_usage_record` are one request: the first of the two in the file counts (with its own key) and the second meets rule 5.2. A copy of the file has the same order and the same keys.

### 3.5 The scan (`analytics/walk.rs`, `analytics/scan.rs`)

1. **Homes and trust** (revision 2: one portable policy, the one of the browse). The homes of `plan_homes(policy)`, each through `resolve_plan(plan, uid, &Mounts::load())` (both in `history/browse.rs`, with `HomePlan` and `Resolved`, made `pub(crate)`; no other change there). The result maps to the source:
   - `Resolved { state: Ok, exists: false, .. }`: `Missing`.
   - `Resolved { state: Unavailable { reason }, .. }` (`home_not_trusted` for `PathCheck::Untrusted`, `home_not_absolute`): `Unavailable { reason }`; nothing is read.
   - `Resolved { state: Ok, exists: true, verified, canon }`: read from `canon`; `AnalyticsSource.verified = verified`. `verified` is false for `PathCheck::Unverifiable` (every home on macOS, `fs_unsupported`; on Linux a filesystem or mount that the check does not know). Such a home is read with the same file rules as a verified one (no link followed, regular files of the station user only, step 3).
   - The source key is §3.2 `AnalyticsSource.key` over `canon`. macOS has no `/etc/machine-id`: the key hashes an empty machine id and the path (two stations on one Mac with one home still match).
2. **Walk** (`walk.rs`, a new walker with the primitives of `history/browse_preview.rs`: `descend`, `statat` with `SYMLINK_NOFOLLOW`, regular files only, links never followed; POSIX calls only, so it builds and runs on Linux and macOS). Patterns:
   - Claude: `projects/*/*.jsonl`, `projects/*/*/subagents/*.jsonl`, `projects/*/*/subagents/workflows/*/*.jsonl` without the name `journal.jsonl`.
   - Codex: `sessions/*/*/*/rollout-*.jsonl`, `archived_sessions/rollout-*.jsonl`.
   - Budget per pass: 20,000 directories, 100,000 files; over it, the pass stops the walk, logs `analytics walk truncated` and counts the home as `Unavailable { reason: "too_many_files" }` for this pass (`complete = false`).
3. **Codex ancestry first** (revision 2). Before any usage is read in a pass, for each Codex file of the walk without a row in `analytics_threads` for its thread: read its first record (one `Lines` call, `line_max` 16 MiB, the same open rules as step 4) and `parse_meta`; `INSERT OR IGNORE INTO analytics_threads(home, thread, parent)`. So every root is known before step 4 uses it, whatever the file order.
4. **Read** each candidate. A file is a candidate (revision 2) when it has no row, or when any of these differs from its row: `dev`, `ino`, `size` against `seen_size`, `mtime_ns`; or when `offset < size` (a file that an earlier pass left unfinished is always read again).
   - Open with `O_NOFOLLOW`; `fstat`: a regular file of the station user, else skip it (no row).
   - Resume at `offset` only when all hold: the same `dev` and `ino`; `size >= offset`; the SHA-256 of the 64 bytes before `offset` equals `guard`; and not (`mtime_ns` changed and `size == seen_size`) (a rewrite of the same size). Else start at 0 with a fresh state (the dedupe keys make a second read harmless).
   - Read whole lines with `history::lines::Lines` (`line_max` 16 MiB, a longer line counts as malformed); a last line without its newline stays for the next pass.
   - No age limit (revision 2): every record counts, however old.
   - **Conversation root (Codex):** `root(thread)` follows `analytics_threads.parent` from the file's thread: at most 16 steps; a parent without its own row is the root (its file is gone or was never on this station, but its id is known); a thread seen twice on the path (a cycle) or a 17th step: the conversation is the file's own thread, and the scan logs `analytics thread ancestry broken` once per file (no id in the log). Claude: the conversation is the record's `sessionId` (nested files carry the parent's, §1.1 item 6).
   - **Attribution** of each record (§3.6a) at its own time `at`: the sessions `S` with `sessions.provider = <provider>` and `sessions.native_session_id = <conversation>` that have an `analytics_exec` row with `started_at <= at` and (`ended_at IS NULL` or `at <= ended_at`). Exactly one such session: `session_id = it`. None, or more than one: `session_id = ''`. The lookup is cached per file and conversation for the transaction.
   - **One transaction per file** (station store, `store/analytics.rs`): for each record, `INSERT OR IGNORE INTO analytics_seen(key)`; when it inserted a row (or the record has no key), upsert the hour bucket `(hour, home, provider, model, conversation, session_id)` with `hour = at - at.rem_euclid(3_600_000)`; then update the file row (offset, guard, state, sizes, `mtime_ns`, counters, `scanned_at`). The offset and the counts commit together.
5. **Schedule** (`scan.rs`, `pub async fn run(manager: SessionManager, enabled: bool, shutdown: watch::Receiver<bool>)`, spawned in `serve.rs` next to the prune at `:406`). At the start, the task first drives the event cursor until it is caught up (§3.6, `catch_up_step` until it returns `false`; no wait before it, so it runs before the first prune at 60 s; the prune fence of §3.3 protects the events anyway; a failed step logs `analytics catch-up failed` with the error kind and is retried after 30 s). The first scan pass follows at once after that (an attribution needs the execution intervals of the retained events), then one every 10 min. A pass reads at most 2 GiB or runs at most 5 min; when it stops with bytes left, the next pass comes after 30 s (catch-up). The reading runs on one blocking thread (`spawn_blocking`); the store writes go through the store as usual. A pass that fails logs `analytics scan failed` with the error kind (never a path) and the loop goes on. The `Refresh` of the board does not start a pass (it reads the store).
6. **Off:** with `analytics.transcripts = false` the task does not read; the stored rows stay; every source has `state: off`. The event cursor runs whatever the knob says.
7. `pending_bytes` = sum over the rows of the last walk of `max(0, size on disk - offset)`, plus the size of files without a row; `complete` = `pending_bytes == 0` and the last walk was not truncated; kept in memory by the scanner (a `Mutex` in a shared `ScanStatus`, read by the query) with `scanned_at` and the per-home state. Before the first pass ends: `complete = false`.
8. `keeps_days` (Claude): `cleanupPeriodDays` of `<home>/settings.json` when it is an integer 1 to 36,500, else 30. Read at each pass; a read error gives 30.

### 3.6 The Fleet Deck counters (`crates/fleetdeck-station/src/store/analytics_observe.rs`)

**One ordered cursor** (revision 2). Every event is counted once, in `station_seq` order, by one function `observe_one(conn, envelope)`. Two callers drive it, and the store's one database thread (§1.4 item 14) serializes them:

- `pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope) -> Result<()>`, called in `append_at` after `workflow_state::observe` (`log.rs:193`; the order does not matter any more, see `workflow.updated`). It reads `analytics_meta.next_seq`. When `envelope.station_seq == next_seq` (the cursor is caught up): `observe_one`, then `next_seq += 1`, in the event transaction. Else (the cursor is behind): nothing; `catch_up_step` reaches this event later, in order.
- `pub fn catch_up_step(conn: &mut Connection, limit: u32) -> Result<bool>`: in one transaction, read the events with `station_seq >= next_seq` in `station_seq` order, at most `limit` (2,000), `observe_one` each, set `next_seq` past the last one. Returns `true` while events are left. The scanner task (§3.5 step 5) runs it until `false` at the start (on Lighting about 77,000 events, 39 steps). After that, every new event is counted inline by `observe`, because the next append has `station_seq == next_seq`.

So a historical `turn.started` is always applied before its live `turn.completed`, and `analytics_open` never holds a row of an event that is already past. `observe_one` never fails on the content of an event (a payload that does not decode counts nothing); only a database error stops it, and that error also stops the append. `hour` is the event `at` floored to the hour. `add(metric, n)` upserts `analytics_activity(hour, session, metric) += n`. When `analytics_meta.events_since` is NULL (a station with no event at V50), the first `observe_one` sets it to the event `at`.

| Event | Counts |
| --- | --- |
| `session.started` | `resumed == Some(true)`: `resumes`, else `starts`; execution interval: close every open `analytics_exec` row of this session at `at`; close every open row of the other sessions that hold the same `(provider, native_session_id)` (read from `sessions`) at `at`; insert `analytics_exec(session, at, NULL)` |
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
| `workflow.updated` | `INSERT OR IGNORE INTO analytics_workflows(session_id, task_id)`; `workflows += 1` only when it inserted a row (revision 2: analytics-owned identities, idempotent, independent of `workflow_runs`) |
| `run.completed`, `session.ended`, `session.state` to `exited` or `lost` | delete the session's open `tool` rows (a run that ended runs no tool) |
| `session.ended`, `session.state` to `exited` or `lost` | delete all the session's open rows; close the session's open `analytics_exec` row at `at` |

Every other kind: nothing (a `match` on the kind name first, so `message.delta` costs no JSON work).

**Start of the station.** After the first `catch_up_step` returns `false`, the scanner task closes the open `analytics_exec` rows of every session whose summary state is `exited` or `lost`, at the `at` of that session's last stored event (else at `started_at`). A station that stopped without the end event of a session so does not keep that interval open.

A turn or tool that started before the first stored event has no open row: its outcome counts, its duration does not. A workflow whose first update is older than the first stored event counts once at its first retained update (a fact of the retained window; `events_since` says where it starts).

### 3.6a Attribution of usage to a session (revision 2)

A usage record belongs to a Fleet Deck session only when the station can prove that a process of that session made it:

1. **Proof:** the record's time is inside an `analytics_exec` interval of a session that holds the record's conversation (`sessions.native_session_id`, Codex root thread), and inside no interval of another holder (§3.5 step 4). An interval runs from the `session.started` of a process to its end event (or to the next start of the same session, or to a start of another holder of the conversation).
2. **What is not proven, and so has `session_id = ''`:**
   - usage outside Fleet Deck (no holder);
   - the earlier history of an imported session: the records before the import are before the first `session.started` of the importing session, so in none of its intervals (the `history` permission of DL-189 has nothing to show; see 3 below);
   - a conversation of session A that the owner continued in a terminal or in session B after A ended: outside A's intervals (B's own runs are proven for B);
   - records older than the retained events at V50 (no interval exists for them);
   - two holders with intervals at one time (an ambiguous record).
3. **Who sees what:** the owner sees every row; a row with `''` counts in the owner's totals and in the line "Not in a Fleet Deck run". A member sees only rows with a session that the share covers. The deck sends a member's query with `Audience::Member` whatever `grant.history` says: analytics never shows a member the earlier history of an imported session, so a share with `history: false` is honored and a share with `history: true` loses nothing that the history view shows (the history view shows text, analytics would show only its counts).
4. **Granularity:** the attribution is per record, at ingestion, before the hour bucket is summed; an hour that crosses an interval edge splits into two buckets (`session_id` is in the bucket key). No bucket mixes a proven and an unproven record.
5. **The session's conversation id** is the current `sessions.native_session_id`. A session whose conversation id changed (a provider that forks on resume) keeps the proof for its current id only; the records of the earlier id are `''` (the safe side: a member sees less, never more).
6. **Residual risk:** the owner continues the conversation of a shared session in a terminal **while** the Fleet Deck process of that session still runs. The terminal records fall into the open interval and count for the shared session. Fleet Deck cannot tell the two processes apart in the transcript (one conversation, one file). The owner caused it, on a conversation that the owner shared; §4 item 3 says so.

### 3.7 The query (`crates/fleetdeck-station/src/analytics/query.rs`, `store/analytics.rs`)

`pub async fn answer(manager: &SessionManager, status: &ScanStatus, query: AnalyticsQuery) -> Result<AnalyticsAnswer, HistoryError>`:

1. `query.check()`; a refusal is `invalid_params` with the text.
2. One query at a time per station (a `Semaphore(1)` in `ScanStatus`, wait 2 s, else `busy` "another analytics request is running").
3. **Window of the page:** hours `h` with `since <= h < top`, `top = before` (a later page) or `until` (the first page).
4. **Usage rows:** `analytics_usage` in the window. `Audience::Owner`: every row (`session_id = ''` gives `None`). `Audience::Member`: only rows with `session_id <> ''` and, when `query.sessions` is `Some`, `session_id` in it. `conversation` = the §3.2 hash of the stored conversation; `source` = the index of `home` in `sources`; `cost_usd` = `estimate_cost(provider, model, input, cache_read, cache_write, output)` (§3.8).
5. **Activity rows:** `analytics_activity` in the window, plus the kept tables grouped by hour and session: `sessions.created` (`sessions.created_at`), `turns.recorded` (`turn_checkpoints` with `state IN ('finalized', 'abandoned')`, `created_at`), `shell.commands` and `shell.failed` (`shell_runs.created_at`; failed: `exit_code IS NOT NULL AND exit_code <> 0`). Filtered by `query.sessions` when `Some`.
6. **Paging by bytes** (revision 2): the rows are taken by hour, newest hour first; all the usage and activity rows of an hour go into the page or none of them. The size is counted exactly as `serde_json` writes it: the answer without rows (`sources`, `coverage`, the scalars, empty arrays, `next_before` set to the largest value it can have) plus, per row, `serde_json::to_vec(row).len() + 1` (the comma). An hour that would make the total larger than `MAX_PAGE_BYTES` ends the page: `next_before = Some(<the oldest hour in the page>)`. When hours older than the page remain in the window but the first hour alone is larger than `MAX_PAGE_BYTES`: the page holds the rows of that hour that fit (usage rows by tokens, largest first, then activity rows by session id), `partial_hour = Some(hour)`, and `next_before` = that hour. No hour left after the page: `next_before = None`. The final page is serialized once and checked against `MAX_PAGE_BYTES` (a failed check is `internal`, never a frame that the link refuses).
7. **Sources and coverage** (on every page, small):
   - Owner form: `sources` from `ScanStatus` and `analytics_files` (`files` = rows of the home, `malformed` sum, `pending_bytes`, `scanned_at`, `keeps_days`, the reason word) and `first_at` = `min(hour)` of the home in `analytics_usage`; `coverage.events_since` = `analytics_meta.events_since`; `coverage.kept_since` = `min(sessions.created_at)`.
   - Member form (revision 2): per source only `provider`, `key`, the state (an `Unavailable` reason becomes `unavailable`), `verified`, `complete`, and `first_at = max(<owner first_at>, coverage.events_since)` (`None` when either is `None`); `scanned_at`, `files`, `pending_bytes`, `malformed`, `keeps_days` are `None`. `coverage.events_since` as the owner (a station-wide start date of the counters, no session fact); `coverage.kept_since` = `min(created_at)` of the listed sessions (all sessions when `sessions` is `None`).

Link: `crates/fleetdeck-station/src/link/intake_analytics.rs` (`pub(super) async fn analytics(ctx, params) -> Outcome`, in the style of `intake_native.rs`), dispatched at `link/intake.rs:514` as `"station.analytics" => intake_analytics::analytics(ctx, req.params).await,`. The method row of §3.2 lets the request through `methods::authorize`. Error codes: `invalid_params`, `busy`, `internal`. The station checks the params again; it trusts the deck for `sessions` and `audience` (the deck is the authority on members, as for every member read).

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

1. Methods table (`methods_table_rest.rs`, after `stations.native_preview` at `:581`): `m("analytics.read", Method::GET, "/api/analytics", Some(Scope::Read), false).member(),` with the comment `// The analytics page (B29): the owner and members; a member gets the rows of proven runs of the covered sessions only.` Route in `http/mod.rs` next to `:605`.
2. Principal: `Owner` → every station (or the named one), `sessions: None`, `audience: Owner`, `owner: true`. `Member` with `ShareScope::Board` → `sessions: None`, `audience: Member`. `Member` with `ShareScope::Sessions(set)` → only the stations in the set, each with `sessions: Some(<its ids>)`, `audience: Member`. `grant.history` does not change the query: a member answer never holds the earlier history of an imported session (§3.6a item 3). `Token` and `Display` → 403 `owner_only` ("Only a board login reads analytics.").
3. Params: `since` and `until` integers with the rule of `AnalyticsQuery::check` (400 `invalid_params` with its text; `since` must be an hour start); `station` a plain name (`plain_name` rule of `native_routes.rs`); an unknown name: 404 `unknown_station`; a member asking a station with no covered session: 404 `unknown_station` (no hint that it exists).
4. Fan out to the chosen stations at once (`futures::future::join_all`), each with the `native_routes.rs::read` steps: `link_feature(…, feature::ANALYTICS)` → `Down`: `offline`, `Without`: `unsupported`. **Page loop** (revision 2): call `station.analytics` with `before: None`, then with `before = next_before` while it is `Some`, at most `MAX_PAGES` calls; each call has a 15 s timeout and the whole station 60 s. The pages merge into one `AnalyticsAnswer`: the rows appended, `sources` and `coverage` of the first page, `partial_hour` of each page into `partial_hours`. A 16th page with `next_before` still `Some`: `truncated_before = next_before`. A timeout of any page → `timeout` (the pages before it are dropped: a station is all or nothing); station `busy` → `busy`; another error or an answer that does not parse → `error` (a warning without the text). One request per principal at a time: a deck-wide `Semaphore(4)`; when none is free in 2 s, 503 `busy` "The deck is busy. Try again in a moment.".
5. **Defense in depth:** for a member, the deck drops every `UsageHour` without a `session_id` or with a session that `grant.scope.covers` does not cover, and every such `ActivityHour`; it sets every owner-form field of each source to `None` and an `Unavailable` reason to `unavailable` (§3.7 step 7), before it answers.
6. Stations in the answer: by name. Nothing is stored or cached. A failure writes an audit row as `native_routes.rs::audit` (action `analytics.read`, the code only); a success writes none.

### 3.10 Board

Files under `web/src/analytics/` (new): `api.ts` (fetch), `merge.ts` (sums and local days), `learnings.ts`, `AnalyticsPage.tsx`, `Bars.tsx`, `analytics.css`.

**Routes** (`routes.ts`): `{ name: 'analytics'; station: string | null }`, `#/analytics` (fleet) and `#/analytics/<station name>` (URI-encoded). Sidebar: a nav row "Analytics" (Lucide `chart-column`) after Activity, for the owner and for members (not in `GUEST_HIDDEN_ROUTES`). Layout: in `BAR_ROUTES` and `MORE_TAB`; `PHONE_TITLES.analytics = 'Analytics'`; a More row on the phone. `App.tsx` (790 lines): the case is one line, `case 'analytics': return <AnalyticsPage route={route} stations={stations} sessions={sessions} />;` (any extra line moves the `case 'more'` body into `app/MoreRoute.tsx`).

**Merge** (`merge.ts`, pure functions, tested):

```ts
export type Period = 1 | 7 | 30 | 90 | 365 | 'all'; // days; 1 = the past 24 hours; 'all' = since = 0
export type Totals = {
  readonly input: number; readonly cacheRead: number; readonly cacheWrite: number;
  readonly output: number; readonly reasoning: number; readonly requests: number;
  /** Sum of the priced buckets. */ readonly cost: number;
  /** Tokens of buckets without a price. */ readonly unpricedTokens: number;
  readonly unpricedModels: readonly string[];
};
export type Day = { readonly key: string /* YYYY-MM-DD local */; readonly start: number; readonly totals: Totals };
export type Merged = {
  readonly total: Totals;
  /** Rows without a session (not in a proven Fleet Deck run); null for a member. */
  readonly notInRun: Totals | null;
  readonly days: readonly Day[]; // every local day of the window, oldest first
  readonly models: readonly { provider: string; model: string; totals: Totals }[]; // by tokens, largest first
  readonly stations: readonly { id: string; name: string; state: string; totals: Totals | null;
    pending: number | null; complete: boolean; truncatedBefore: number | null; partialHours: readonly number[] }[];
  /** The earliest `first_at` of the counted sources (member form: already clipped to
   *  `events_since` by the station); days before its local day have no data. */
  readonly usageSince: number | null;
  /** Per session: turns that ended and the attributed usage, for `cost_per_turn` (§3.12). */
  readonly perSession: ReadonlyMap<string, { readonly turnsEnded: number; readonly totals: Totals }>;
  /** Every counted source is `ok` and `complete`. */
  readonly usageComplete: boolean;
  readonly counts: ReadonlyMap<string, number>; // metric key → sum
  readonly eventsSince: number | null; // the latest `events_since` of the stations that answered
  readonly keptSince: number | null; // the earliest `kept_since`
  readonly priceTables: readonly string[];
  readonly duplicateSources: readonly string[]; // "Claude on thunder (same home as Lighting)"
  readonly window: { readonly since: number; readonly until: number };
};
export function merge(answer: FleetAnalytics, since: number, until: number, zone: string): Merged;
```

Rules: tokens = `input + cacheRead + cacheWrite + output` (reasoning is a part of output, never added). Days are local days of the browser's zone (`Intl.DateTimeFormat(undefined).resolvedOptions().timeZone`) through `Intl`; an hour row counts on the local day of its start. Only stations with `state: ok` count.

**One home read by two stations** (revision 2). Rows of sources with one key on two or more stations merge by bucket identity `(source key, hour, provider, model, conversation)`: of the rows with one identity, the row with the most `requests` counts and the others do not (a tie: the row with a `session_id`, then the station first by name). Each station's bucket holds only records of that bucket (each station counts a record once), so the largest is the most complete count of the bucket and never a double count; a row that only one station still holds (its transcript was deleted before the other station began) counts. The sources go to `duplicateSources`.

**The window** (revision 2, the minor finding): hour-aligned. `until = now`; `since = floor_hour(now - period days)` (the past 24 hours: `floor_hour(now - 24 h)`), `0` for `all`. The page says the exact start under the filters: `From <Mon D, HH:00> to now.` (`all`: `All the history that the stations hold.`).

**Page** (`AnalyticsPage.tsx`, `Page` with `wide`):

- Header: title `Analytics`; description `Tokens and cost from the provider transcripts of each station, and the work of the Fleet Deck sessions.`; for a member `Your shared sessions only.` Filters: station `select.fd-ghost fd-sm` (`All stations`, then the station names; it navigates), period `select.fd-ghost fd-sm` (`Past 24 hours`, `7 days`, `30 days` (default), `90 days`, `365 days`, `All time`), chart metric `.fd-seg` (`Cost`, `Tokens`; default `Cost`), Refresh icon button (`aria-label="Refresh analytics"`). The page reads once on open, on a filter change and on Refresh; no polling.
- **Totals** (stat values over one-line labels, the ops glance look): `Tokens`, `New input`, `Cache read`, `Cache write`, `Output`, `Cost estimate` (`--fuel`; `title`: `At API list prices from the Fleet Deck price table of <date>. A subscription does not bill it. Models without a price are not in it.`). Under them, when `unpricedTokens > 0`: `.fd-note` `<n> tokens of <model list> have no price and are not in the cost.` For the owner, one line `Not in a Fleet Deck run (terminal, other apps, older or imported history): <tokens> tokens · <cost>` when `notInRun` has tokens.
- **By day** (`Bars.tsx`, §3.11): one bar per local day of the window (24 hour bars for the past 24 hours), cost or tokens. Under the chart, one line per provider with data: `Claude transcripts on <station> start on <Mon D> (the CLI keeps them <n> days).` / `Codex transcripts on <station> start on <Mon D>.`; owner, while `pending_bytes > 0`: `Reading the transcripts of <station>: <pct> % done.`; member, while a source is not `complete`: `<station> is still reading its transcripts.` For a member the start line is `Usage of your shared sessions on <station> starts on <Mon D>.` (the member `first_at`).
- **By model**: a table: Model, Provider, Tokens, a `.fd-meter` of its share of the tokens, Cost estimate (or `no price`).
- **By station** (fleet page only, owner): Station, Tokens, Cost estimate, then the state text: `offline` → `Offline: not in the totals.`; `unsupported` → `Update this station to see its analytics.`; `busy` → `Busy: try again.`; `timeout` → `No answer in time.`; `error` → `The station could not answer.`; a source `off` → `Transcript reading is off (Settings → station).`
- **Fleet Deck work**: rows with a label and a count. Kept counters first, with the line `Since <keptSince date>.`: `Sessions started`, `Turns`, `Shell commands` (`, <n> failed`). Then the event counters with the line `Since <eventsSince date>. The station keeps its events 7 days; Fleet Deck counts them from then on.`: `Turns that ended` (`<done> done · <interrupted> interrupted · <failed> failed`), `Average turn` (duration, from `turns.ms / turns.timed`), `Tool calls` with one row per kind (label `Shell`, `Edit`, `Read`, `Search`, `Web`, `MCP`, `Subagent`, `Other`; the count; a neutral `.fd-meter` of its share; `<n> failed`, `<n> denied` when > 0), `Approvals` (`<asked> asked · <answered> answered · <expired> expired`), `Average wait for an answer`, `Resumes`, `Files changed`, `Lines` (`+<added> −<removed>`), `Artifacts published`, `Workflows`, `Errors`. A row whose count is 0 shows `0`; a section whose coverage starts after the window end shows `No data in this period.`
- **Learnings** (§3.12): a list; each item a sentence and a `title` with its rule. No item passes: the section is absent.
- **Sources** (a fold, closed): one line per station and home: `<station> · <Claude|Codex>: <files> files, since <date>, read <age>` (or the state text); `Prices of <date>` (all price tables when they differ: `Prices of 2026-10-24 (Lighting, dev1) and 2026-10-23 (thunder)`); `Counted once per hour and conversation: <duplicateSources>`; `Hours before <Mon D, HH:00> are not shown: the answer of <station> was too large.` when `truncatedBefore`; `Some rows of <Mon D, HH:00> are not shown.` for each of `partialHours`. Member: the fold has no file counts, no read age and no reason words (the member form).
- Empty: no station answered → the `.fd-empty` state `No station answered. Analytics needs a station that is online and up to date.`; the window has no rows → `No usage in this period.`

### 3.11 Chart and the design system

The design system has no chart (§1.4 item 16). The page needs one series over time, used also by a later station page: an expansion by the skill rule (two places or more). Added to `DESIGN-SYSTEM.md` §3 and the §7 log:

- `.fd-bars`: a bar chart of one series. An inline SVG, `width: 100%`, height 160px (phone 120px). Bars `--fuel` (usage and cost) or `--dim` with `data-tone="neutral"`; a 1px `--border-soft` zero baseline; the gap between bars 2px (1px when there are more than 90); the y scale starts at 0; one top grid line at the rounded maximum with its value in 12px `--faint` (tabular figures); x labels: the first, the middle and the last day, 12px `--faint`. A day with no data inside the coverage is a zero (no bar, the baseline shows). Days before the coverage start are not drawn as zero: they get a 1px dashed `--border` vertical line at the start and the label `No data before <Mon D>` in 12px `--faint`. Hover or keyboard focus of a bar (each bar is a focusable `<rect tabIndex=0>` with `aria-label` `<date>: <value>`): a 2px `--focus` outline and the readout line above the chart (`<date> · <value>`, 13px `--text`); without a focus the readout shows the window total. The SVG has `role="group"` and `aria-label` `<metric> by day`; a visually hidden `<table>` repeats the values. Forced colors: bars `CanvasText`, focus `Highlight`. No animation.
- The other parts use existing primitives: `.fd-meter` (shares), `.fd-note`, `.fd-empty`, `select.fd-ghost`, `.fd-seg`, tables of the settings look.

### 3.12 Learnings (`learnings.ts`)

Each learning is computed over the merged rows of the window, only from the part covered by its source (`cost_per_turn` needs the usage and the turns of one period and one set of sessions, see its row); when the coverage starts after the window start, the sentence ends with ` (since <Mon D>)`.

| Id | Shown when | Sentence | Rule (`title`) |
| --- | --- | --- | --- |
| `cache` | `requests >= 100` | `<p> % of the input came from the prompt cache.` | `Cache read tokens divided by all input tokens (new input, cache read and cache write), from the provider transcripts.` |
| `tool_failures` | a kind with `tools.<kind> >= 20` and failed share `>= 5 %`; the kind with the highest share | `<Kind> calls fail most often: <p> % of <n> failed.` | `Failed tool calls divided by all tool calls of each kind, kinds with 20 calls or more.` |
| `approval_wait` | `approvals.waited >= 5` | `An approval waited <duration> on average for its answer (<n> answered).` | `The time from the request to the answer, averaged over the answered approvals.` |
| `cost_per_turn` | all of (revision 2): `usageComplete`; the learning's period `P` = from `max(window since, every answering station's events_since, usageSince)` to the window end, and `P` is not empty; `T` = the sessions with a turn that ended in `P` (`turns.done + turns.interrupted + turns.failed > 0`); **every** session of `T` has attributed usage in `P`; no attributed usage of `T` in `P` is unpriced; the turns of `T` in `P` `>= 20`. Any check fails: the learning is not shown (no partial value) | `A turn cost about <usd> at API prices (estimate, <n> turns).` | `The cost estimate of the proven runs of the sessions with ended turns, divided by those turns, in the same period; shown only when the transcripts of every station are fully read and every such session has usage.` |
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
6. **Feature flag:** `analytics.v1`, negotiated per link like `native_sessions.v1`; the method row in `methods_table.rs` and ADR-004 §9.6 (§3.2). An older deck without the row never sends the method.
7. No change to `Usage`, `SessionSummary`, the fold or any existing event.

### 3.15 Errors and edge cases

| Case | Result |
| --- | --- |
| A transcript is rotated or rewritten in place | the guard, the inode, or (same size) the mtime differs: read from 0; the dedupe keys skip the counted records |
| A pass stops inside a file (byte or time budget) | `offset < size`: the file is a candidate in every later pass until it is read to its end |
| The CLI deletes a transcript | its row stays (counted numbers stay); the next walk does not find it; nothing is subtracted |
| A Claude fork copies messages into a new file | the same `message.id:requestId`: counted once (keys are kept forever) |
| A Codex rollout under `sessions/` and `archived_sessions/` | the same replayed epochs and keys (or the same `response_id`): counted once |
| A Codex fork or sub-agent file opens with copied history | copies within 1 s of each other set the baseline only |
| A Codex thread resumed in a new process restarts its cumulative | rule 5.4 (`C == L`) or 5.7 (a field below the baseline): the request's own usage counts, a new epoch starts |
| A Codex request is reported by `token_count` and `token_usage_record` | rule 5.2: counted once |
| A Codex grandchild sub-agent | `analytics_threads` resolves the root; the usage goes to the root's conversation |
| A Codex parent whose rollout is gone | the parent id is the root (its id is known from the child) |
| A cycle or more than 16 levels in the Codex ancestry | the conversation is the file's own thread; one log line per file |
| A line longer than 16 MiB | malformed, skipped |
| A record of any age | counted (no age limit) |
| The station restarts in the middle of a file | offset and counts commit together: no double count, no loss |
| The station restarts with events not yet counted | the prune fence keeps them; the cursor counts them first |
| Two stations read one home (same machine id and path) | the board keeps the most complete row of each bucket and says so |
| Two Fleet Deck sessions hold one conversation | each record goes to the session whose run covers its time; a time in two runs, or in none: no session (owner only) |
| A session imports a conversation | the earlier records are in none of its runs: no session (owner only), whatever `grant.history` says |
| A session ran before the retained events at V50 | no interval: its usage of that time has no session (owner only) |
| A usage record a moment before its `session.started` (a provider call at process start) | outside the interval: no session (the safe side) |
| A member's share changes | the deck reads the grant on each request (`ShareGrant` is loaded per request) |
| A station's clock is wrong | hours are on the station clock; the board does not correct them |
| A model without a price | tokens count, `cost_usd: None`, `no price` |
| An answer larger than one frame | pages of at most `MAX_PAGE_BYTES`; after `MAX_PAGES` the station is `truncated_before` |
| `turn.completed` without its `turn.started` (before the first stored event) | the outcome counts, the duration does not |
| A home on macOS (or an unverifiable mount) | read, `verified: false` |

### 3.16 Out of scope

- Speed tiers, the long-context premium, the 1-hour cache write price (the fit of §1.2 prices all cache writes at one rate).
- A price editor on the board (t3code `UsagePriceOverrides`); the table changes with a release.
- Export (CSV), alerts, budgets.
- Per-member attribution of usage (who typed); the counters are per session.
- The MCP `fleet_view` and the CLI do not get analytics.
- Other providers than Claude and Codex.

## 4. Threat notes

1. **Transcript text.** The station reads files that hold prompts, tool output and secrets. The parsers keep only numbers, the model name, the conversation id, the Codex thread and parent ids, the response id (as a hash in `analytics_seen`) and the time; the typed Claude struct ignores every other field without storing it; nothing of a line is logged. `analytics_files.rel_path` holds the encoded working folder: station-local only, never in an answer or a log.
2. **What leaves the station:** numbers, model names, the eight tool kinds, the error codes that match `^[a-z][a-z0-9_]{0,31}$`, session ids, hours, the source key (a hash, not a path), the conversation key (a hash, not the id), the price table date. No path, no cwd, no MCP server name, no prompt, no tool input, no conversation id.
3. **Members** (revision 2).
   - A member gets only usage rows that a proven run of a covered session made (§3.6a): never usage outside Fleet Deck, never a terminal continuation after the session ended, never the earlier history of an imported session (the `history` permission is honored by construction), never a record at a time when two sessions held the conversation, never a station outside the share (404 as for an unknown name), never another session's counters.
   - A member gets no station-wide source facts: no file count, no unread bytes, no scan time, no malformed count, no reason word, no `keeps_days`, and a `first_at` clipped to `events_since` (§3.7 step 7). What stays: the provider, the source key, `ok`/`missing`/`unavailable`/`off`, `verified`, `complete`, the station-wide `events_since`, and `kept_since` of the covered sessions.
   - The station decides the attribution and the member form; the deck sets the audience from the grant, drops uncovered or unattributed rows and clears the owner-form fields again (§3.9 step 5). The station trusts the deck for the audience and the session list, as for every member read over the link.
   - **Residual risk (accepted, stated):** when the owner continues a shared session's conversation in a terminal while that session's Fleet Deck process still runs, the terminal records count for the shared session (§3.6a item 6). The rows hold numbers and model names only.
4. **Tokens and walls** get 403: analytics is a board login read.
5. **Reading the home.** The walk follows no link; a file must be a regular file of the station user; the home passes the browse's resolution (`resolve_plan`): an `Untrusted` home is not read; an `Unverifiable` home (macOS, an unknown mount) is read from its canonical path and reported `verified: false`, as the browse does today. A process of the owner (or, on an unverifiable mount, of whoever can write there) can write a transcript with false numbers; that changes only numbers on the page, never a permission.
6. **Load.** One scan thread, at most 2 GiB or 5 min per pass; one query at a time per station; a deck-wide limit of 4; a page at most `MAX_PAGE_BYTES` (960 KiB, under the 1 MiB frame cap, checked after serialization), at most 16 pages per station and request (about 15 MiB). A cold Lighting backfill reads 4.3 GB once.
7. **Storage.** The seen keys are kept forever (about 20 MB a year on the busiest station); the other `analytics_*` tables grow by a few thousand rows a month.
8. **The prune fence.** The prune keeps every event that the analytics cursor did not count yet (§3.3). In the normal state the cursor is caught up after every append and the fence changes nothing. A failed `catch_up_step` (a database error; `observe_one` never fails on event content) is retried every 30 s and logs `analytics catch-up failed` each time: the events grow past the retention window only while that warning repeats. An inline `observe` error fails its append, as for the other observers.

## 5. Risk class

**High.** A station migration (V50), a new link method and route, and a member visibility rule over data of other sessions. Adversary DESIGN REVIEW of this document before the build; PRE-PR REVIEW of the final branch. Test focus of the review: the attribution to proven runs and the member form (station and deck), the event cursor and the prune fence, the paging against the frame cap, the dedupe, resume and Codex restart rules (no double count, no loss), the parsers on hostile input (huge lines, missing fields, ancestry cycles), and the migration.

## 6. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Workers compile only (`cargo check -p <crate> --tests`, `bun run typecheck`, `bun run lint`) and keep every file under 800 lines; the coordinator runs the tests at the merge (owner rule 2026-10-23). At most two workers at a time (machine rule).

Order (revision 2: ten tasks): A1 ∥ A2 → A3 → A4 ∥ A5 → A6 → A7 → A8 ∥ A9 → A10.

### A1 (backend): protocol, prices and the link method row (≈ 400 lines)

- **Owned files:** `crates/fleetdeck-proto/src/analytics.rs` (new), `crates/fleetdeck-proto/src/analytics_tests.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-providers/src/prices.rs`, `crates/fleetdeck-providers/src/prices_tests.rs` (new or existing test module of `prices.rs`), `crates/fleetdeck-link/src/methods_table.rs` (one row), `docs/v2/adr/ADR-004-link-protocol-v1.md` (the feature row and the §9.6 row), `web/src/gen/analytics/*` (generated), the ts-drift list if the proto crate keeps one.
- **Changes:** §3.2 (types, `Audience`, paging constants, `metric`, `check`, feature, the method row and the ADR rows), §3.8.
- **Tests:** `analytics_query_check_refuses_a_bad_window` (since < 0, since not an hour start, since ≥ until, a `before` that is not an hour start or not above `since`, 10,001 sessions; a window of 10 years is valid); `analytics_answer_round_trips_and_omits_empty_options` (JSON of a `UsageHour` without session and cost has neither key; an answer without `next_before` has no key); `source_state_is_flattened` (`{"state":"unavailable","reason":"owner",…}`); `max_page_bytes_leaves_room_for_the_envelope` (`MAX_PAGE_BYTES + 64 KiB == MAX_CONTROL_FRAME_BYTES`); `estimate_price_of_has_the_fitted_claude_rows` (the three rows, `[1m]` and a date suffix removed, haiku 4.5 and fable `None`); `estimate_cost_matches_the_recorded_cost_states`: opus (18,601 input, 1,039,922,797 cache read, 119,025,395 cache write, 2,140,297 output) → 845.9919 ± 0.01; sonnet (21,998; 1,734,626,170; 68,605,469; 12,197,000) → 466.9903 ± 0.01; haiku (18; 114,047; 127,790; 2,069) → 0.01815 ± 0.00001; `price_of_still_has_no_claude_row`. Link: the existing `crates/fleetdeck-link/tests/method_table.rs` (code and ADR agree) and `station_analytics_is_a_read_method_of_analytics_v1` (new, in the link crate's method tests: `authorize("station.analytics", Deck, Station, None, <negotiated with analytics.v1>, Some(read))` is `Ok`; without the feature it is `feature_not_negotiated`).
- **depends_on:** none.

### A2 (backend): transcript parsers (≈ 600 lines with fixtures)

- **Owned files:** `crates/fleetdeck-station/src/analytics/mod.rs` (new: §3.4 types, module list; `scan`, `query` and `walk` declared by A6/A7: A2 declares only `pub mod claude; pub mod codex;`), `analytics/claude.rs`, `analytics/codex.rs`, `analytics/claude_tests.rs`, `analytics/codex_tests.rs`, `analytics/fixtures/*.jsonl` (new), `crates/fleetdeck-station/src/lib.rs` (`pub mod analytics;`).
- **Changes:** §3.4. Fixtures are hand-written with the field names of §1.1 and no prompt text (content arrays hold `"x"`).
- **Tests (each compares with a known value):**
  - Claude: `a_message_counted_once_across_its_content_blocks` (3 records with one `message.id`/`requestId`: 3 `Record`s with one key; the store dedupe is A3); `synthetic_and_usage_free_records_are_nothing`; `a_bad_timestamp_is_malformed`; `the_four_parts_map_to_new_input_cache_read_cache_write_output` (input 3, cache read 1,000, cache creation 200, output 50).
  - Codex: `token_count_deltas_follow_the_cumulative` (totals 100/0→250/50→400/50 output 10/20/30: records 100,150,150 input); `a_token_usage_record_of_the_same_request_is_not_counted_again`; `fork_copies_set_the_baseline_only` (meta with `forked_from_id` at t, copies at t+10, t+20 ms, a real record at t+5 s: one record of the delta); `a_restart_larger_than_the_baseline_counts_the_whole_request` (baseline (100, 10), then `C = L = (500, 50)`: one record (500, 50), a new epoch); `a_restart_below_the_baseline_counts_the_request` (baseline (300, 30), then C (120, 12), L (20, 2): one record (20, 2)); `a_missing_report_counts_the_growth` (baseline (100, 10), then C (400, 40), L (100, 10): one record (300, 30)); `keys_differ_across_counter_epochs` (cumulative (100, 10), (300, 30), restart (100, 10): three different keys; a second parse of the same lines from a fresh state gives the same three keys); `a_response_id_is_the_key` (`codex:resp:<id>`); `both_parent_encodings_are_read` (`source.subagent.thread_spawn.parent_thread_id` and `parent_thread_id`, `parse_meta` and `parse_line` agree); `cached_input_is_taken_out_of_new_input` (input 1,000, cached 800, write 100 → input 100); `the_model_comes_from_the_last_turn_context`; `state_round_trips_through_json`.
- **depends_on:** none.

### A3 (backend): migration, the usage store and the prune fence (≈ 550 lines)

- **Owned files:** `crates/fleetdeck-station/src/store/schema_v50.rs` (new), `store/migrations.rs` (one `M::up` line after `:659`), `store/analytics.rs` (new: file rows, thread rows, the root resolution, the attribution lookup, the per-file transaction, seen keys, buckets, the reads of §3.7 steps 4, 5 and 7), `store/analytics_tests.rs` (new), `store/mod.rs` (module line; the `Store` methods that wrap `analytics.rs`), `store/log.rs` (the prune fence line in `prune` only).
- **Changes:** §3.3 (tables and the fence), §3.5 step 4 (root, attribution, the transaction), §3.7 steps 4, 5 and 7.
- **Tests:** `v50_creates_the_tables_and_the_meta_row` (with events seq 5..9 at times 1,000..: `events_since` 1,000, `next_seq` 5; an empty events table: `next_seq` = `meta.station_seq_next`); `a_file_commit_counts_each_key_once` (two commits with overlapping keys: one bucket, `requests` = distinct keys); `records_without_a_key_always_count`; `hour_buckets_floor_to_the_utc_hour`; `the_prune_keeps_events_that_the_cursor_did_not_count` (events 1..10 acknowledged and old, `next_seq` 4: the prune deletes 1..3 only; `next_seq` 11: it deletes 1..10); `a_record_inside_one_run_goes_to_that_session`; `a_record_outside_every_run_has_no_session` (session A shared and ended, the same conversation continued later: no session); `an_imported_prefix_has_no_session` (session B imports at t; records before t: no session; records in B's run: B); `a_record_in_two_runs_has_no_session`; `an_hour_across_a_run_edge_splits_into_two_buckets`; `codex_root_follows_the_ancestry` (root ← child ← grandchild: the grandchild's records are in the root's conversation); `a_missing_parent_is_the_root`; `an_ancestry_cycle_keeps_the_own_thread`; `the_member_form_has_no_owner_fields` (sources of a member read: `files`, `pending_bytes`, `scanned_at`, `malformed`, `keeps_days` `None`, reason `unavailable`, `first_at` clipped to `events_since`); `member_rows_are_attributed_rows_of_the_listed_sessions`; `kept_counters_come_from_sessions_checkpoints_and_shell_runs`.
- **depends_on:** A1 (types), A2 (`UsageRecord`, `ThreadMeta`).

### A4 (backend): the event cursor, the counters and the execution intervals (≈ 550 lines)

- **Owned files:** `crates/fleetdeck-station/src/store/analytics_observe.rs` (new), `store/analytics_observe_tests.rs` (new), `store/log.rs` (the `observe` call after `workflow_state::observe` only). A4 starts after A3 merged into the track branch (both change `log.rs`).
- **Changes:** §3.6 (the cursor, the table, the start of the station close), §3.6a item 1 (the intervals).
- **Tests:** one per row of the §3.6 table with its exact counter (`a_turn_counts_its_status_and_duration`: started at 1,000, completed `failed` at 4,000 → `turns.failed` 1, `turns.ms` 3,000, `turns.timed` 1; `a_tool_failure_counts_under_its_kind`; `an_approval_counts_who_answered_and_the_wait`; `an_error_code_that_does_not_match_counts_as_other`; `a_finalized_turn_adds_files_and_lines`; `a_resume_is_not_a_start`; `a_workflow_counts_once_across_its_updates` (3 updates of one task: `workflows` 1); `a_workflow_already_in_workflow_runs_still_counts_once` (the projection row exists before the first counted update: `workflows` 1); `the_end_of_a_session_clears_its_open_rows`; `message_deltas_write_nothing`); intervals: `a_start_opens_and_an_end_closes_the_run`; `a_start_of_another_holder_closes_the_open_run`; `a_lost_session_closes_its_run`; cursor: `catch_up_counts_the_old_events_once_and_observe_the_new` (events 1..10 before V50, 11..12 after: every counter once); `a_live_event_waits_while_the_cursor_is_behind` (V50 with events 1..5; append 6 (`turn.completed` of the turn started at 3) before any catch-up: nothing counted at the append; catch-up then counts 1..6 in order: `turns.timed` 1 and no open row left); `catch_up_runs_in_steps` (limit 3 over 10 events: 4 calls, the last returns `false`, `next_seq` 11).
- **depends_on:** A3.

### A5 (backend): the settings knob (≈ 120 lines)

- **Owned files:** `crates/fleetdeck-core/src/config_knobs_station.rs`, `config_raw.rs`, `config.rs`, `settings/fields_knobs.rs`, `config_knobs_tests.rs`.
- **Changes:** §3.13.
- **Tests:** `analytics_transcripts_defaults_to_true`; `analytics_transcripts_false_parses`; `the_field_is_listed_with_restart`.
- **depends_on:** none (runs in parallel with A4).

### A6 (backend): the walk and the scanner (≈ 550 lines)

- **Owned files:** `crates/fleetdeck-station/src/analytics/mod.rs` (add `pub mod scan; mod walk;` and `ScanStatus`), `analytics/walk.rs`, `analytics/scan.rs`, `analytics/scan_tests.rs`, `crates/fleetdeck-station/src/history/browse.rs` (`plan_homes`, `HomePlan`, `resolve_plan` and `Resolved` to `pub(crate)` only), `crates/fleetdeck-station/src/serve.rs` (spawn, and the shutdown join).
- **Changes:** §3.5 (trust policy, walk, Codex ancestry first, candidates, schedule, off, status), §3.6 the cursor driving and the start of the station close.
- **Tests:** `a_pass_reads_new_lines_only` (append to a fixture file between passes: the second pass reads from the offset); `a_rewritten_file_is_read_again_without_double_counts`; `a_same_size_rewrite_is_read_again` (same size, new mtime: read from 0, keys dedupe, new records count); `an_unfinished_file_is_read_in_the_next_pass_without_new_bytes` (a byte budget that stops inside a file; the next pass, with the file unchanged, reads the rest: totals equal a one-pass read); `an_unfinished_last_line_waits_for_the_next_pass`; `links_and_files_of_other_users_are_not_read` (a symlinked transcript is skipped); `nested_subagent_and_workflow_files_are_read_and_journal_is_not`; `codex_metas_are_read_before_usage` (a grandchild file that sorts before its parent: its usage goes to the root); `an_unverifiable_home_is_read_and_marked` (a resolver stub that returns `verified: false`: the home is read, `verified: false`, `complete` true at the end); `an_untrusted_home_is_unavailable`; `off_reads_nothing_and_reports_off`; `keeps_days_reads_cleanup_period_days`; `a_pass_stops_at_its_byte_budget_and_reports_pending_bytes`; `the_first_pass_waits_for_the_cursor` (no scan before `catch_up_step` returns `false`).
- **depends_on:** A3, A4, A5.

### A7 (backend): the query and the link intake (≈ 450 lines)

- **Owned files:** `crates/fleetdeck-station/src/analytics/mod.rs` (add `pub mod query;`), `analytics/query.rs`, `analytics/query_tests.rs`, `crates/fleetdeck-station/src/link/intake_analytics.rs` (new), `link/intake_analytics_tests.rs` (new), `link/intake.rs` (one dispatch line), `link/mod.rs` (module line), the station feature advertisement if it lists features separately from `feature::ALL`.
- **Changes:** §3.7.
- **Tests:** `the_answer_prices_the_buckets` (fixture store → one Claude bucket of 1 opus request with the A1 price); `the_answer_filters_by_sessions`; `a_member_answer_has_only_attributed_rows_and_the_member_form`; `pages_hold_whole_hours_newest_first` (rows in 5 hours, a budget that fits 2: 3 pages, `next_before` chained, the union equals a one-page read); `a_page_fits_the_real_frame` (a store with 60,000 usage rows over 2,000 hours, model names of 64 bytes: every page, wrapped as `ControlFrame::Response { id: u64::MAX, outcome: Outcome::Ok(serde_json::to_value(&page)?) }` and serialized with `serde_json::to_string` as `control.rs::encode` (`:231`) does, is at most `MAX_CONTROL_FRAME_BYTES`; the pages together hold every row); `an_hour_larger_than_a_page_is_partial`; `a_second_query_is_busy`; link: `station_analytics_refuses_bad_params`. (The method row is tested in A1: `station_analytics_is_a_read_method_of_analytics_v1`.)
- **depends_on:** A6.

### A8 (backend): the deck route (≈ 500 lines)

- **Owned files:** `crates/fleetdeck-deck/src/http/analytics_routes.rs` (new), `http/methods_table_rest.rs`, `http/mod.rs`, `crates/fleetdeck-deck/tests/analytics_route.rs` (new), the methods-table count test if it pins the number of methods.
- **Changes:** §3.9.
- **Tests:** `the_owner_gets_every_station_with_unattributed_rows`; `a_member_with_sessions_gets_only_their_stations_and_rows` (a fake station that answers rows of two sessions and rows without a session: the member answer has one session's rows only, `owner: false`); `a_board_share_member_gets_no_unattributed_rows`; `a_member_without_history_gets_the_same_query` (`history: false` and `true` send the same params); `the_deck_clears_owner_fields_for_a_member` (a fake station that wrongly sends `files` and a reason: the member answer has `None` and `unavailable`); `pages_are_read_until_next_before_is_none`; `the_sixteenth_page_sets_truncated_before`; `a_member_asking_an_uncovered_station_gets_404`; `a_token_and_a_display_get_403`; `an_offline_and_an_old_station_are_states_not_errors`; `a_station_timeout_is_timeout`; `bad_params_are_400`.
- **depends_on:** A1, A7 (the station side for the integration test with a real station; with a fake station the test needs only A1).

### A9 (ux): data functions (≈ 500 lines)

- **Owned files:** `web/src/analytics/api.ts`, `web/src/analytics/merge.ts`, `web/src/analytics/learnings.ts`, `web/src/analytics/merge.test.ts`, `web/src/analytics/learnings.test.ts` (all new).
- **Changes:** §3.10 Merge, §3.12.
- **Tests:** `merge_sums_the_four_parts_and_never_adds_reasoning`; `merge_groups_hours_into_local_days` (zone `America/New_York`, an hour at 03:00 UTC goes to the day before); `merge_keeps_the_largest_row_of_a_shared_home` (stations A and B with one source key: a bucket with 3 requests on A and 5 on B counts 5 once; a bucket only on A counts; a tie keeps the row with a session); `merge_keeps_unpriced_tokens_out_of_the_cost`; `merge_skips_stations_that_did_not_answer`; `merge_has_no_not_in_run_for_a_member`; `the_window_starts_at_an_hour` (now 10:30, past 24 hours: `since` = yesterday 10:00; `all`: 0); one test per learning: shown at its threshold, hidden one under it, the `(since …)` suffix; `cost_per_turn_is_hidden_when_a_session_with_turns_has_no_usage` (20 turns, one session of them without usage: hidden); `cost_per_turn_is_hidden_while_a_source_is_not_complete`.
- **depends_on:** A1 (generated types).

### A10 (ux): the page, the chart and the routes (≈ 600 lines)

- **Owned files:** `web/src/analytics/AnalyticsPage.tsx`, `web/src/analytics/Bars.tsx`, `web/src/analytics/analytics.css`, `web/src/analytics/AnalyticsPage.test.tsx`, `web/src/analytics/Bars.test.tsx` (new), `web/src/app/routes.ts`, `web/src/app/routes.test.ts`, `web/src/App.tsx`, `web/src/app/Sidebar.tsx`, `web/src/app/Layout.tsx`, the phone More page file that lists its rows, `docs/v2/DESIGN-SYSTEM.md` (§3 `.fd-bars`, §7 log line), the gallery entry for the new page (`web/e2e` screenshot list) if the gallery lists pages.
- **Changes:** §3.10 page and routes, §3.11. Use the fleet-design-system and dataviz skills.
- **Tests:** `routes_parse_analytics_and_a_station`; `the_page_shows_totals_and_the_price_date`; `days_before_the_transcripts_are_not_zero` (the dashed start line and `No data before …`); `a_member_page_has_no_not_in_run_line_no_station_table_and_no_file_counts`; `an_old_station_says_update`; `truncated_and_partial_hours_are_said`; `the_bars_are_focusable_with_their_values`; `the_hidden_table_repeats_the_values`.
- **depends_on:** A9.
- **Acceptance for A10 also:** `sh tools/design-check.sh`; gallery shots of the page in both themes, desktop and phone.

### At the merge (coordinator)

- `cargo nextest run -p fleetdeck-proto -p fleetdeck-providers -p fleetdeck-core -p fleetdeck-link --no-fail-fast -E 'test(analytics) | test(estimate) | test(price_of) | binary(ts_drift) | binary(method_table)'`
- `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(analytics) | test(v50) | test(retention) | test(prune) | test(catch_up)'`
- `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(analytics_route) | test(methods_table)'`
- `cd web && bun run typecheck && bun run lint && bun run test src/analytics src/app`
- The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
- `make check`.
- A live check on Lighting (read only of the transcripts): the station's first full pass ends; the page for 30 days shows Claude from 2026-09-29, and `All time` shows Codex from 2026-04-06; the Claude tokens of one known conversation equal the sum of `scratch/buckets.py` restricted to it; a member login of one shared session sees only rows from 2026-10-17 on, and no file counts.
- A live check on mac-m4: the sources are `ok` with `verified: false`, and the page shows its Claude and Codex history.
- RE-REVIEW of this design (adversary) before A1 starts; PRE-PR REVIEW (adversary) of the final branch.

## 7. Decisions made in this draft

1. **The time series of tokens and cost comes from the provider transcripts only**, with one pricing rule for every row. Rejected: the session totals of B14 (no time series, events gone after 7 days, 17 of 18 Lighting sessions hold an old count) and a mix of both (two numbers for one session).
2. **The station keeps hour buckets and dedupe keys, not records.** Rejected: one row per record (about 1.5 M rows a year on Lighting); per-file buckets without keys (no dedupe across files; Claude copies messages into forks).
3. **No age limit on the ingestion; the dedupe keys are kept forever** (revision 2; the owner's rule "the metrics go as far as the data goes", `BACKLOG.md:407`). The page offers `All time`. Rejected: the 400-day cutoff of revision 1 (it discarded extant history); a key prune by age (a fork or archive copy of an old record would count twice); a key prune by the files on disk (a restored backup would count twice). Cost: about 20 MB of keys a year on the busiest station.
4. **Codex counts the growth of the thread's cumulative total** (both `token_count` and `token_usage_record`), with the t3code fork-copy rule and the t3code restart check `C == L` before any subtraction (revision 2); the keys hold a replay-stable counter epoch, or the `response_id` when there is one; a sub-agent's usage goes to its root thread through a persisted ancestry. Rejected: `last_token_usage` alone (the pair of records for one request would count twice); `token_usage_record` alone (older rollouts have only `token_count`); a key of the cumulative alone (it repeats after a restart).
5. **Claude rows enter the price table for the estimate only**, fitted from the CLI's own `cost-state`. Rejected: no Claude cost (the main provider would have no cost); spreading the `cost-state` totals over time (invented history); a network price list (DL-444).
6. **A usage record belongs to a Fleet Deck session only inside a proven run of that session** (revision 2, §3.6a), decided per record at ingestion; everything else has no session and is owner only. Rejected: the session created first (revision 1: it relabelled a later private run, a terminal continuation or an imported history as a shared session's usage); count it in every holder (double counts, and the same leak); attribution per hour bucket at query time (an hour across a run edge would mix proven and unproven records).
7. **The deck returns the union of the station answers; the board sums into local days.** Rejected: deck-side sums (the days must follow the viewer's time zone; the station boundary of a day would differ per station).
8. **Fleet Deck counters follow one ordered event cursor:** inline in the event transaction when the cursor is caught up, by `catch_up_step` when it is behind (the retained events at V50, or after a failure); the prune keeps every event that the cursor did not count (revision 2). The kept tables (`sessions`, `turn_checkpoints`, `shell_runs`) are read at query time for all history. Rejected: a separate backfill range next to a live observer (revision 1: a live completion could come before its historical start, and the prune could delete events before the backfill read them); a fold over the deck timeline (500 rows per session, not complete); counting from the migration only (loses 7 days that exist).
9. **Each section shows its own start date;** no metric is drawn before its source starts. Rejected: one page-wide start date (it would hide the Codex history or invent Claude history).
10. **Members see the proven runs of their covered sessions and a member form of the sources; everything else is owner only;** tokens and walls get 403. A member never sees the imported earlier history, whatever `grant.history` says. Rejected: owner only (the guidance asks for the fleet-view rule); outside usage for board-share members (it is the owner's private terminal work); the full source facts for members (revision 1: station-wide file counts, bytes and dates of other work); imported history for members with `history: true` (one more rule for counts that the history view does not show).
11. **One scan thread, every 10 min, 2 GiB or 5 min per pass;** the Refresh button does not start a scan. Rejected: a scan per page read (a cold read of 4 GB on a click).
12. **A station knob `analytics.transcripts`, default on.** Rejected: no switch (the owner must be able to stop the reading of private files); default off (the page would be empty).
13. **A new `.fd-bars` primitive** in the design system, one series, `--fuel`. Rejected: a chart library (a new dependency and a second visual language); a stacked chart by provider (needs new categorical colors; the model table shows the split).
14. **Five learnings with thresholds;** the others in Q3 are rejected for lack of a rule that the data holds.
15. **No deck migration, no deck cache.** The page reads on open only.
16. **The link answer comes in pages of whole hours, at most 960 KiB each, checked after serialization; the deck reads at most 16 pages per station** (revision 2). Rejected: a row limit (revision 1: 50,000 rows are far above the 1 MiB frame cap); day buckets on the station (the days must follow the viewer's time zone); a data stream (more protocol for an answer of a few MB at most).
17. **The home trust of analytics is the browse's (`resolve_plan`): `Untrusted` is not read, `Unverifiable` is read and marked `verified: false`** (revision 2). Rejected: `trusted_home` alone (revision 1: every macOS home would be unavailable); a second policy for analytics (two rules for one home).
18. **A home that two stations read merges per bucket identity (the most complete row counts)** (revision 2). Rejected: the first station by name (revision 1: it dropped rows that only the other station still held); a sum (double counts).

## 8. Owner questions (the build proceeds with the defaults)

1. **Codex prices.** `gpt-6.1-sol`, `gpt-daybreak-blue-latest`, `gpt-5.6-sol`, `gpt-6-astra`, `gpt-5.4` and `gpt-5.5` have no row. Their tokens show as "no price". Default: leave them unpriced until the owner gives the rates.
2. **Claude rates fitted from the CLI's own records** (§1.2). Default: use them for the estimate, labelled "Cost estimate" with the table date.
3. **Keep the numbers after the CLI deletes a transcript** (Claude keeps 30 days). Default: yes; only numbers and model names are kept.
4. **Usage not in a Fleet Deck run** (terminal, VS Code, other apps that use the same provider home, older and imported history) on the owner's page. Default: yes, as its own line and in the totals.
5. **Default period of the page.** Default: 30 days (Claude's reach).
6. **Members see analytics of their shared sessions.** Default: yes, only the usage of proven runs (§3.6a) and the counters of those sessions.
7. **Imported history for members with `history: true`.** Default: not shown in analytics (the history view stays the place for it).
8. **The terminal continuation residual** (§3.6a item 6): the owner continues a shared session's conversation in a terminal while its Fleet Deck process runs. Default: accepted and stated; those numbers count for the shared session.

## 9. Recordings and sessions

- `measure-lighting.json`, `measure-lighting-db.json`, `measure-buckets.json` in `/home/luismorales/fleetdeck-briefs/backlog2/analytics/` (counts, key names, model and tool names, dates; no record text). Scripts in `scratch/` (`measure.py`, `stationdb.py`, `buckets.py`, `rates.py`, `costrow.py`, `subagent_ids.py`), run with `python3 -I` on Lighting.
- The other stations (thunder, dev1, mac-m4) were not measured: the architect's fleet grant answered "The session token is not valid." No Fleet Deck session was started. The page itself shows the reach of each station's transcripts (the Sources fold), so the first live run measures them.

## Decision log entries (to append)

## DL-560: B29 analytics: tokens and cost by time come from the provider transcripts (design agent, 2026-10-24)

- Context: B29; design `docs/v2/design/analytics.md` §1-§3. The station events are kept 7 days and the session totals have no time; the transcripts hold every API response with its time (Claude 30 days by the CLI default, Codex since install).
- Decision: each station scans its provider homes every 10 minutes, incrementally by byte offset with a hash guard (an unfinished file is always read again; a same-size rewrite is found by its mtime), and stores hour buckets per home, provider, model, conversation and proven session, with dedupe keys kept forever (Claude `message.id:requestId`, Codex `response_id` or thread, counter epoch and cumulative total). No age limit on the ingestion. It keeps the numbers after the CLI deletes a transcript. The home trust is the browse's (`Unverifiable` homes, every macOS home, are read and marked). A station knob `analytics.transcripts` (default on) stops the reading.
- Rejected: the B14 session totals as a time series; one stored row per record; a scan on each page read.

## DL-561: B29 Claude transcript records are deduplicated globally by `message.id:requestId` (design agent, 2026-10-24)

- Evidence: 203,926 records, 105,652 keys on Lighting (one record per content block, copies in forks).
- Decision: a record counts only when its key is new in `analytics_seen`; the bucket and the key commit in one transaction with the file offset.

## DL-562: B29 Codex usage counts the growth of the thread's cumulative total (design agent, 2026-10-24)

- Decision: `token_count.total_token_usage` and `token_usage_record.thread_token_usage` feed one baseline per file; an equal total is the same request; a total equal to the request (`C == L`) after a non-zero baseline is a restart and counts the whole request; the first total of a file, or a total below the baseline, counts the request's own usage; otherwise the growth counts; records of a fork or sub-agent copy burst (gaps under 1 s after `session_meta`) set the baseline only; each restart starts a counter epoch that is part of the key; a sub-agent counts in its root thread's conversation through `analytics_threads` (both parent encodings, at most 16 levels, a cycle keeps the own thread).
- Rejected: `last_token_usage` alone (double counts with `token_usage_record`); `token_usage_record` alone (older rollouts lack it).

## DL-563: B29 Claude rows in the price table, for the analytics estimate only (design agent, 2026-10-24)

- Decision: `estimate_price_of` adds `claude-opus-5-5` (4.0 / 0.2 / 5.0 / 20.0), `claude-sonnet-5-5` (2.0 / 0.1 / 2.5 / 10.0) and `claude-haiku-5-5` (0.1 / 0.01 / 0.125 / 0.5) USD per 1M (input, cache read, cache write, output), fitted exactly from the CLI's own `cost-state` records; `PRICE_TABLE_VERSION` becomes `2026-10-24`. `price_of` keeps its Codex-only rule (DL-444) for the live counts. Every analytics cost is "Cost estimate" with the table date; a model without a row is "no price", never zero.

## DL-564: B29 Fleet Deck counters per hour and session, in the event transaction (design agent, 2026-10-24)

- Decision: `analytics_observe` counts turns by outcome and duration, tool calls by kind and outcome, approvals by who answered and their wait, errors by code, files and lines changed, starts, resumes, runs, artifacts and workflows (analytics-owned identities, first seen counts) into `analytics_activity`, and the execution intervals of the sessions into `analytics_exec`; one ordered cursor (`analytics_meta.next_seq`) counts every event once in order, inline when caught up and by `catch_up_step` when behind (the events that the station still holds at V50); the prune keeps every event that the cursor did not count; sessions started, turns recorded and shell commands come from the kept tables at query time. Each family reports its start date (`events_since`, `kept_since`).

## DL-565: B29 the link method `station.analytics`, feature `analytics.v1` (design agent, 2026-10-24)

- Decision: params `since` (an hour start), `until`, `before` (the page cursor), `sessions` (optional filter), `audience` (owner or member); the answer has the usage and activity rows of whole hours, newest first, at most `MAX_PAGE_BYTES` (960 KiB) serialized, the sources (owner or member form), the coverage, the price table date and `next_before`; the deck reads at most 16 pages per station (`truncated_before`). One query at a time per station. The method row is in `fleetdeck-link/src/methods_table.rs` and ADR-004 §9.6 (deck → station, read, `analytics.v1`, not mutating).

## DL-566: B29 analytics visibility: members see the proven runs of their covered sessions, everything else is owner only (design agent, 2026-10-24)

- Decision: `GET /api/analytics` serves the owner (every station, every row) and members (the stations and sessions of `ShareScope::covers`, an uncovered station is 404); a usage record belongs to a session only when its time is inside a proven execution interval of exactly one holder of its conversation (decided per record at ingestion); a member gets only such rows of covered sessions and the member form of the sources (no file counts, bytes, scan times, reasons, `keeps_days`; `first_at` clipped to `events_since`); the imported earlier history of a session is never in a member answer, whatever `grant.history` says; the deck filters the rows and clears the owner-form fields again; tokens and wall displays get 403.

## DL-567: B29 the deck returns the union of the station answers; the board sums into local days (design agent, 2026-10-24)

- Decision: the deck fans out, reads the pages of each station (15 s per page, 60 s per station) and returns each station's state and merged answer; the board merges, groups hour rows into days of the browser's time zone, and for a provider home that two stations report (same source key) keeps the row with the most requests per bucket identity (hour, provider, model, conversation key).

## DL-568: B29 no invented history on the analytics page (owner scope rule; design agent, 2026-10-24)

- Decision: a day before a source's first record is not a zero: the chart marks the start (`No data before <date>`) and each section says its start date; learnings computed over a part of the window say `(since <date>)`; five learnings only, each with a rule and a minimum sample.

## DL-569: B29 design system: `.fd-bars`, a one-series bar chart (design agent, 2026-10-24)

- Decision: an inline SVG bar chart with `--fuel` bars (or neutral `--dim`), a zero baseline, a top value line, three x labels, focusable bars with a readout line and a hidden value table, a dashed start line for missing history; listed in `DESIGN-SYSTEM.md` §3 and §7.
- Rejected: a chart library; stacked categorical colors.

## Revision 2 (design review)

Adversary DESIGN REVIEW round 1 (`/home/luismorales/fleetdeck-briefs/backlog2/analytics/adv-design.md`, on `3e46323b`): FAIL. The architect's deputy checked each finding against the code on `d1fa4205`; every finding held. The changes:

| # | Severity | Finding | Checked against | Change |
| --- | --- | --- | --- | --- |
| 1 | BLOCKER | The session created first took a whole conversation, so a later private run (an import into an unshared session, a terminal continuation) showed as a shared session's usage. | `store/resumes.rs:229-300` (holders that are not busy are allowed), `manager/usage_recount.rs:321-326` | New §3.6a: per-record attribution at ingestion to a proven execution interval (`analytics_exec`, from `session.started` to the end of that process) of exactly one holder; everything else has no session and is owner only. `analytics_usage` gets `session_id` in its key; §3.5 step 4, §3.7 step 4, §4 item 3, decision 6, DL-566. The one residual case (a terminal continuation while the Fleet Deck process runs) is stated in §3.6a item 6, §4 and owner question 8. |
| 2 | BLOCKER | Analytics bypassed `ShareGrant.history`. | `deck/src/store/members.rs:61`, `http/history_routes.rs:135-143` | The imported prefix is before the importing session's first run, so it is never attributed (§3.6a items 2-3); a member never gets it, whatever `history` says (stricter than the history view). Hour buckets cannot cross the boundary: the attribution is per record and the bucket key holds it (§3.6a item 4). Test `an_imported_prefix_has_no_session`, `a_member_without_history_gets_the_same_query`. |
| 3 | BLOCKER | Member answers carried station-wide source facts. | design §3.7 step 8, §3.9 step 5 (revision 1) | `Audience` param; a member form of `AnalyticsSource` (owner-only fields are `Option` and `None`; reason `unavailable`; `first_at` clipped to `events_since`) and of `kept_since` (§3.2, §3.7 step 7); the deck clears the fields again (§3.9 step 5). |
| 4 | BLOCKER | `station.analytics` had no method row: every call would be `unknown_method`. | `fleetdeck-link/src/control.rs:280`, `methods.rs:79-100`, `methods_table.rs:306-322` | The row (deck → station, read, `analytics.v1`, not mutating), the ADR-004 feature row and §9.6 row (§3.2); A1 owns them and tests `authorize`. |
| 5 | BLOCKER | The prune could delete events before the backfill counted them. | `serve.rs:84-112` (prune 60 s after start), `store/log.rs:353-405` | The prune fence `through = min(through, next_seq - 1)` in the prune transaction (§3.3); the cursor runs at the task start with no wait (§3.5 step 5). Test `the_prune_keeps_events_that_the_cursor_did_not_count`. |
| 6 | MAJOR | 50,000 rows do not fit the 1 MiB control frame. | `fleetdeck-proto/src/link.rs:17`, `fleetdeck-link/src/control.rs:231-238` | Pages of whole hours, at most `MAX_PAGE_BYTES` (1 MiB less 64 KiB), counted as `serde_json` writes them and checked after serialization; the deck reads up to 16 pages (`next_before`, `truncated_before`, `partial_hour`) (§3.2, §3.7 step 6, §3.9 step 4). Test `a_page_fits_the_real_frame` serializes the real `ControlFrame::Response`. `MAX_ROWS` is gone. |
| 7 | MAJOR | Backfill and live observation applied stateful events out of order. | design §3.6 (revision 1) | One ordered cursor `analytics_meta.next_seq`: inline in the event transaction only when caught up, else `catch_up_step` in order (§3.6). Test `a_live_event_waits_while_the_cursor_is_behind`. |
| 8 | MAJOR | The workflow count read the folded, pruned `workflow_runs`. | `store/workflow_state.rs:63-90`, `:251` | `analytics_workflows(session_id, task_id)`, `INSERT OR IGNORE`, count on insert (§3.3, §3.6). Tests `a_workflow_counts_once_across_its_updates`, `a_workflow_already_in_workflow_runs_still_counts_once`. |
| 9 | MAJOR | A file left unfinished by a budget could be skipped forever; a same-size rewrite was not found. | design §3.5 step 3 (revision 1) | `mtime_ns` column; a candidate is a file with no row, a changed `dev`, `ino`, size or mtime, or `offset < size`; a same-size rewrite with a new mtime is read from 0 (§3.3, §3.5 step 4). Tests `an_unfinished_file_is_read_in_the_next_pass_without_new_bytes`, `a_same_size_rewrite_is_read_again`. |
| 10 | MAJOR | A Codex restart whose request is larger than the old totals lost usage. | t3code `codexUsage.ts:242-252` | Rule 5.4: `C == L` after a non-zero baseline is a restart, checked before any subtraction (§3.4). Test `a_restart_larger_than_the_baseline_counts_the_whole_request`. |
| 11 | MAJOR | Codex keys collided across counter resets. | design §3.4 item 6 (revision 1) | `codex:resp:<response_id>` when present; else the key holds a replay-stable counter epoch (`CodexState.epoch_ms`) (§3.4). Test `keys_differ_across_counter_epochs`. |
| 12 | MAJOR | Codex grandchild usage lost its root; `payload.parent_thread_id` was not read. | `history/trust.rs:494-502`, `history/import_check.rs:213` | Both encodings (§3.4 step 2); `analytics_threads` filled before any usage of a pass (§3.5 step 3); the root by the ancestry, at most 16 levels, a missing parent is the root, a cycle keeps the own thread (§3.5 step 4). Tests `codex_root_follows_the_ancestry`, `a_missing_parent_is_the_root`, `an_ancestry_cycle_keeps_the_own_thread`, `codex_metas_are_read_before_usage`. |
| 13 | MAJOR | `trusted_home` fails on macOS, so every Mac home was unavailable. | `history/trust.rs:187-197`, `history/browse.rs:110-150` | The browse's policy through `resolve_plan`: `Untrusted` is not read, `Unverifiable` is read and reported `verified: false` (§3.5 step 1, §4 item 5, decision 17); a live check on mac-m4 at the merge. |
| 14 | MAJOR | The first station by name dropped rows that only the other station held. | design §3.10 (revision 1) | A conversation key on every row; the board keeps the row with the most requests per `(source key, hour, provider, model, conversation)` (§3.2, §3.10, decision 18). Test `merge_keeps_the_largest_row_of_a_shared_home`. |
| 15 | MAJOR | `cost_per_turn` treated missing usage as complete. | design §3.12 (revision 1) | The learning needs complete sources, one period from the latest coverage start, every session with ended turns to have attributed usage, no unpriced usage and 20 turns; else it is not shown (§3.12). Tests `cost_per_turn_is_hidden_when_a_session_with_turns_has_no_usage`, `cost_per_turn_is_hidden_while_a_source_is_not_complete`. |
| 16 | MAJOR | The 400-day cutoff broke the owner's rule "the metrics go as far as the data goes". | `docs/v2/BACKLOG.md:407` | No age limit on the ingestion and on the query window, keys kept forever (about 20 MB a year), period `All time` (§2 Q1, §3.2, §3.3, §3.5 step 4, §3.10, decision 3). `skipped_old` and `MAX_WINDOW_MS` are gone. |
| 17 | MINOR | A rolling window included up to an hour before its start. | design §3.10 (revision 1) | `since` is an hour start (`check` refuses another value); the board uses `floor_hour(now - period)` and says the exact start (§3.2, §3.10). Test `the_window_starts_at_an_hour`. |

The build plan has ten tasks now (A6 of revision 1 is split into the scanner A6 and the query and intake A7); the deck route is A8, the board tasks A9 and A10.
