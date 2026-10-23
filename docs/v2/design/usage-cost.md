# Token counts and cost: a correct source, the resume baseline and a recount (B14, B20, B21)

- **Status:** Draft, revision 1 (2026-10-23). Design agent of the backlog program. Needs an adversary DESIGN REVIEW before the build (risk high, §8).
- **Items:** B14 (token counts and cost not correct), B21 (the first turn after a Claude resume is not counted), B20 (stored cost totals too high: repair or reset).
- **Base:** `v2` at `497b2fbc`. Every `file:line` below is on that commit.
- **Recordings:** `~/fleetdeck-briefs/backlog/usage-cost/` (`claude/`, `codex/`, `comparison.txt`). Only the usage frames are kept; prompts, e-mail addresses and account data are removed. The drivers that made them are in `scratch/` (`drive_claude.py`, `drive_codex.py`, `extract.py`).

## 1. Problem and verified facts

The board shows a token count and a cost for each session and for the fleet. The owner reports that both are wrong (B14), that the stored totals are too high (B20), and that the first turn after a Claude resume adds no cost (B21).

### 1.1 How the total is made today

1. A session total is a sum. The fold adds each `usage.updated` event to `SessionSummary.usage` (`crates/fleetdeck-proto/src/fold.rs:154-156`, `add_usage` at `fold.rs:366-379`). Only `input_tokens`, `output_tokens`, `cost_usd` and `cost_inferred` exist (`crates/fleetdeck-proto/src/event_types.rs:198-208`). So each event must carry a delta.
2. The station and the deck run the same fold. The station folds in the event transaction into `sessions.summary` (`crates/fleetdeck-station/src/store/migrations.rs:37-52`). The deck folds the events that it receives into its own `sessions.summary` (`crates/fleetdeck-deck/src/store/read_model.rs:214-220`), and a snapshot replaces the deck rows with the station rows (`read_model.rs:284-293`).
3. The station keeps its events for 7 days after a deck acknowledged them (`EVENT_RETENTION_MS`, `crates/fleetdeck-station/src/store/mod.rs:143`; prune at `store/log.rs:352-395`). The deck keeps at most 500 timeline rows per session (`TIMELINE_CAP`, `read_model.rs:31`).

### 1.2 Claude, as the code does it on `497b2fbc`

1. One `usage.updated` per `result` frame (`crates/fleetdeck-providers/src/claude/map.rs:547-581`). The tokens are `result.usage.input_tokens` and `result.usage.output_tokens` (`map.rs:513-522`).
2. **Correction of the backlog:** the cost is no longer `total_cost_usd`. Commit `0e6c3600` (2026-10-23) made it "the growth of the process total": `cost_part` (`map.rs:532-542`) with the state `ClaudeCore.last_total_cost` (`crates/fleetdeck-providers/src/claude/core.rs:218-222`). The rules: the first result of a new process adds its whole total; the first result of a resumed process adds 0; a later result adds the growth; a total under the last one is "all new".
3. The rule "the first result of a resumed process adds 0" depends on the claim in the comment at `map.rs:527-529`: "the CLI sends a zero-usage `result` with that total right after the restart". **This claim is false** (§1.4, items 3 and 4).
4. `config.resumed` is not part of the checkpoint (`core.rs:166-168`). A shim session that the station restores after a station restart gets its config from `adapter.durable_launch(&spec)` (`crates/fleetdeck-station/src/manager/shim_configs.rs:9-26`), which sets `resumed: None` (`claude/checkpoint.rs:23-30` for the placeholder, `claude/durable_launch.rs:55-60`). So a resumed process whose station restarts before its first result adds its whole restored total (a second error of the same rule).

### 1.3 Codex, as the code does it on `497b2fbc`

1. One `usage.updated` per `thread/tokenUsage/updated` of the session's own thread, from the `last` block (`crates/fleetdeck-providers/src/codex/map.rs:361-367`). The update of another thread (a sub-agent) is dropped (`map.rs:368`).
2. The cost is inferred from a local table (`crates/fleetdeck-providers/src/codex/prices.rs:23-39`, `usage_of` at `prices.rs:58-74`). `input_tokens` is Codex `inputTokens`, which contains the cached input. A model that is not in the table has no cost.

### 1.4 Live recordings (2026-10-23, `claude` 2.1.308, `codex` 0.163.0)

Claude ran with `-p --input-format stream-json --output-format stream-json --verbose --replay-user-messages --model haiku` in a scratch folder. Codex ran through `codex app-server` with the requests of `codex/core.rs:299-346`.

1. **Claude, one process, 3 prompts** (`claude/claude-run1.usage.json`). Prompt 2 started a subagent with the Agent tool; the CLI ran it in the background, and its notification started a fourth turn by itself.
   - `result.usage` is the usage of the main thread in that turn only. It has `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens`.
   - `result.modelUsage` is the running total of the process, per model: `inputTokens`, `outputTokens`, `cacheReadInputTokens`, `cacheCreationInputTokens`, `costUSD`, and `costBasis: "list"`. It contains the subagent: after the subagent ended, the next `modelUsage` grew by the subagent's 2 input, 4 output and 9,625 cache-write tokens, which no `result.usage` contains.
   - `total_cost_usd` is the sum of `modelUsage.*.costUSD`.
2. **Claude transcript.** At each clean exit (stdin closed, or `SIGTERM`) the CLI writes one `{"type":"cost-state", "sessionId", "totalCostUSD", "modelUsage", ...}` record to the transcript `<home>/projects/<cwd with every non-alphanumeric character as "-">/<native id>.jsonl` (`claude/transcript-cost-states.jsonl`). A `SIGKILL` writes none (`claude-run4-killed`, `claude-run6-term`).
3. **Claude `--resume`** (`claude-run2-resume`, `claude-run3-resume-idle`). The resumed process restores the totals of the last `cost-state` record and counts on from them: its first result reports `0.006837855` after the earlier final total `0.006649605`. There is **no** zero-usage result after the restart, also when the process waits 12 s before the first prompt. The recorded fixture `spikes/s1-claude/fixtures/rust/provider_thread_resume.jsonl` (CLI 2.1.289) shows the same: the first result of process 2 reports `0.0115062` after `0.00758225`.
4. **Claude resume after a `SIGKILL`** (`claude-run5-after-kill`). The killed process reported `0.007534915`; it wrote no `cost-state`. The next resume restored the older `0.00734601` and reported `0.00753496` for its first result. A session without any `cost-state` record restores nothing and starts at 0 (`claude/no-cost-state-resume.txt`).
5. **Claude `--fork-session`** (`claude-run7-fork`). The fork restores the parent's last `cost-state` and counts on from it (`0.007921785` after `0.00773005`), under the new session id.
6. **Codex, one thread, 3 turns** (`codex/codex-run1.usage.json`). Each `thread/tokenUsage/updated` has `last` (the last model call) and `total` (the thread). Both have `totalTokens`, `inputTokens`, `cachedInputTokens`, `cacheWriteInputTokens`, `outputTokens`, `reasoningOutputTokens`. `inputTokens` contains `cachedInputTokens`; `totalTokens = inputTokens + outputTokens`. The model was `gpt-daybreak-blue-latest`, which is not in the price table.
7. **Codex `thread/resume`** (`codex-run2-resume`). Right after the resume response, before any `turn/started`, the app-server sends one update with the **old** turn id, the old `last` and the restored `total`. Then `total` counts on. The recorded fixtures show the same for a resume (`spikes/s2-codex/fixtures/provider_thread_resume.jsonl`, lines 45-54) and for a fork, where the new thread id gets the parent's `total` (`fork_thread.jsonl`, lines 44-53).
8. **Codex sub-agent** (`codex-run3-subagent`). The sub-agent thread sends its own updates with its own `total` (15,069 tokens). The parent `total` does not contain them. The fixture `spikes/s2-codex/fixtures/subagent.jsonl` (line 51) shows the same.
9. **Codex `account/usage/read { threadId }`** answered `threadUsage: null` for the owner's account (no billing route). The schema has `estimatedUsageUsdMicros` for other accounts.
10. The Codex rollout file (`~/.codex/sessions/.../rollout-*-<thread>.jsonl`) has one `event_msg` / `token_count` record per turn with `total_token_usage` in snake case. Its last total (73,793 input, 25 output) equals the last live `total`. The resume replay of item 7 is not written to it.

### 1.5 The rules compared on the recordings

`~/fleetdeck-briefs/backlog/usage-cost/comparison.txt` has the numbers. The provider truth is the last cumulative `modelUsage` (Claude) and the last `total` (Codex).

| Case | Truth | `7333db5d` | `497b2fbc` (HEAD) | This design |
| --- | --- | --- | --- | --- |
| Claude cost, run 1 + resume (6 results) | $0.007154475 | $0.034214795 (4.8x) | $0.006966225 (the first resumed turn lost) | $0.007154475 |
| Claude input tokens | 26 new, 85,425 cache read, 49,629 cache write | 24, no cache | 24, no cache | the same as the truth |
| Claude output tokens | 188 | 184 (no subagent) | 184 | 188 |
| Codex tokens, run 1 + resume | 73,818 | 88,880 (the replay counted again) | 88,880 | 73,818 |
| Codex tokens with a sub-agent | 42,276 | 27,207 (sub-agent dropped) | 27,207 | 42,276 |

### 1.6 Corrections of the backlog text

1. B14 "Claude: `cost_usd = result.total_cost_usd`": out of date since `0e6c3600` (§1.2.2).
2. B14 suspect 1 (running total): true. The total is the running total of the process **and** of the restored transcript record, so it also continues across a resume and a fork.
3. B14 suspect 2 (cache tokens): true. Claude cache reads and writes are not read at all.
4. B14 suspect 3 (subagents): for Claude, HEAD counts the subagent cost (through the growth of the total) but not its tokens. For Codex, HEAD drops the sub-agent tokens and cost.
5. B14, Codex: two more errors. Every resume and fork counts the last turn again (§1.4.7). The owner's model `gpt-daybreak-blue-latest` has no price, so the owner's Codex sessions show no cost.
6. B21 "suspected cause": on HEAD the cause is the rule of `0e6c3600`, "a resumed process adds 0 for its first result" (`map.rs:535`), built on a false claim (§1.2.3). B21 is dated 2026-10-22, before `0e6c3600`; on `7333db5d` a resumed first turn added the whole restored total (too high). The symptom that the owner describes is the behavior of HEAD.
7. B21 "owner proposal" (give the stored cost to the resumed process as its start): not possible and not needed. The CLI has no option for a start value. It already restores its own total from the transcript, and a station-local baseline (§3.3) gives the exact delta.
8. B20 "recompute from the stored events": not possible in general. The station deletes events after 7 days, the deck keeps 500 rows per session, the old events have no cache tokens and no per-model totals, and the meaning of their `cost_usd` changed at `0e6c3600` without a marker.

## 2. Answers to the questions of the items

### B14

1. *Record live fixtures and compare with the provider numbers.* Done (§1.4, §1.5). Claude's own number is the cumulative `modelUsage` and `total_cost_usd` of a `result`, and the `cost-state` record of the transcript. Codex's own number is `tokenUsage.total` and the rollout `token_count`. Claude `/cost` shows the same `cost-state` values; it was not run separately.
2. *Use the provider's running totals and store the last value, instead of deltas?* Use the running totals as the **source**, but keep **deltas** in the events (DL-440). The adapter remembers the last running total per process (Claude) or per thread (Codex) and emits the growth. Reasons: the fold, the station and deck stores and every older deck stay correct; a `SIGKILL` makes Claude restore an older total (§1.4.4), so "the last value per conversation" would lose a killed process's usage; a fork starts from the parent's total, so "the last value" would count the parent twice in the fleet total.
3. *Show cache tokens separately and use them in the cost.* Yes. `Usage` gets `cache_read_tokens` and `cache_write_tokens` (§3.1). `input_tokens` means "new input" (not read from the cache and not written to it) for both providers (DL-441). Claude's `costUSD` already contains the cache prices. The Codex price table gets a cache-read and a cache-write rate.
4. *One price table, its date, "estimate" on a subscription.* One module `crates/fleetdeck-providers/src/prices.rs` with `PRICE_TABLE_VERSION` (§3.5). It prices only what no provider prices: today Codex. Claude's cost stays the CLI's own (`costBasis: "list"`). An inferred cost carries the table date to the board (`Usage.prices`). Every cost is already labeled "at API prices", with the title "A subscription does not bill it" (`web/src/board/usage.ts:72-83`); that is the "estimate" text, and it stays (DL-446).

### B20

1. *Recompute from the stored events (a migration) or reset and count from the fix date?* Neither. The events are gone or carry the wrong data (§1.6.8), and a reset to zero is false. The design **recounts from the provider's own record** on the station: the last `cost-state` of the Claude transcript, or the last `token_count` of the Codex rollout. A new event `usage.set` replaces the stored total; both folds apply it (§4).
2. *Show the reset in the audit log.* Yes. The recount is an owner command `session.usage_recount` through the generic command route, which writes one audit row per session (§4.4). The event keeps the previous total (`previous`), so the timeline keeps the old number.
3. *Which sessions?* Claude and Codex sessions without a live process, that the owner recounts from the Usage tab. A running session keeps its total and is listed as "runs now". Sessions of other providers are not offered.

### B21

1. *Pass the recorded cost to the resumed process?* No (§1.6.7). The station reads the totals that the CLI will restore (the last `cost-state` of the transcript) before the spawn and uses them as the baseline of the new process (§3.3).
2. *Depends on B14.* Yes: the same per-model running-total rule handles a new process, a resume, a fork and a station restart.

## 3. Design: live counting (B14, B21)

### 3.1 Data shapes (protocol)

`crates/fleetdeck-proto/src/event_types.rs`, `Usage` (`event_types.rs:198-208`) gets three fields. All are optional, so an older peer reads and writes the old shape:

```rust
/// Token usage. `None` means unknown, which is not zero.
pub struct Usage {
    /// Input tokens that the prompt cache did not hold and the turn did not
    /// write to it ("new input"), for both providers (DL-441).
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub cost_inferred: bool,
    /// Input tokens read from the prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to the prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cache_write_tokens: Option<u64>,
    /// The `PRICE_TABLE_VERSION` of an inferred cost; `None` for a cost that
    /// the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prices: Option<String>,
}
```

TypeScript (`web/src/gen/event/Usage.ts`, generated): `cache_read_tokens?: number, cache_write_tokens?: number, prices?: string`.

`add_usage` (`fold.rs:366-379`) adds `cache_read_tokens` and `cache_write_tokens` with the same `add` rule, and sets `total.prices = more.prices.clone()` when `more.prices.is_some()` (the latest table wins).

### 3.2 Claude: per-model running totals

New module `crates/fleetdeck-providers/src/claude/process_usage.rs`:

```rust
/// The running totals of one model in one CLI process (`result.modelUsage.<model>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelTotals {
    pub input: u64,        // inputTokens
    pub output: u64,       // outputTokens
    pub cache_read: u64,   // cacheReadInputTokens
    pub cache_write: u64,  // cacheCreationInputTokens
    pub cost_usd: f64,     // costUSD
}

/// The running totals of a process, by model id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessTotals {
    pub models: BTreeMap<String, ModelTotals>,
}

/// What the CLI counts from when the process starts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum UsageBase {
    /// A new conversation: the CLI counts from zero.
    #[default]
    Zero,
    /// A resume or a fork: the totals that the CLI restores (the last
    /// `cost-state` record of the transcript; empty when it has none).
    Restored(ProcessTotals),
    /// A resume or a fork whose transcript the adapter could not read.
    Unknown,
}

/// The usage state of a core; it is part of the checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessUsage {
    /// The totals of the last counted `result`, or the base before the first one.
    /// `None`: the base is unknown and no result came yet.
    pub last: Option<ProcessTotals>,
}

impl ProcessTotals {
    /// `result.modelUsage`; `None` when the frame has no object there.
    pub fn from_model_usage(v: &Value) -> Option<Self>;
    /// A `cost-state` record of a transcript (`modelUsage`, same keys).
    pub fn from_cost_state(v: &Value) -> Option<Self>;
    /// The sum over the models, as one `Usage` (cost not inferred).
    pub fn usage(&self) -> Usage;
}

impl ProcessUsage {
    pub fn new(base: &UsageBase) -> Self; // Zero → Some(empty); Restored(t) → Some(t); Unknown → None
    /// The delta of one `result`, and the new state.
    pub fn delta(&mut self, res: &ResultFrame, legacy_last_cost: Option<f64>) -> Usage;
}
```

`delta` rules (exact):

1. `current = ProcessTotals::from_model_usage(res.body["modelUsage"])`.
2. If `current` is `Some` and `self.last` is `Some(last)`: for each model in `current`, `d = current[m] − last[m]` per field (a model that is not in `last` counts from zero). If any field of a model is lower than in `last`, that model's count started again: `d = current[m]`. The event is the sum of all `d`, with `cost_inferred: false`, `prices: None`. Then `self.last = Some(current)`.
3. If `current` is `Some` and `self.last` is `None` (base unknown, first result): tokens from `result.usage` of this turn (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`); `cost_usd`: if `legacy_last_cost` is `Some(c)` and `total_cost_usd ≥ c` then `total_cost_usd − c`, else `None`. Then `self.last = Some(current)`.
4. If `current` is `None` (a CLI without `modelUsage`): tokens from `result.usage` of this turn; cost: the growth of `total_cost_usd` over the cost sum of `self.last` (the `0e6c3600` growth rule without the "resumed adds 0" case); `self.last` keeps its models and gets no new values. Log `debug!("claude result without modelUsage")`.
5. A delta whose every part is 0 is still emitted (the turn completed; the fold adds nothing).

`ClaudeCore` (`core.rs:165-223`): add `#[serde(default)] pub(super) usage: ProcessUsage`. Keep `last_total_cost` (it is read from a checkpoint of an older station: `legacy_last_cost` of rule 3) and stop writing it. `ClaudeCore::new` sets `usage = ProcessUsage::new(&config.usage_base)`.

`CoreConfig` (`core.rs:40-48`) gets `pub usage_base: UsageBase`. Every literal of `claude::CoreConfig` adds `usage_base: UsageBase::Zero` (the build task lists them; `grep -rn "claude::CoreConfig {\|CoreConfig {" crates`). The placeholder of `checkpoint.rs:23-30` uses `Zero`; it is not used for counting because the restored `usage` state wins.

`map.rs`: `on_result` (`map.rs:547-581`) calls `core.usage.delta(res, core.last_total_cost)`; `usage_of` and `cost_part` (`map.rs:513-542`) are removed. The `usage.updated` and the `turn.completed` usage carry the same delta.

### 3.3 Claude: the resume baseline (B21)

New module `crates/fleetdeck-providers/src/claude/cost_state.rs`:

```rust
/// The directory name of `cwd` under `<home>/projects` (every character that
/// is not ASCII alphanumeric becomes `-`).
pub fn project_dir_name(cwd: &str) -> String;

/// The last `cost-state` record of `native` in an open transcript, read from
/// the end in 64 KiB blocks, at most `max_scan` bytes.
/// `Ok(None)`: the scanned part has no record of that session.
pub fn last_cost_state<R: Read + Seek>(file: &mut R, native: &str, max_scan: u64)
    -> io::Result<Option<ProcessTotals>>;

/// The base of a resume or fork of `native` in `cwd` (§3.3 rules).
pub fn restored_base(env: &ChildEnv, cwd: &Path, native: &str) -> UsageBase;
```

Rules of `restored_base`:

1. Home: `CLAUDE_CONFIG_DIR` of `env` when set and not empty, else `HOME/.claude` (the same rule as `crates/fleetdeck-station/src/history/locate.rs:51-61`). A home that is not absolute: `Unknown`.
2. File: `<home>/projects/<project_dir_name(cwd)>/<native>.jsonl`, opened with `O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK` (rustix). It must be a regular file owned by the effective uid with one link; else `Unknown`. A missing file: `Unknown` (the CLI cannot resume it either).
3. Scan: `last_cost_state(file, native, 32 MiB)`. A line is a candidate when it contains `"cost-state"`; it counts when it parses as JSON with `type == "cost-state"` and `sessionId == native`. Found: `Restored(totals)`. The whole file scanned and no record: `Restored(ProcessTotals::default())` (the CLI starts at 0, §1.4.4). The limit reached without a record: `Unknown`. An I/O error: `Unknown`. The scan has a deadline of 2 s; past it: `Unknown`.
4. `native` must pass the existing native id check (UUID form, `claude/args.rs` `is_uuid`); else `Unknown`.

Where it runs:

1. Direct driver `claude/driver.rs:203-208`: for `Origin::Resume(id)` and `Origin::Fork(id)`, `usage_base: cost_state::restored_base(&env, &cwd, &id.0)`, inside `tokio::task::spawn_blocking`; a join error is `Unknown`. For `Origin::New`: `Zero`.
2. Durable launch `claude/durable_launch.rs:24-61`: when `resume` is `Some(id)`, `usage_base: cost_state::restored_base(&env, &cwd, &id.0)` (the call is synchronous; its deadline is 2 s). Else `Zero`.
3. The env is the one that the process gets (`env_with_extra(&self.config.env, spec.extra_env.as_deref())`), so a provider instance with its own `CLAUDE_CONFIG_DIR` reads its own home.

Why this is exact: the resumed process restores exactly the last `cost-state` (§1.4.3, §1.4.4, §1.4.5), so `first total − restored` is the usage of this process. The station cannot see usage that the CLI recorded after the last `result` of the previous process (a background agent that ended after the last result, before a clean exit): it is in the restored record and is not counted (§9, open item a).

Resume is refused while the previous execution is not proven ended (`crates/fleetdeck-station/src/manager/shim_target.rs:332-342`), so no other process writes a `cost-state` between the read and the spawn.

### 3.4 Codex: per-thread running totals

New module `crates/fleetdeck-providers/src/codex/thread_usage.rs`:

```rust
/// One `TokenUsageBreakdown` (app-server camelCase, rollout snake_case).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input: u64,        // inputTokens / input_tokens (contains the cached input)
    pub cached: u64,       // cachedInputTokens / cached_input_tokens
    pub cache_write: u64,  // cacheWriteInputTokens / cache_write_input_tokens (0 when absent)
    pub output: u64,       // outputTokens / output_tokens
}

impl TokenTotals {
    pub fn from_app_server(v: &Value) -> Option<Self>; // needs inputTokens and outputTokens
    pub fn from_rollout(v: &Value) -> Option<Self>;    // needs input_tokens and output_tokens
    /// The normalized usage of these tokens, with the cost of `model` (§3.5).
    pub fn usage(&self, model: Option<&str>) -> Usage;
}

/// The usage state of a core; part of the checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadUsage {
    /// The last counted `total` of each thread of this session (own and sub-agents).
    pub totals: BTreeMap<String, TokenTotals>,
    /// A `turn/started` of the own thread came in this process.
    pub turn_seen: bool,
}

impl ThreadUsage {
    /// The delta of one `thread/tokenUsage/updated`; `None`: nothing to emit.
    pub fn delta(&mut self, thread: &str, own: bool, p: &Value) -> Option<TokenTotals>;
}
```

Normalized usage (`TokenTotals::usage`): `input_tokens = input − cached − cache_write` (saturating), `cache_read_tokens = cached`, `cache_write_tokens = cache_write`, `output_tokens = output` (reasoning tokens are part of `output`). The cost and `prices` come from §3.5; `cost_inferred = cost.is_some()`.

`delta` rules (exact):

1. `total = TokenTotals::from_app_server(p["tokenUsage"]["total"])`. If it is `None`: return `from_app_server(p["tokenUsage"]["last"])` (the HEAD rule, for an older app-server).
2. No entry for `thread`, `own`, and `!turn_seen`: this is the replay of a resume or fork (§1.4.7). Store `total`; return `None`.
3. No entry otherwise (a new thread, a sub-agent): store `total`; return `Some(total)`.
4. An entry `prev` with every field of `total ≥ prev`: store; return `Some(total − prev)`, or `None` when the difference is all zero.
5. An entry with a lower field (the count started again): store; return `Some(total)`.

`CodexCore` (`codex/core.rs:35`) gets `#[serde(default)] pub(super) usage: ThreadUsage` (one field: `core.rs` has 782 lines). `on_turn_started` sets `self.usage.turn_seen = true`. In `codex/map.rs:361-368` the two arms become one: `"thread/tokenUsage/updated"` for any thread → `self.usage.delta(thread, self.own_thread(p), p)` → `Event::UsageUpdated { usage: delta.usage(self.thread_model.as_deref()), rate_limits: vec![] }`. A sub-agent's tokens are priced at the session model (DL-445). A sub-agent update never touches the turn state.

### 3.5 One price table

`crates/fleetdeck-providers/src/codex/prices.rs` moves to `crates/fleetdeck-providers/src/prices.rs` (`pub mod prices` in `lib.rs`):

```rust
/// The date of the rates below (R4.6, DL-161, DL-444).
pub const PRICE_TABLE_VERSION: &str = "2026-10-23";

/// USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price { pub input: f64, pub cache_read: f64, pub cache_write: f64, pub output: f64 }

/// The price of a model of a provider; `None` for a model that is not listed.
/// A trailing date (`-YYYY-MM-DD`) is removed before the match.
pub fn price_of(provider: ProviderKind, model: &str) -> Option<Price>;

/// The cost of new input, cache reads, cache writes and output.
pub fn cost(price: Price, new_input: u64, cache_read: u64, cache_write: u64, output: u64) -> f64;
```

Rows (all `ProviderKind::Codex`; no Claude rows, DL-444): `gpt-5`, `gpt-5-codex`, `gpt-5.1`, `gpt-5.1-codex`, `gpt-5.6-luna` → 1.25 / 0.125 / 1.25 / 10.0; `gpt-5-mini`, `gpt-5.1-codex-mini` → 0.25 / 0.025 / 0.25 / 2.0; `gpt-5-nano` → 0.05 / 0.005 / 0.05 / 0.4. These are the HEAD rates (`codex/prices.rs:27-31`); the cache-write rate is the input rate. `gpt-daybreak-blue-latest` has no row (owner question 1).

### 3.6 Board (live counting)

Files: `web/src/board/usage.ts`, `web/src/board/UsageMeters.tsx`, `web/src/board/UsageGlance.tsx`, `web/src/session/PropertiesPanel.tsx`, `web/src/compare/ComparePage.tsx`. Colors: tokens and cost stay `--fuel`. Numbers: `countText`, `compactCount`, `usdText` (DESIGN-SYSTEM §5).

1. `inputLabel` (`usage.ts:64-69`) returns `'new input'` for every fleet (the meaning is one now). `NEW_INPUT_TITLE` (`PropertiesPanel.tsx:203`) becomes: `Input that the prompt cache did not hold. Cache reads and cache writes are separate counts.`
2. `FleetUsage` gets `cacheRead: UsageTotal` and `cacheWrite: UsageTotal` (from `cache_read_tokens ?? null`, `cache_write_tokens ?? null`). The Usage tab (`UsageMeters.tsx:35-49`) shows four token stats in this order: "New input tokens", "Cache read tokens", "Cache write tokens", "Output tokens", then "At API prices". A stat whose total is `known === null` shows "unknown" (the existing `totalText`). The glance (`UsageGlance.tsx`) does not change its parts.
3. Properties, row "Tokens": `tokensText(input, output, inputWord)` becomes `tokensText(usage: Usage)` with four parts in this order: `<n> new input`, `<n> cache read`, `<n> cache write`, `<n> output`; a part whose count is `null` or absent is left out.
4. Properties, row "At API prices": the title of an inferred cost (`cost_inferred` and `prices` set) is `apiPricesTitle(1)` plus ` Fleet Deck inferred it from its price table of <prices>.`
5. Compare page (`ComparePage.tsx:191-196`): adds `<n> cache read` after `<n> in` when known.

## 4. Design: the recount (B20)

### 4.1 Protocol

`crates/fleetdeck-proto/src/event_types.rs`:

```rust
/// Where a recounted usage total came from (B20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum UsageSource {
    /// The last `cost-state` record of the Claude transcript.
    ClaudeTranscript,
    /// The last `token_count` record of the Codex rollout file.
    CodexRollout,
    /// The provider has no record of the session: the total is unknown.
    None,
}

/// The last recount of a session (B20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct UsageRecount {
    /// Unix ms of the `usage.set` event.
    pub at: i64,
    pub source: UsageSource,
}
```

`crates/fleetdeck-proto/src/event.rs`, a new variant (kind `usage.set`, added to `Event::KINDS` and `kind_name` in `event_kinds.rs`):

```rust
/// The station replaced the stored usage total of the session with the
/// provider's own record (B20). The fold replaces, it does not add.
#[serde(rename = "usage.set")]
UsageSet {
    usage: Usage,
    /// The total that the summary had before; kept for the timeline.
    previous: Usage,
    source: UsageSource,
},
```

Fold (`fold.rs`): `Event::UsageSet { usage, source, .. } => { self.usage = usage.clone(); self.usage_recount = Some(UsageRecount { at, source: *source }); }`. The rate limits do not change.

`crates/fleetdeck-proto/src/session.rs`, `SessionSummary` gets:

```rust
/// The last recount of `usage` from the provider record (B20). Absent before.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[ts(optional)]
pub usage_recount: Option<UsageRecount>,
```

`crates/fleetdeck-proto/src/command.rs`, a new `SessionCommand` variant, method `session.usage_recount`, with its `session_id`:

```rust
/// Replace the stored usage total of a session without a live process
/// with the provider's own record (B20). Owner action, scope `admin`.
UsageRecount { session_id: SessionId },
```

`crates/fleetdeck-proto/src/version.rs`: `pub const USAGE_RECOUNT: &str = "usage_recount.v1";` (the command and the `usage.set` event), in `ALL`, not in `REQUIRED`.

Board command map (`web/src/session/commands.ts:102-135`): `usage_recount: 'session.usage_recount'`.

### 4.2 Station

New module `crates/fleetdeck-station/src/manager/usage_recount.rs`, and a reader `crates/fleetdeck-station/src/history/usage_record.rs`.

1. **Prepare (outside the writer transaction),** in `SessionManager::submit`, next to `reconcile_check` (`manager/mod.rs:628`). For `UsageRecount { session_id }`:
   - Read the session row (provider, instance, cwd, native id, `session_seq`).
   - The provider must be Claude or Codex; else the decision refuses `unsupported_provider`.
   - No native id: the record is `Usage::default()` with `UsageSource::None`.
   - Else open the provider file with the trusted locator: `history::locate::locate(home, kind, cwd, native, trust)` (`history/locate.rs:87-97`, home from `history::env_for` and `locate::resolve_home`, the history `Limits` and a deadline of 5 s), in `spawn_blocking`.
     - Claude: `fleetdeck_providers::claude::cost_state::last_cost_state(file, native, u64::MAX)` (the whole file is allowed here, under the deadline). `Some(t)` → `t.usage()`, `ClaudeTranscript`. `None` → `Usage::default()`, `None`.
     - Codex: `usage_record::codex_total(file)`: read from the end for the last line with `"type":"event_msg"` and `payload.type == "token_count"`, take `payload.info.total_token_usage` → `TokenTotals::from_rollout`; the model is `payload.model` of the last `turn_context` line, else the session summary's model. `Some` → `totals.usage(model)`, `CodexRollout`. No `token_count` → `Usage::default()`, `None`.
     - `HistoryUnavailable::NotFound` → `Usage::default()`, `None`. Every other failure (not trusted, I/O, deadline) → the decision refuses `record_unreadable` with the reason word.
2. **Decide (in the writer transaction):**
   - Refuse `session_running` when the session has a live provider process (the manager's live map) or an undispatched `Start`, `Resume` or `Import` effect.
   - Refuse `session_changed` when the summary's `session_seq` is not the one read in step 1 (an event came between the read and the decision; the board may send the command again).
   - Else append `Event::UsageSet { usage, previous: summary.usage, source }`. The receipt is `Accepted` with `result(session_id, None)` (`manager/decide.rs:360-366`).
3. The direct decide (`decide.rs`) refuses the command like `StopExecution` (`decide.rs:349`): `SessionCommand::UsageRecount { .. } => Err(super::usage_recount::refuse_direct())`.
4. **Module size:** `manager/mod.rs` has 797 lines and `decide.rs` 794. The task moves the closure body `match command { … }` of `mod.rs:635-664` without change into `manager/submit_route.rs` (`pub(super) fn route(d, ctx: RouteCtx<'_>, command: SessionCommand) -> Result<…>`, `RouteCtx` holds `origin`, `policy`, `coordinator`, `shells`, `restarting`, `root`, `check`, `stop` and the new `recount`), and adds the `UsageRecount` arm there.
5. The station advertises `usage_recount.v1`.

Refusals are built as in `manager/stop_decide.rs:40-43`: `reject(<RejectCode>, "<code>: <message>")`. `unsupported_provider` uses `RejectCode::Unsupported`; the other three use `RejectCode::InvalidState`. The board shows the message after `Refused:` (`web/src/session/commands.ts:148-160`).

| Code | Message |
| --- | --- |
| `unsupported_provider` | `Only Claude and Codex sessions have a provider record.` |
| `session_running` | `This session runs now. It keeps its totals.` |
| `session_changed` | `The session changed during the recount. Recount it again.` |
| `record_unreadable` | `Fleet Deck could not read the provider record: <reason word>.` |

### 4.3 Deck

1. Methods table (`crates/fleetdeck-deck/src/http/methods_table_rest.rs`, after `session.release_shell` at line 516): `cmd("session.usage_recount", Scope::Admin),` with the comment `// The owner recounts a stored usage total (B20); owner login only, the station needs usage_recount.v1.`
2. Generic command route (`crates/fleetdeck-deck/src/http/command_routes.rs`, next to the `ReleaseShell` owner check at lines 321-337): `UsageRecount` from a caller that is not the owner's board login → 403 `owner_only` ("only the owner's board login recounts usage"); a station whose link has not negotiated `usage_recount.v1` (`board.store.link_feature(&rec.station_id, feature::USAGE_RECOUNT) != LinkFeature::Negotiated`) → 409 `not_supported` ("this station cannot recount usage").
3. The audit row is the generic one of the route (name `session.usage_recount`, target `<station>/<session>`, result).
4. No deck migration: the summary is JSON, and the event is stored in `timeline` as every event.

### 4.4 Board (recount)

New component `web/src/board/UsageRecount.tsx`, shown in the Usage tab under the stats of `UsageMeters.tsx`.

1. Eligible sessions: `provider` is `claude` or `codex`, and `summary.state !== 'running'`. Sessions with `state === 'running'` are counted as "runs now" and are not sent.
2. Look: one row, the text `Stored totals` (`.fd-label`) and a plain button `Recount from provider records`. No row when no session is Claude or Codex.
3. Confirm dialog (`.fd-dialog-head`, title `Recount the stored usage?`, description: `Fleet Deck reads the provider records on each station (Claude transcripts, Codex session files) and replaces the token and cost totals of every Claude and Codex session that does not run now. The timeline of each session keeps the old total.`). Buttons: `Recount` (`.fd-primary`) and `Cancel`.
4. Run: one `session.usage_recount` command per eligible session, at most 4 at a time, through `sessionActions(...).run` (`web/src/session/commands.ts:225`). While it runs the button shows `Loading` with `Recounting <done> of <n>`.
5. Result line (`role="status"`), the parts that are not 0, joined by a space:
   - `Recounted <n> sessions.` (accepted with source `claude_transcript` or `codex_rollout`; one session: `Recounted 1 session.`)
   - `<n> have no provider record: their totals are now unknown.` (accepted with source `none`; read from `summary.usage_recount.source` after the commands)
   - `<n> run now and keep their totals.`
   - `<n> stations need a newer Fleet Deck.` (409 `not_supported`, counted per station)
   - `<n> could not be recounted.` (every other refusal or failure; the first message follows in parentheses)
6. Properties: when `summary.usage_recount` is set, the "At API prices" row gets the aside `recounted` with the title `Recounted from the provider record on <date of at>.`; with source `none` the aside is `no record` with the title `The provider has no record of this session. The total is unknown since <date>.`

## 5. Errors and edge cases

1. **A station restart while a resumed Claude process runs under a shim.** The usage state (`ClaudeCore.usage.last`) is in the checkpoint, so the restored core counts on from the right base. The config's `resumed: None` does not matter any more (fixes §1.2.4).
2. **A checkpoint from a station before this change.** It has `last_total_cost` and no `usage`. `usage.last` is `None`, so the first result uses rule 3 of §3.2 (turn tokens; cost = growth over `last_total_cost`), then the per-model rule.
3. **A model switch in a process.** A new model key counts from zero (§3.2 rule 2).
4. **`/clear` or another reset of the CLI count.** A lower field means a new count; the model's whole current value is the delta.
5. **Claude interrupted or failed turns.** Their `result` carries `modelUsage`; the delta counts the tokens that the API billed.
6. **Workflows (ultracode, B12) and background agents.** They run in the same CLI process, so their usage is in `modelUsage` at the next `result`. Not recorded live in this draft (owner cost); the build verifies it once (task T3 acceptance item 4).
7. **A Codex sub-agent that a resumed parent knew before.** A replay of a sub-agent thread is not seen in the recordings. If one comes, it is counted once more (§9, open item b).
8. **Codex fork.** The new thread's first update before `turn/started` is the parent's total (§1.4.7): rule 2 of §3.4 counts nothing.
9. **Codex `total` absent.** The HEAD `last` rule (§3.4 rule 1).
10. **Recount of a forked or imported session.** The provider record contains the conversation before the fork or the import, so the recounted total contains it too (DL-447). Live counting after the recount adds only new usage.
11. **Recount while the owner resumes the session.** The resume creates a `Resume` effect and events: the decision refuses `session_running` or `session_changed`. The board shows the count.
12. **A paired deck that is older** sees `usage.set` as an unknown kind and keeps the old total (L16). Only a deck with the route sends the command, so only an owner with a second, older deck sees this (§9, open item c).
13. **The transcript is edited by an agent** (it can write files under `~/.claude` in a permissive mode). A forged `cost-state` changes only the shown numbers of that session (threat note T3, §8).

## 6. Compatibility

| Peer | Behavior |
| --- | --- |
| New station, older deck | `usage.updated` has optional new fields; an older deck ignores them (serde ignores unknown fields of a struct variant). An older deck never sends `session.usage_recount`. |
| Older station, new deck | No new fields (they stay unknown on the board). `session.usage_recount` → 409 `not_supported`; the board counts it under "need a newer Fleet Deck". |
| Older checkpoint, new station | §5.2. |
| Stored summaries | No SQL migration. A summary without `usage_recount` reads as before. Codex totals stored before this change have the old meaning of `input_tokens` (with the cached input) until a recount. |
| Web board, older deck | The new TS fields are optional; the board shows the parts that exist. |

## 7. Out of scope

1. Claude prices in the table (the CLI reports Claude's cost; DL-444).
2. `account/usage/read` thread estimates of Codex (null for the owner's account, §1.4.9).
3. The usage that a Claude process records after its last `result` (§3.3, open item a).
4. A plan type detector ("subscription" vs "API key"): the label "at API prices" and its title stay for every cost.
5. A chat line for `usage.set` (Properties shows the recount).
6. Fleet-wide history graphs of usage.
7. The `spikes/` adapters.

## 8. Risk class: high

Reasons: a repair of stored totals (`usage.set` replaces the stored summary value on the station and the deck), a new command path (`session.usage_recount`), a new event kind in the public schema, and the read of provider files. An adversary DESIGN REVIEW comes before the build, and a PRE-PR REVIEW before the merge.

Threat notes:

- **T1, path and file trust.** The recount opens the provider file only through the station's trusted locator (no-follow walk, owner uid, regular file, the history limits). The resume baseline in the providers crate opens one fixed path below the provider home with `O_NOFOLLOW`, checks the owner uid, the file type and the link count, and reads numbers only. Neither reads a path from the board.
- **T2, authority.** `session.usage_recount` is owner-only (board login, scope `admin`), checked by the deck and audited; the station accepts it only from a paired deck with origin `Board`, as `ReleaseShell`.
- **T3, forged records.** An agent that can write its own transcript can change the baseline or the recount of its own session. The effect is limited to the shown usage numbers of that session; no authority, file or process depends on them.
- **T4, data loss.** `usage.set` keeps the previous total in the event; the old `usage.updated` events stay until retention. A wrong recount is visible in the timeline and the owner can recount again. Nothing is deleted.
- **T5, races.** The recount decision rechecks the `session_seq` and the live map in the writer transaction (§4.2 step 2). A resume is refused until the previous execution is proven ended, so no `cost-state` is written between the baseline read and the spawn.
- **T6, denial of service.** The baseline scan has a 32 MiB cap and a 2 s deadline, the recount scan the history limits and 5 s; the board sends at most 4 recounts at a time.

## 9. Open items after the build

- a. The usage that Claude records after the last `result` of a process (a background agent stopped with the process) is in the next restored record and is never counted by the live rule; a recount includes it.
- b. A Codex sub-agent thread replayed after a parent resume would be counted again (not observed).
- c. A second, older deck keeps the old totals after a recount.
- d. Codex totals of stations that are not recounted keep the old input meaning.

## 10. Build plan

Every Rust command runs through the queue: `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. The guards run in every task: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. Every module stays under 800 lines.

Order: T1 → (T2 ∥ T3) → T4 → T5 ∥ T6 (T5 and T6 share no file).

### T1: protocol: usage fields, `usage.set`, `session.usage_recount` (backend, about 300 lines)

- **Owned files:** `crates/fleetdeck-proto/src/event_types.rs`, `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_kinds.rs`, `crates/fleetdeck-proto/src/fold.rs`, `crates/fleetdeck-proto/src/fold_usage_tests.rs` (new), `crates/fleetdeck-proto/src/session.rs`, `crates/fleetdeck-proto/src/command.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-proto/src/fold_tests.rs`, `crates/fleetdeck-proto/src/fold_tests_more.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `crates/fleetdeck-providers/src/claude/map.rs` (only the `Usage` literal: `..Usage::default()`), `crates/fleetdeck-providers/src/codex/prices.rs` (only the `Usage` literal), `crates/fleetdeck-station/src/manager/decide.rs` (one arm: `SessionCommand::UsageRecount { .. } => Err(super::usage_recount::refuse_direct())`), `crates/fleetdeck-station/src/manager/usage_recount.rs` (new; in T1 only `pub fn refuse_direct() -> DecideError` that returns `reject(RejectCode::InvalidState, "usage_recount_direct: The session manager decides a usage recount.")`), `crates/fleetdeck-station/src/manager/mod.rs` (only the `mod usage_recount;` line), `web/src/session/commands.ts` (one map entry), `web/src/gen/**` (generated).
- **Changes:** §3.1 (Usage fields, `add_usage`), §4.1 (UsageSource, UsageRecount, `Event::UsageSet` and its fold, `SessionSummary.usage_recount`, `SessionCommand::UsageRecount` with `method()` and `session_id()`, `feature::USAGE_RECOUNT` in `ALL`). Export the new types to TS. `fold.rs` gets `#[cfg(test)] #[path = "fold_usage_tests.rs"] mod usage_tests;` (`fold_tests.rs` has 798 lines: no new test there).
- **Tests (in `fold_usage_tests.rs`):**
  - `cache_parts_add_and_unknown_stays_unknown`: two events with cache parts `Some(3)` and `None` give `Some(3)`; two `None` give `None`.
  - `the_latest_price_table_wins`: `prices` `Some("a")` then `None` then `Some("b")` gives `Some("b")`.
  - `usage_set_replaces_the_total_and_marks_the_recount`: after two `usage.updated` and one `usage.set`, `usage` equals the set value and `usage_recount == Some({ at, source })`; the rate limits are unchanged.
  - `a_usage_set_event_round_trips_and_has_its_kind`: JSON round trip, kind `usage.set`, in `Event::KINDS`.
  - `an_old_usage_json_reads_with_no_cache_parts`: `{"input_tokens":1,"output_tokens":2,"cost_usd":0.1,"cost_inferred":false}` reads with the new fields `None` and writes back without them.
  - In `command.rs` tests (or a new `command_usage_tests.rs`): `usage_recount_has_its_method_and_session`.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast`; `cargo check -p fleetdeck-providers -p fleetdeck-station -p fleetdeck-deck`; the guards; `cd web && bun run typecheck`.

### T2: one price table and Codex per-thread totals (backend, about 450 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/prices.rs` (new, from `codex/prices.rs`), `crates/fleetdeck-providers/src/codex/prices.rs` (deleted), `crates/fleetdeck-providers/src/lib.rs`, `crates/fleetdeck-providers/src/codex/mod.rs`, `crates/fleetdeck-providers/src/codex/thread_usage.rs` (new), `crates/fleetdeck-providers/src/codex/tests_usage.rs` (new), `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/tests/codex_core_replay.rs`, `crates/fleetdeck-providers/tests/codex_driver_replay.rs`, `web/src/testing/fixtures/codex-*.events.json` (regenerated with `FD_UPDATE_FIXTURES=1`).
- **Changes:** §3.4 and §3.5.
- **Tests:**
  - `prices.rs`: `a_listed_model_costs_its_four_parts` (exact sum for 1,000,000 of each part of `gpt-5-codex`); `a_dated_model_id_matches_its_row` (`gpt-5-2025-08-07`); `an_unlisted_model_has_no_price` (`gpt-daybreak-blue-latest`, a Claude id).
  - `tests_usage.rs`: `the_total_grows_by_the_turn` (the three updates of `codex-run1` give deltas whose `totalTokens` sum is 43,625); `a_resume_replay_counts_nothing` (an update before `turn/started` after `thread/resume` adds nothing; the next two add 15,085 and 15,108); `a_fork_replay_counts_nothing` (the fixture shape of `fork_thread.jsonl` lines 44-58); `a_sub_agent_thread_is_counted` (the `codex-run3-subagent` updates give 42,276 tokens, the sub-agent never ends the turn); `a_lower_total_is_a_new_count`; `without_total_the_last_block_counts`; `input_is_new_input` (13,519 input with 11,264 cached → `input_tokens 2,255`, `cache_read_tokens 11,264`); `the_usage_state_survives_a_checkpoint`.
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/codex/) | test(/prices/) | binary(codex_core_replay) | binary(codex_driver_replay)'`; the guards.

### T3: Claude per-model totals and the resume baseline (backend, about 550 lines)

- **Depends on:** T1. Runs in parallel with T2.
- **Owned files:** `crates/fleetdeck-providers/src/claude/process_usage.rs` (new), `crates/fleetdeck-providers/src/claude/cost_state.rs` (new), `crates/fleetdeck-providers/src/claude/tests_process_usage.rs` (new), `crates/fleetdeck-providers/src/claude/mod.rs`, `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/checkpoint.rs`, `crates/fleetdeck-providers/src/claude/driver.rs`, `crates/fleetdeck-providers/src/claude/durable_launch.rs`, `crates/fleetdeck-providers/src/claude/tests.rs`, `crates/fleetdeck-providers/src/claude/tests_usage.rs`, every other file with a `claude::CoreConfig` literal (only the added `usage_base: UsageBase::Zero` line: `crates/fleetdeck-providers/tests/*.rs`, `crates/fleetdeck-station/src/manager/*` test files, `crates/fleetdeck-cli/tests/shim_*.rs`), `web/src/testing/fixtures/claude-*.events.json` (regenerated).
- **Changes:** §3.2 and §3.3. The two tests of `0e6c3600` in `tests_usage.rs:159-202` are replaced by the tests below (the rule "a resumed process adds 0" is wrong).
- **Tests (`tests_process_usage.rs`, frames from `~/fleetdeck-briefs/backlog/usage-cost/claude/`):**
  - `one_process_adds_the_growth_per_model`: the four results of `claude-run1` sum to its last `modelUsage` (18 input, 178 output, 49,743 cache read, 48,491 cache write, $0.006649605).
  - `a_subagent_appears_in_the_next_result`: result 3 adds 2 input and 4 output more than its `result.usage`.
  - `a_resume_counts_from_the_restored_record`: base `Restored(cost-state 1)`, the two results of `claude-run2-resume` add $0.00018825 and $0.00031662 (sum with run 1: $0.007154475).
  - `a_resume_after_a_kill_counts_from_the_older_record`: base `Restored(cost-state 3)`, the first result of `claude-run5-after-kill` adds $0.00018895.
  - `an_unknown_base_counts_the_turn_tokens_and_no_cost`: base `Unknown`, the first result gives the `result.usage` tokens and `cost_usd: None`; the second adds the growth.
  - `a_legacy_checkpoint_counts_the_cost_growth`: a checkpoint JSON with `last_total_cost: 0.5` and no `usage`; a result with total 0.8 adds 0.3.
  - `a_new_model_counts_from_zero` and `a_lower_model_total_is_a_new_count`.
  - `the_usage_state_survives_a_checkpoint`.
  - `cost_state.rs`: `the_last_record_of_the_session_wins` (two records and a record of another session id); `no_record_is_a_zero_base`; `a_missing_file_is_unknown`; `a_link_is_unknown` (a symlink in place of the file); `a_record_past_the_limit_is_unknown` (`max_scan` 1 KiB); `project_dir_name_replaces_every_non_alphanumeric_character`.
  - `durable_launch.rs`: `a_durable_resume_reads_the_restored_base` (a temp `CLAUDE_CONFIG_DIR` with a transcript) and `a_new_durable_launch_has_a_zero_base`.
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/claude/) | binary(claude_core_replay) | binary(claude_driver_replay)'`; `cargo check -p fleetdeck-station -p fleetdeck-cli --tests`; the guards; live check (architect, once): a Fleet Deck Claude session on Lighting with one turn, Stop, Resume, one turn: the Properties cost equals the last `cost-state` `totalCostUSD` of its transcript.

### T4: station recount (backend, about 500 lines)

- **Depends on:** T1, T2, T3.
- **Owned files:** `crates/fleetdeck-station/src/manager/usage_recount.rs` (new), `crates/fleetdeck-station/src/manager/usage_recount_tests.rs` (new), `crates/fleetdeck-station/src/manager/submit_route.rs` (new), `crates/fleetdeck-station/src/manager/mod.rs`, `crates/fleetdeck-station/src/manager/decide.rs`, `crates/fleetdeck-station/src/history/usage_record.rs` (new), `crates/fleetdeck-station/src/history/mod.rs` (the `mod` line), the station feature list file (where `feature::SHELL_COMMAND` is advertised).
- **Changes:** §4.2.
- **Tests (`usage_recount_tests.rs`, `history/usage_record.rs` tests):**
  - `a_claude_session_takes_the_transcript_total` (temp home, a transcript with two `cost-state` records → `usage.set` with the last, source `claude_transcript`, `previous` = the old summary usage).
  - `a_codex_session_takes_the_rollout_total` (the snake-case `token_count` of §1.4.10 → normalized usage, the model of the last `turn_context`, cost `None` for `gpt-daybreak-blue-latest`).
  - `no_record_sets_an_unknown_total` (no file → `Usage::default()`, source `none`).
  - `an_untrusted_file_is_refused` (a symlink → `record_unreadable`, no event).
  - `a_live_session_is_refused` (`session_running`, no event).
  - `a_session_that_changed_is_refused` (an event appended between prepare and decide → `session_changed`).
  - `another_provider_is_refused` (`unsupported_provider`).
  - `the_direct_decide_refuses_a_recount`.
  - `the_station_advertises_usage_recount`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(/usage_recount/) | test(/usage_record/) | test(/decide/)'`; the guards.

### T5: deck route rules (backend, about 120 lines)

- **Depends on:** T1 (T4 for the end-to-end test).
- **Owned files:** `crates/fleetdeck-deck/src/http/methods_table_rest.rs`, `crates/fleetdeck-deck/src/http/command_routes.rs`, `crates/fleetdeck-deck/tests/board_usage_recount.rs` (new).
- **Changes:** §4.3.
- **Tests (`board_usage_recount.rs`):** `a_token_cannot_recount` (403 `owner_only`, audit `refused`); `a_station_without_the_feature_answers_not_supported` (409); `the_owner_recount_is_audited_and_folded` (a fake station that answers with a `usage.set` event; the deck summary has the new usage and `usage_recount`; one audit row `session.usage_recount` `ok`).
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_usage_recount) | test(/methods/)'`; the guards.

### T6: board (ux, about 450 lines)

- **Depends on:** T1 (types), T5 (route behavior). Load the `fleet-design-system` skill first.
- **Owned files:** `web/src/board/usage.ts`, `web/src/board/UsageMeters.tsx`, `web/src/board/UsageRecount.tsx` (new), `web/src/board/usageRecount.test.tsx` (new), `web/src/board/usage.test.ts`, `web/src/session/PropertiesPanel.tsx`, `web/src/session/properties.test.tsx`, `web/src/compare/ComparePage.tsx`, `web/src/styles/home.css` (only if a layout rule is needed; tokens only).
- **Changes:** §3.6 and §4.4, with the exact texts there.
- **Tests:**
  - `usage.test.ts`: `fleet_usage_sums_cache_parts`; `the_input_label_is_new_input_for_every_provider`; `an_inferred_cost_title_names_the_price_table`.
  - `properties.test.tsx`: `the_tokens_row_has_four_parts_in_order`; `a_missing_part_is_left_out`; `a_recounted_session_shows_recounted`; `a_session_without_record_shows_no_record`.
  - `usageRecount.test.tsx`: `the_button_is_absent_without_claude_or_codex_sessions`; `cancel_sends_nothing`; `recount_sends_one_command_per_stopped_session_and_skips_running_ones`; `the_result_line_counts_each_outcome` (accepted with source, `none`, running, 409, refused); `at_most_four_commands_run_at_once`.
- **Acceptance:** `cd web && bun run test src/board src/session/properties src/compare` (through `heavy.sh`); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; the guards; gallery shots of the Usage tab and Properties in both themes, desktop and phone.

## 11. Decisions made in this draft

1. **Deltas stay; running totals are the source** (DL-440). Rejected: store the last running total per process or thread in the summary (changes the fold contract for older decks, loses a killed process, counts a fork's parent twice).
2. **`input_tokens` means new input for both providers** (DL-441). Rejected: keep each provider's meaning and the `inputLabel` switch (the same word meant two counts).
3. **Claude source is `result.modelUsage` per model** (DL-442). Rejected: `result.usage` per turn (no subagents, no cost); `total_cost_usd` alone (no tokens per kind).
4. **The Claude resume baseline is the last `cost-state` of the transcript, read by the adapter before the spawn** (DL-443). Rejected: the owner proposal to give the stored cost to the CLI (no such option); "the first resumed result adds 0" (HEAD, loses a turn); a station-stored "last counted" value (wrong after a `SIGKILL`, and the events are pruned after 7 days).
5. **The baseline lives in the checkpointed core state, not in the config** (DL-443). Rejected: `config.resumed` (lost on a shim restore, §1.2.4).
6. **An unreadable baseline counts the turn tokens and no cost for the first result.** Rejected: count the whole total (a large overcount); count nothing (B21 again).
7. **Codex source is `tokenUsage.total` per thread, with the pre-turn replay as a base** (DL-445). Rejected: `last` (counts the replay of every resume and fork; misses a dropped notification).
8. **Codex sub-agent threads are counted and priced at the session model** (DL-445). Rejected: drop them (HEAD); price them by a model that the core does not know.
9. **One price table in `fleetdeck-providers/src/prices.rs`, Codex rows only, dated, cache rates added** (DL-444). Rejected: Claude rows (the CLI prices Claude, and a second price would disagree); a price table in the deck (the station infers at the source, as DL-161).
10. **No new row for `gpt-daybreak-blue-latest`; the `gpt-5.6-luna` placeholder stays** (owner question 1). Rejected: invent a rate.
11. **B20 is a recount from the provider record with a new `usage.set` event** (DL-447). Rejected: a SQL migration that recomputes from stored events (the events are gone or wrong, §1.6.8); a reset to zero (false); a reset to unknown for every session (loses correct data that the provider has).
12. **The recount is an owner command per session through the generic command route, run from the Usage tab** (DL-448). Rejected: an automatic run at upgrade (changes stored data without the owner); a new deck bulk route (a second audit and receipt path).
13. **A session without a provider record becomes "unknown"** (owner question 2). Rejected: keep the old, too high total.
14. **A running session is not recounted.** Rejected: recount and keep counting (the live deltas after the last `cost-state` would be lost).
15. **The recount of a forked or imported session includes the conversation before it** (DL-447). Rejected: subtract an unknown fork-time value.
16. **"At API prices" stays the cost label; no plan detection** (DL-446). Rejected: a "subscription" word from `apiKeySource` (the board has no reliable plan fact).
17. **No chat line for `usage.set`.** Rejected: a timeline line (noise in the chat; Properties shows it).
18. **The recount scans the whole provider file under a 5 s deadline; the live baseline scans at most 32 MiB in 2 s.** Rejected: one shared limit (the live path is on the resume path and must stay short).

## 12. Owner questions

1. **Codex prices.** Your Codex sessions use `gpt-daybreak-blue-latest`, which has no price, so they show tokens and no cost. Do you want a price for it (and are the placeholder rates of `gpt-5.6-luna` correct)? *Recommended default:* no row for `gpt-daybreak-blue-latest` (cost unknown), keep the `gpt-5.6-luna` placeholder as DL-161 decided.
2. **Sessions without a provider record.** When the recount finds no transcript or rollout (for example a session whose files were deleted), should the stored total become unknown, or keep the old (too high) value? *Recommended default:* unknown.
3. **Where the recount button sits.** The Usage tab of the fleet home, or Settings → each station? *Recommended default:* the Usage tab (one action for the fleet).

## Decision log entries (to append)

## DL-440: Usage events stay deltas; provider running totals are the source (B14) (design agent, 2026-10-23)

- `usage.updated` keeps the delta contract of `fold.rs` `add_usage`. Each adapter keeps the last running total of its process (Claude, per model) or thread (Codex) and emits the growth. Older decks stay correct. Design `docs/v2/design/usage-cost.md` §3.

## DL-441: `input_tokens` is new input for both providers; cache reads and writes are separate counts (B14) (design agent, 2026-10-23)

- `Usage` gets `cache_read_tokens`, `cache_write_tokens` (optional). Codex input = `inputTokens − cachedInputTokens − cacheWriteInputTokens`. The board says "new input", "cache read", "cache write", "output". Design §3.1, §3.6.

## DL-442: Claude usage comes from `result.modelUsage` per model (B14) (design agent, 2026-10-23)

- The cumulative per-model totals contain subagents, background agents and the cache, and their `costUSD` is Claude's own list price. `result.usage` is used only when the base is unknown or `modelUsage` is absent. Replaces the cost rule of `0e6c3600`. Design §3.2.

## DL-443: The Claude resume baseline is the last `cost-state` record of the transcript (B21) (design agent, 2026-10-23)

- The CLI restores its totals from the last `cost-state` record (written at a clean exit, not at `SIGKILL`; a fork restores the parent's). The adapter reads it before the spawn (no-follow, owner uid, 32 MiB, 2 s) and stores it in the checkpointed core state. Unreadable: the first result counts the turn tokens and no cost. Fixes the "resumed first result adds 0" rule of `0e6c3600` and the shim-restore overcount. Design §3.3.

## DL-444: One dated price table for inferred costs, Codex rows only (B14) (design agent, 2026-10-23)

- `crates/fleetdeck-providers/src/prices.rs`, `PRICE_TABLE_VERSION = "2026-10-23"`, rates per 1M tokens for input, cache read, cache write and output. An inferred cost carries the table date in `Usage.prices`. No Claude rows. Design §3.5.

## DL-445: Codex usage comes from `tokenUsage.total` per thread, sub-agents included (B14) (design agent, 2026-10-23)

- An own-thread update before the first `turn/started` of a process (the resume or fork replay) is a base and adds nothing. Sub-agent threads add their own growth, priced at the session model. Design §3.4.

## DL-446: Every cost stays "at API prices"; no plan detection (B14) (design agent, 2026-10-23)

- The existing label and title ("A subscription does not bill it") are the estimate text. Design §2 (B14.4).

## DL-447: Stored totals are recounted from the provider record with a `usage.set` event (B20) (design agent, 2026-10-23)

- The events cannot rebuild the totals (7-day retention, 500 deck rows, old meanings). The station reads the last Claude `cost-state` or Codex `token_count` through the trusted history locator and appends `usage.set { usage, previous, source }`; the folds replace. A session without a record becomes unknown. A forked or imported session's recount includes the conversation before it. Design §4.

## DL-448: The recount is an owner command per session, `session.usage_recount`, feature `usage_recount.v1` (B20) (design agent, 2026-10-23)

- Generic command route, scope `admin`, owner login only, audited per session; the station refuses a session with a live process or a pending start. The board runs it from the Usage tab, four at a time, after a confirm. Design §4.2-§4.4.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Your Codex sessions run gpt-daybreak-blue-latest, which has no price, so they show tokens but no cost. Do you want to give rates for it, and are the gpt-5.6-luna placeholder rates (gpt-5 rates) correct? — **The recommended default: No row for gpt-daybreak-blue-latest (cost unknown); keep the gpt-5.6-luna placeholder per DL-161**
2. When the recount finds no provider record for a session (transcript or rollout deleted), should its stored total become unknown or keep the old, too-high value? — **The recommended default: Unknown**
3. Where should the Recount button sit: the Usage tab of the fleet home, or Settings for each station? — **The recommended default: The Usage tab (one fleet-wide action)**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.
