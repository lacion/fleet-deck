# Token counts and cost: a correct source, the resume baseline and a recount (B14, B20, B21)

- **Status:** Draft, revision 2 (2026-10-23), after the adversary DESIGN REVIEW round 1 (FAIL; every finding is resolved, see the last section). Design agent of the backlog program. Needs a RE-REVIEW before the build (risk high, §8).
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
8. B20 "recompute from the stored events": not possible in general. The station deletes events after 7 days, the deck keeps 500 rows per session, the old events have no cache tokens and no per-model totals, and the meaning of their `cost_usd` changed at `0e6c3600` without a marker. For the same reason a stored total cannot be continued with numbers of a new meaning: the design marks the new numbers (`scheme: 2`) and sets the old total apart (§3.1, DL-449).

## 2. Answers to the questions of the items

### B14

1. *Record live fixtures and compare with the provider numbers.* Done (§1.4, §1.5). Claude's own number is the cumulative `modelUsage` and `total_cost_usd` of a `result`, and the `cost-state` record of the transcript. Codex's own number is `tokenUsage.total` and the rollout `token_count`. Claude `/cost` shows the same `cost-state` values; it was not run separately.
2. *Use the provider's running totals and store the last value, instead of deltas?* Use the running totals as the **source**, but keep **deltas** in the events (DL-440). The adapter remembers the last running total per process (Claude) or per thread (Codex) and emits the growth. Reasons: the fold, the station and deck stores and every older deck stay correct; a `SIGKILL` makes Claude restore an older total (§1.4.4), so "the last value per conversation" would lose a killed process's usage; a fork starts from the parent's total, so "the last value" would count the parent twice in the fleet total. The scope of every session total is therefore **the usage of the executions that Fleet Deck ran for that session** ("execution scope"). The recount (§4) keeps that scope.
3. *Show cache tokens separately and use them in the cost.* Yes. `Usage` gets `cache_read_tokens` and `cache_write_tokens` (§3.1). `input_tokens` means "new input" (not read from the cache and not written to it) for both providers (DL-441). Because this changes the meaning of a stored number, every event of the new rules carries `scheme: 2`, and the fold never adds a new-rule part to an old total: the old total is set apart as an "old count" (DL-449, §3.1). Claude's `costUSD` already contains the cache prices. The Codex price table gets a cache-read and a cache-write rate.
4. *One price table, its date, "estimate" on a subscription.* One module `crates/fleetdeck-providers/src/prices.rs` with `PRICE_TABLE_VERSION` (§3.5). It prices only what no provider prices: today Codex. Claude's cost stays the CLI's own (`costBasis: "list"`). An inferred cost carries the table date to the board (`Usage.prices`). Every cost is already labeled "at API prices", with the title "A subscription does not bill it" (`web/src/board/usage.ts:72-83`); that is the "estimate" text, and it stays (DL-446).

### B20

1. *Recompute from the stored events (a migration) or reset and count from the fix date?* Neither alone. The events are gone or carry the wrong data (§1.6.8), and a reset to zero is false. The design has two parts (DL-447, DL-449):
   - **Automatic separation.** The first event of the new rules moves the stored total of a session into `SessionSummary.usage_legacy` (the "old count"). The new total counts from there. The board shows the old count apart and never adds it to the fleet totals.
   - **Owner recount.** For a session whose whole total is an old count, the owner can replace it with the provider's own record on the station: the last `cost-state` of the Claude transcript, or the Codex rollout priced turn by turn. A new event `usage.set` replaces the total; both folds apply it (§4). The record can miss usage (a killed Claude run, Codex sub-agent threads), so a recounted total is always marked as a lower bound ("at least").
2. *Show the reset in the audit log.* Yes. The recount is an owner command `session.usage_recount` with two audit rows per session on the deck: `stage=submitted` and the terminal outcome (§4.3). The summary keeps the replaced total durably (`usage_recount.previous`); it does not depend on event retention.
3. *Which sessions?* Claude and Codex sessions whose total is entirely an old count, whose conversation started in this session on this station (no import, no move, no other session on the same conversation), and whose every execution is proven ended. All other sessions keep their old count apart, and the board says why.

### B21

1. *Pass the recorded cost to the resumed process?* No (§1.6.7). The station reads the totals that the CLI will restore (the last `cost-state` of the transcript) before the spawn and uses them as the baseline of the new process (§3.3).
2. *Depends on B14.* Yes: the same per-model running-total rule handles a new process, a resume, a fork and a station restart.

## 3. Design: live counting (B14, B21)

### 3.1 Data shapes (protocol)

`crates/fleetdeck-proto/src/event_types.rs`, `Usage` (`event_types.rs:198-208`) gets six fields. All are optional or default, so an older peer reads and writes the old shape:

```rust
/// The accounting scheme of this design (DL-449). An event or a total
/// without `scheme` uses the rules before it (an "old count").
pub const USAGE_SCHEME: u32 = 2;

/// Token usage. `None` means unknown, which is not zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct Usage {
    /// Scheme 2: input tokens that the prompt cache did not hold and the turn
    /// did not write to it ("new input"), for both providers (DL-441).
    /// Without a scheme: the provider's old meaning (Codex: cached input included).
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
    /// `Some(USAGE_SCHEME)` for usage counted by the rules of this design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub scheme: Option<u32>,
    /// Some tokens of this interval are not in the token counts (a count is
    /// a lower bound).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional)]
    pub tokens_partial: bool,
    /// Some usage of this interval has no cost in `cost_usd` (a known cost
    /// is a lower bound; `None` stays unknown).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional)]
    pub cost_partial: bool,
}

impl Usage {
    /// No token part and no cost is known (a rate-limit-only update).
    pub fn is_empty(&self) -> bool; // input, output, cache_read, cache_write and cost_usd are all None
    /// Any token part is `Some`.
    pub fn has_tokens(&self) -> bool;
}
```

TypeScript (`web/src/gen/event/Usage.ts`, generated): `cache_read_tokens?: number, cache_write_tokens?: number, prices?: string, scheme?: number, tokens_partial?: boolean, cost_partial?: boolean`.

`crates/fleetdeck-proto/src/session.rs`, `SessionSummary` gets (the `SessionSummary` constructor at `session.rs:255` sets both to `None`):

```rust
/// The old count of a session (DL-449): the total that the session had
/// when the first usage of `USAGE_SCHEME` came, plus every later usage
/// without a scheme. It is never added to `usage`.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[ts(optional)]
pub usage_legacy: Option<LegacyUsage>,
/// The last recount of `usage` from the provider record (B20, §4.1).
#[serde(default, skip_serializing_if = "Option::is_none")]
#[ts(optional)]
pub usage_recount: Option<UsageRecount>,
```

```rust
/// An old count (DL-449).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct LegacyUsage {
    pub usage: Usage,
    /// Unix ms of the first event of `USAGE_SCHEME`: the old count is the
    /// usage before it.
    pub until: i64,
}
```

**Fold rules** (`fold.rs`, the arm `Event::UsageUpdated` at `fold.rs:154-156` calls a new `fold_usage(self, usage, at)`; the rate limits merge as today):

1. `more.is_empty()`: nothing changes (`account/rateLimits/updated` sends `Usage::default()`, `codex/map.rs:369-372`).
2. `self.usage.scheme == more.scheme`: `add_usage(&mut self.usage, more)`.
3. `self.usage.scheme == None`, `more.scheme == Some(_)` (the first new-rule event of the session):
   - When `self.usage.is_empty()`: `self.usage = Usage { scheme: more.scheme, ..Usage::default() }`, then `add_usage`.
   - Else: `let old = std::mem::take(&mut self.usage)`. `self.usage_legacy` becomes `Some(LegacyUsage { usage: old, until: at })` (when it is already `Some`, `add_usage(&mut legacy.usage, &old)` and `legacy.until = at`). Then `self.usage = Usage { scheme: more.scheme, ..Usage::default() }` and `add_usage(&mut self.usage, more)`.
4. `self.usage.scheme == Some(_)`, `more.scheme == None` (an old-rule event after the change, for example from an older station build that a deck folds): `add_usage` into `self.usage_legacy` (created with `until: at` when absent). `self.usage` does not change.
5. `self.usage.scheme == Some(a)`, `more.scheme == Some(b)`, `a != b`: rule 2 (only scheme 2 exists; a later scheme defines its own rule).

`add_usage` (`fold.rs:366-379`) also:

- adds `cache_read_tokens` and `cache_write_tokens` with the same `add` rule;
- sets `total.prices = more.prices.clone()` when `more.prices.is_some()` (the latest table wins);
- sets `total.tokens_partial |= more.tokens_partial` and `total.cost_partial |= more.cost_partial`;
- sets `total.cost_partial = true` when `more.cost_usd.is_none() && more.has_tokens()` (a part with tokens and no cost), and when `total` had tokens and no cost before and `more.cost_usd.is_some()` (the earlier unpriced tokens are not in the new cost). This stops `None + Some(x) = Some(x)` from showing a complete-looking cost.

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
    /// The per-model totals of the last `result` that had `modelUsage`, or
    /// the base before the first one. `None`: the token base is not known
    /// (an unknown base, an older checkpoint, or the last result had no
    /// `modelUsage`).
    pub last: Option<ProcessTotals>,
    /// The cumulative cost of the process at the last `result` (or the base).
    /// It moves at every result that has a cost, with or without
    /// `modelUsage`. `None`: not known.
    pub cost_cursor: Option<f64>,
}

impl ProcessTotals {
    /// `result.modelUsage`; `None` when the frame has no object there.
    pub fn from_model_usage(v: &Value) -> Option<Self>;
    /// A `cost-state` record of a transcript (`modelUsage`, same keys).
    pub fn from_cost_state(v: &Value) -> Option<Self>;
    /// The sum of `cost_usd` over the models.
    pub fn cost(&self) -> f64;
    /// The sum over the models, as one `Usage` (scheme 2, cost not inferred).
    pub fn usage(&self) -> Usage;
}

impl ProcessUsage {
    /// Zero → { last: Some(empty), cost_cursor: Some(0.0) };
    /// Restored(t) → { last: Some(t), cost_cursor: Some(t.cost()) };
    /// Unknown → { last: None, cost_cursor: None }.
    pub fn new(base: &UsageBase) -> Self;
    /// The state of a checkpoint written before this design: no token base,
    /// and the cost cursor of its `last_total_cost`.
    pub fn legacy(last_total_cost: Option<f64>) -> Self;
    /// The delta of one `result`, and the new state.
    pub fn delta(&mut self, res: &ResultFrame) -> Usage;
}
```

`delta` rules (exact). Every returned `Usage` has `scheme: Some(USAGE_SCHEME)`, `cost_inferred: false`, `prices: None`.

1. `current = ProcessTotals::from_model_usage(res.body["modelUsage"])`. `now_cost = res.total_cost_usd.or(current.map(|c| c.cost()))`.
2. **Cost part (every shape):** with `now_cost = Some(n)` and `cost_cursor = Some(c)`: `n − c` when `n ≥ c`, else `n` (the count started again). With `now_cost = Some(n)` and `cost_cursor = None`: `cost_usd: None`, `cost_partial: true`. With `now_cost = None`: `cost_usd: None`, `cost_partial: true`. Then `cost_cursor = now_cost` when it is `Some`.
   - Exception: in rule 3 (both `Some`), the cost part is the sum of the per-model `cost_usd` deltas of rule 3, not `n − c`, so that a reset of one model does not count the others again.
3. **Tokens, `current` `Some` and `last` `Some(last)`:** for each model in `current`, `d = current[m] − last[m]` per field (a model that is not in `last` counts from zero). If any field of a model is lower than in `last`, that model's count started again: `d = current[m]`. The tokens are the sum of all `d`; `tokens_partial: false`. Then `last = Some(current)`.
4. **Tokens, `current` `Some` and `last` `None`** (the first result after an unknown base, an older checkpoint, or a result without `modelUsage`): the tokens of this turn from `result.usage` (`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`), `tokens_partial: true` (a subagent's tokens of this turn are not in `result.usage`, §1.4.1). Then `last = Some(current)`; the next result uses rule 3. No earlier token is counted again.
5. **Tokens, `current` `None`** (a CLI without `modelUsage`): the tokens of this turn from `result.usage`, `tokens_partial: true`. Then `last = None`, so a later result with `modelUsage` re-seeds through rule 4 and does not count the per-model totals again. Log `debug!("claude result without modelUsage")`.
6. A delta whose every part is 0 is still emitted (the turn completed; the fold adds nothing).

Example (the adversary's case): base `Zero`, two results without `modelUsage` with `total_cost_usd` 0.10 and 0.20 emit 0.10 and 0.10 (sum 0.20).

`ClaudeCore` (`core.rs:165-223`): add `#[serde(default)] pub(super) usage: Option<ProcessUsage>`. `None` exists only in a checkpoint of an older station. `ClaudeCore::new` sets `usage = Some(ProcessUsage::new(&config.usage_base))`. `ClaudeCore::restore` (`claude/checkpoint.rs`) replaces `None` with `Some(ProcessUsage::legacy(core.last_total_cost))`. Keep the field `last_total_cost` (read from an older checkpoint) and stop writing it.

`CoreConfig` (`core.rs:40-48`) gets `pub usage_base: UsageBase`. Every literal of `claude::CoreConfig` adds `usage_base: UsageBase::Zero` (the build task lists them; `grep -rn "claude::CoreConfig {\|CoreConfig {" crates`). The placeholder of `checkpoint.rs:23-30` uses `Zero`; it is not used for counting because the restored `usage` state wins.

`map.rs`: `on_result` (`map.rs:547-581`) calls `core.usage_mut().delta(res)` (`usage_mut` returns the `Some` value; it is always `Some` after `new` or `restore`); `usage_of` and `cost_part` (`map.rs:513-542`) are removed. The `usage.updated` and the `turn.completed` usage carry the same delta.

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
    /// `self − prev` per field; `self` itself when any field of `self` is lower (a new count).
    pub fn since(&self, prev: &TokenTotals) -> TokenTotals;
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
    /// Restored from a checkpoint of a build before this design: the restored
    /// app-server did not reopen its thread, so no resume replay comes, and
    /// the old core counted the `last` blocks. The first update of a thread
    /// without an entry counts its `last` block and seeds the entry.
    #[serde(default)]
    pub legacy: bool,
}

impl ThreadUsage {
    /// The state of a checkpoint written before this design.
    pub fn legacy() -> Self; // { totals: empty, turn_seen: true, legacy: true }
    /// The delta of one `thread/tokenUsage/updated`; `None`: nothing to emit.
    pub fn delta(&mut self, thread: &str, own: bool, p: &Value) -> Option<TokenTotals>;
}
```

Normalized usage (`TokenTotals::usage`): `input_tokens = input − cached − cache_write` (saturating), `cache_read_tokens = cached`, `cache_write_tokens = cache_write`, `output_tokens = output` (reasoning tokens are part of `output`), `scheme: Some(USAGE_SCHEME)`. The cost and `prices` come from §3.5; `cost_inferred = cost.is_some()`. When the model is `None` or has no price and any token count is above 0: `cost_usd: None`, `cost_partial: true`.

`delta` rules (exact):

1. `total = TokenTotals::from_app_server(p["tokenUsage"]["total"])`. If it is `None`: return `from_app_server(p["tokenUsage"]["last"])` (the HEAD rule, for an older app-server).
2. No entry for `thread` and `legacy`: store `total`; return `from_app_server(p["tokenUsage"]["last"])` (the model call that this update reports; the old core counted the earlier ones, or dropped a sub-agent's, as HEAD did).
3. No entry for `thread`, `own`, and `!turn_seen`: this is the replay of a resume or fork (§1.4.7). Store `total`; return `None`.
4. No entry otherwise (a new thread, a sub-agent): store `total`; return `Some(total)`.
5. An entry `prev`: store `total`; return `Some(total.since(&prev))`, or `None` when the difference is all zero.

`CodexCore` (`codex/core.rs:35`) gets `#[serde(default)] pub(super) usage: Option<ThreadUsage>` (one field: `core.rs` has 782 lines). `CodexCore::new` sets `Some(ThreadUsage::default())`. `CodexCore::restore` (`codex/checkpoint.rs:59`) replaces `None` with `Some(ThreadUsage::legacy())`. `on_turn_started` sets `turn_seen = true`. In `codex/map.rs:361-368` the two arms become one: `"thread/tokenUsage/updated"` for any thread → `usage.delta(thread, self.own_thread(p), p)` → `Event::UsageUpdated { usage: delta.usage(self.thread_model.as_deref()), rate_limits: vec![] }`. A sub-agent's tokens are priced at the session model (DL-445). A sub-agent update never touches the turn state.

Why the legacy state is safe: a checkpoint is written by a running shim session, and the restored core keeps the same app-server (`codex/checkpoint.rs:1-10`). Restored during a turn: the next update is a real model call, and rule 2 counts its `last` block (as the old core would). Restored between turns: the next `turn/started` and update follow; rule 2 counts `last`, not the whole thread total.

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

Files: `web/src/board/usage.ts`, `web/src/board/UsageMeters.tsx`, `web/src/board/UsageGlance.tsx`, `web/src/board/fleet.ts`, `web/src/session/PropertiesPanel.tsx`, `web/src/compare/ComparePage.tsx`. Colors: tokens and cost stay `--fuel`. Numbers: `countText`, `compactCount`, `usdText` (DESIGN-SYSTEM §5).

1. New helper in `usage.ts`:

   ```ts
   /** The new-rule usage of a session and its old count (DL-449). */
   export type SplitUsage = {
     /** The usage counted by the new rules; `null` when the whole total is an old count. */
     readonly current: Usage | null;
     /** The old count; `null` when there is none. */
     readonly old: Usage | null;
     /** Unix ms: the old count is the usage before it; `null` when the whole total is old. */
     readonly oldUntil: number | null;
   };
   export function splitUsage(summary: SessionSummary): SplitUsage;
   ```

   Rules: `usage.scheme` set → `current = usage`, `old = usage_legacy?.usage ?? null`, `oldUntil = usage_legacy?.until ?? null`. `usage.scheme` absent and the usage has a known part → `current = null`, `old = usage`, `oldUntil = null`. Else `current = usage`, `old = null`, `oldUntil = null`.
2. `UsageTotal` gets `readonly partial: number` (sessions whose part is a lower bound). `fleetUsage` sums only `current`. A session with `current === null` counts as unknown for every part. A session counts as partial for a token part when `current.tokens_partial` or `old !== null`, and for the cost when `current.cost_partial` or `old !== null`. `FleetUsage` gets `cacheRead: UsageTotal`, `cacheWrite: UsageTotal` and `oldCounts: number` (sessions with `old !== null`).
3. `totalText`: `unknown` when `known === null`. Else, with `unknown === 0 && partial === 0`: the sum. With `partial === 0`: the existing `at least <sum> (<n> session(s) unknown)`. With `unknown === 0`: `at least <sum> (<p> partial)`. With both: `at least <sum> (<n> unknown, <p> partial)`.
4. `inputLabel` (`usage.ts:64-69`) returns `'new input'` for every fleet (only new-rule usage is summed). `NEW_INPUT_TITLE` (`PropertiesPanel.tsx:203`) becomes: `Input that the prompt cache did not hold. Cache reads and cache writes are separate counts.`
5. The Usage tab (`UsageMeters.tsx:35-49`) shows four token stats in this order: "New input tokens", "Cache read tokens", "Cache write tokens", "Output tokens", then "At API prices". Under the stats, when `oldCounts > 0`, a `.fd-note`: `1 session has an old count that is not in these totals.` or `<n> sessions have old counts that are not in these totals.` The glance (`UsageGlance.tsx`) does not change its parts; it uses the new `fleetUsage`.
6. `costText` (`usage.ts:85`): `null` when unknown; else the existing text with the prefix `at least ` when `cost_partial` (`at least $1.20`, `at least ≈ $1.20 inferred`).
7. Properties, row "Tokens": `tokensText(input, output, inputWord)` becomes `tokensText(usage: Usage)` over `current`, with four parts in this order: `<n> new input`, `<n> cache read`, `<n> cache write`, `<n> output`; a part whose count is `null` or absent is left out; the prefix `at least ` when `tokens_partial`. `current === null` → `unknown`.
8. Properties, row "At API prices": `costText(current)`. The title of an inferred cost (`cost_inferred` and `prices` set) is `apiPricesTitle(1)` plus ` Fleet Deck inferred it from its price table of <prices>.`
9. Properties, new row "Old count" when `old !== null`: `<n> input · <n> output · <usdText(cost)>` (the parts that are known; the old word `input`, because an old Codex count includes the cached input). Title: with `oldUntil === null`: `Fleet Deck counted this with an older rule that was wrong. It is not in the fleet totals. "Recount from provider records" on the Usage tab can replace it.`; with `oldUntil` set: `Fleet Deck counted this before <date of oldUntil> with an older rule that was wrong. It is not in the totals above or in the fleet totals.`
10. `fleet.ts` `usageText` (`fleet.ts:241-244`) uses `current`; `current === null` gives `Old count only`.
11. Compare page (`ComparePage.tsx:191-196`): uses `current` (`null` → `unknown`) and adds `<n> cache read` after `<n> in` when known.

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
    /// The `token_count` records of the Codex rollout file, priced turn by turn.
    CodexRollout,
    /// The provider has no record of the session: the total is unknown.
    None,
}

/// The last recount of a session (B20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct UsageRecount {
    /// Unix ms of the `usage.set` event.
    pub at: i64,
    pub source: UsageSource,
    /// The command that asked for it: the board matches its command to the summary.
    pub command_id: CommandId,
    /// The total that the recount replaced (an old count). Kept here, not
    /// only in the event, so that event retention does not remove it.
    pub previous: Usage,
}
```

`crates/fleetdeck-proto/src/event.rs`, a new variant (kind `usage.set`, added to `Event::KINDS` and `kind_name` in `event_kinds.rs`):

```rust
/// The station replaced the old count of the session with the provider's
/// own record (B20). The fold replaces, it does not add.
#[serde(rename = "usage.set")]
UsageSet {
    /// Scheme 2, `tokens_partial` and `cost_partial` set (§4.2 step 3).
    usage: Usage,
    /// The total that the summary had before.
    previous: Usage,
    source: UsageSource,
    command_id: CommandId,
},
```

Fold (`fold.rs`): `Event::UsageSet { usage, previous, source, command_id } => { self.usage = usage.clone(); self.usage_legacy = None; self.usage_recount = Some(UsageRecount { at, source: *source, command_id: command_id.clone(), previous: previous.clone() }); }`. The rate limits do not change.

`crates/fleetdeck-proto/src/command.rs`, a new `SessionCommand` variant, method `session.usage_recount`, with its `session_id` (`command.rs` has 747 lines; the variant, its `method()` arm and its `session_id()` arm add about 12):

```rust
/// Replace the old count of a session whose executions all ended with the
/// provider's own record (B20). Owner action, scope `admin`.
UsageRecount { session_id: SessionId },
```

`crates/fleetdeck-proto/src/version.rs`: `pub const USAGE_RECOUNT: &str = "usage_recount.v1";` (the command and the `usage.set` event), in `ALL`, not in `REQUIRED`.

Board command map (`web/src/session/commands.ts:102-135`): `usage_recount: 'session.usage_recount'`.

### 4.2 Station

New module `crates/fleetdeck-station/src/manager/usage_recount.rs`, a reader `crates/fleetdeck-station/src/history/usage_record.rs`, and the store reads `crates/fleetdeck-station/src/store/usage_recount.rs`.

1. **Serialize with resume, reconcile and stop.** `stop_prepare` (`manager/stop_plan.rs:73-98`) adds `SessionCommand::UsageRecount { session_id } => (session_id, None)`: the recount holds the per-session lock of BG-4 rule 1 through its prepare and its decision, so a resume, a reconcile or a stop of the same session waits.
2. **Prepare (outside the writer transaction),** in `SessionManager::submit`, next to `reconcile_check` (`manager/mod.rs:628`): `self.recount_prepare(&command).await` returns `Option<RecountFacts>`:

   ```rust
   pub(super) struct RecountFacts {
       pub provider: Option<ProviderKind>,
       pub native: Option<String>,
       pub session_seq: u64,
       /// The process of the session is in the manager's live map.
       pub live: bool,
       /// Why an execution of the session or of its conversation may still run
       /// (the resume rule), after the reboot rows were revalidated.
       pub blocked: Option<String>,
       /// The shim ids whose reboot proof was revalidated here.
       pub revalidated: Vec<String>,
       /// The record, or the refusal of step 2.e.
       pub record: Result<(Usage, UsageSource), DecideError>,
   }
   ```

   a. Read the session row and summary (provider, instance, cwd, native id, `session_seq`, model).
   b. `live`: the session id is in the manager's live map.
   c. `blocked`: `self.store.execution_exclusion(Some(s), kind, native, Some(s), Purpose::Resume)` (`store/resumes.rs:514`, the rule of a resume: the session's own shim rows, other sessions on the same conversation, launches that name it, move holds). When `found.blocked` is `None` and `found.reboot_rows` is not empty: `shims::revalidate(&self.boot_reader(), &found.reboot_rows)` (`manager/shims.rs:464`), as `resume_blocked` does (`shim_target.rs:277-303`); `revalidated` holds their shim ids. A store error: `blocked = Some("store: <e>")`.
   d. When the provider is not Claude or Codex: `record = Err(unsupported_provider)`, and step e does not run.
   e. No native id: `record = Ok((unknown, UsageSource::None))` where `unknown = Usage { scheme: Some(2), tokens_partial: true, cost_partial: true, ..Usage::default() }`. Else open the provider file with the trusted locator: `history::locate::locate(home, kind, cwd, native, trust)` (`history/locate.rs:87-97`, home from `history::env_for` and `locate::resolve_home`, the history `Limits`), in `spawn_blocking`, with a deadline of **3 s** for the open and the scan together.
      - Claude: `fleetdeck_providers::claude::cost_state::last_cost_state(file, native, u64::MAX)`. `Some(t)` → `t.usage()` with `tokens_partial: true, cost_partial: true`, `ClaudeTranscript`. `None` → `(unknown, None)`.
      - Codex: `usage_record::codex_usage(file, summary_model)` (rules below). `Some(u)` → `(u, CodexRollout)`. No `token_count` with `info` → `(unknown, None)`.
      - `HistoryUnavailable::NotFound` → `(unknown, None)`. Every other failure (not trusted, I/O, deadline) → `Err(record_unreadable)` with the reason word.
3. **Decide (in the writer transaction),** in this order; the first that applies refuses:
   a. `unsupported_provider`: the provider is not Claude or Codex.
   b. `counted_since_upgrade`: `summary.usage.scheme.is_some()` or `summary.usage_legacy.is_some()`. Only a total that is entirely an old count is recounted; a new-rule total (live counted, or recounted before) is never replaced, so no counted Codex sub-agent usage and no killed Claude run counted live can be lost.
   c. `other_history`: the provider record can hold usage that another session counts, or usage outside the execution scope: the session has a `session_transcripts` row (an import, `store/migrations.rs:583`), or `summary.moved_to` or `summary.move_in` is set, or another session of the station has the same provider and native id. (A fork has no station path: `session.fork` is refused, `manager/decide.rs:136`.)
   d. `session_running`: `facts.live`; or `facts.blocked` is `Some`; or `db.busy` (the `BUSY` rule of `store/resumes.rs:183`: state not `exited` or `lost`, a live process row, or a queued or claimed start or resume effect); or `db.blocked` is `Some` (the resume rule again, in the transaction); or `d.native_session_held(native)` is `Some` (a terminal holds the conversation, `resumes.rs:337`); or `db.reboot_rows` has a shim id that is not in `facts.revalidated`. A `lost` row without proof (`manager/shims.rs:215` leaves the shim alive when its checkpoint is unreadable) therefore refuses the recount.
   e. `session_changed`: the summary's `session_seq` is not `facts.session_seq`.
   f. `record_unreadable`: `facts.record` is that `Err`.
   g. Else append `Event::UsageSet { usage, previous: summary.usage.clone(), source, command_id: d.command_id().clone() }`. The receipt is `Accepted` with `result(session_id, None)` (`manager/decide.rs:360-366`).
4. `db = d.recount_db(session, kind, native)?` (new, `store/usage_recount.rs`, read once at the start of step 3) returns:

   ```rust
   pub struct RecountDb {
       /// The `BUSY` rule (made `pub(super)` in `resumes.rs`).
       pub busy: bool,
       /// A `session_transcripts` row exists (an import).
       pub imported: bool,
       /// Another `sessions` row has the same `provider` and `native_session_id`.
       pub same_native: bool,
       /// `exclusion(conn, Some(session), kind, native, Some(session), Purpose::Resume).blocked`
       /// (`resumes.rs:208`): the session's own shim rows, other sessions and
       /// launches of the conversation, and move holds.
       pub blocked: Option<String>,
       /// The shim ids of that exclusion's `reboot_rows`.
       pub reboot_rows: Vec<String>,
   }
   ```

   Step 3.c uses `imported` and `same_native`; step 3.d uses the rest.
5. The direct decide (`decide.rs`) refuses the command like `StopExecution` (`decide.rs:349`): `SessionCommand::UsageRecount { .. } => Err(super::usage_recount::refuse_direct())`.
6. **Module size:** `manager/mod.rs` has 797 lines and `decide.rs` 792. The task moves the closure body `match command { … }` of `mod.rs:635-664` without change into `manager/submit_route.rs` (`pub(super) fn route(d, ctx: RouteCtx<'_>, command: SessionCommand) -> Result<…>`, `RouteCtx` holds `origin`, `policy`, `coordinator`, `shells`, `restarting`, `root`, `check`, `stop` and the new `recount`), and adds the `UsageRecount` arm there.
7. The station advertises `usage_recount.v1`.

**Codex record (`usage_record::codex_usage(file, fallback_model: Option<&str>) -> io::Result<Option<Usage>>`),** a forward scan of the whole file under the deadline of step 2.e (finding: a model switch must not re-price earlier turns):

1. `model = fallback_model`, `prev = TokenTotals::default()`, `sum = Usage { scheme: Some(2), tokens_partial: true, cost_partial: true, ..Usage::default() }`, `seen = false`.
2. A line with `"type":"turn_context"` and a string `payload.model`: `model = Some(payload.model)`.
3. A line with `"type":"event_msg"`, `payload.type == "token_count"` and an object `payload.info.total_token_usage`: `total = TokenTotals::from_rollout(..)`; `d = total.since(&prev)`; `add_usage(&mut sum, &d.usage(model))` (the fold rule of §3.1, made `pub` in `fold.rs` as `fleetdeck_proto::fold::add_usage`); `prev = total`; `seen = true`. A `token_count` whose `info` is `null` is skipped.
4. End: `seen` → `Some(sum)`; else `None`.

An interval of an unlisted model adds its tokens and no cost, and `cost_partial` stays set; a session whose every interval is unlisted has `cost_usd: None` (unknown).

**Why every recounted total is a lower bound** (`tokens_partial` and `cost_partial` set): a Claude run that was killed wrote no `cost-state`, and the next resume restored an older one (§1.4.4), so its usage is in no record; the station cannot prove that every earlier Claude run exited cleanly (a shim `exited` row does not record a signal). A Codex sub-agent thread has its own rollout, and the parent rollout does not contain its usage (§1.4.8). The recount reads only the session's own record.

Refusals are built as in `manager/stop_decide.rs:40-43`: `reject(<RejectCode>, "<code>: <message>")`. `unsupported_provider` uses `RejectCode::Unsupported`; the others use `RejectCode::InvalidState`.

| Code | Message |
| --- | --- |
| `unsupported_provider` | `Only Claude and Codex sessions have a provider record.` |
| `counted_since_upgrade` | `Fleet Deck counts this session with the new rules. Only an old count is recounted.` |
| `other_history` | `The provider record of this session can hold usage of another session or station.` |
| `session_running` | `This session may still run (<reason>). It keeps its old count.` |
| `session_changed` | `The session changed during the recount. Recount it again.` |
| `record_unreadable` | `Fleet Deck could not read the provider record: <reason word>.` |

### 4.3 Deck

1. Methods table (`crates/fleetdeck-deck/src/http/methods_table_rest.rs`, after `session.release_shell` at line 516): `cmd("session.usage_recount", Scope::Admin),` with the comment `// The owner recounts an old usage count (B20); owner login only, the station needs usage_recount.v1.`
2. Generic command route (`crates/fleetdeck-deck/src/http/command_routes.rs`, next to the `ReleaseShell` owner check at lines 321-337): `UsageRecount` from a caller that is not the owner's board login → 403 `owner_only` ("only the owner's board login recounts usage"); a station whose link has not negotiated `usage_recount.v1` (`board.store.link_feature(&rec.station_id, feature::USAGE_RECOUNT) != LinkFeature::Negotiated`) → 409 `not_supported` ("this station cannot recount usage").
3. **Audit** (new module `crates/fleetdeck-deck/src/http/recount_audit.rs`, after the pattern of `StopAudit` and `watch_outcome`, `execution_routes.rs:315-366` and `485-515`). The generic route writes `AuditResult::Ok` with the station target before the receipt (`command_routes.rs:67-73`, wait at line 85); for this method that row is not an outcome. So:
   - `RecountAudit::of(req) -> Option<RecountAudit>` (`session`, `command`), for a `UsageRecount` request only. Every row has the action `session.usage_recount`, the target `<station>/<session>` and the command id.
   - In `command()`: when `RecountAudit::of` is `Some`, the generic success row gets the target `<station>/<session>` and the detail `stage=submitted` (`stage=submitted duplicate` for a repeated command id); the generic refusal row gets the target `<station>/<session>` and the detail `stage=refused code=<code>`.
   - For a new command (not a duplicate): `watch_recount(board, actor, station, audit)` spawns a task: `board.link.wait_command(command, board.stop_watch.limit)` (the same configurable limit as a stop outcome). Then one row with `stage=outcome`: receipt `accepted` → `AuditResult::Ok`; receipt `rejected` → `AuditResult::Refused` with `code=<the text of the reject message before the first ": ">`; state `failed` → `AuditResult::Failed` with `delivery=failed`; state `ambiguous` → `AuditResult::Failed` with `delivery=ambiguous`; still `pending` at the limit → `AuditResult::Failed` with `stage=outcome_unobserved`. An error of `wait_command` → `AuditResult::Failed` with `stage=outcome_unobserved`. The row is written with `board.store.audit_or_log`.
4. The receipt wait stays `RECEIPT_WAIT` (5 s, `command_routes.rs:38`). The station's scan deadline is 3 s (§4.2 step 2.e), but a queue in the station can still pass 5 s: the board then gets `pending` and polls (§4.4).
5. No deck migration: the summary is JSON, and the event is stored in `timeline` as every event.

### 4.4 Board (recount)

New files `web/src/board/usageRecountCall.ts` (the typed action) and `web/src/board/UsageRecount.tsx` (the component), shown in the Usage tab under the stats of `UsageMeters.tsx`.

1. **Typed action** (`usageRecountCall.ts`); `sessionActions(...).run` is private (`commands.ts:232`) and its `Outcome` loses the state and the codes, so the recount has its own call:

   ```ts
   export type RecountResult =
     | { readonly kind: 'accepted'; readonly commandId: string }
     | { readonly kind: 'refused'; readonly code: string; readonly message: string }
     | { readonly kind: 'not_supported' }
     | { readonly kind: 'pending'; readonly commandId: string }
     | { readonly kind: 'failed'; readonly text: string };
   export type RecountCall = (key: SessionKey) => Promise<RecountResult>;
   export function usageRecountCall(
     get: (path: string) => Promise<unknown>,
     post: (path: string, body: unknown) => Promise<unknown>,
     options?: { newId?: () => string; pollMs?: number; limitMs?: number; sleep?: (ms: number) => Promise<void> },
   ): RecountCall;
   ```

   - `POST /api/commands/session.usage_recount` with `{ station: key.station_id, command_id, command: { op: 'usage_recount', session_id: key.session_id } }`; `command_id` from `options.newId ?? commandId` (`commands.ts`).
   - An `ApiError` (the class that `commands.ts` imports from `../runtime/api`) with code `not_supported` → `not_supported`; any other thrown error → `failed` with its message.
   - The `DeckCommand` answer: `done` with `receipt.state === 'accepted'` → `accepted`. `done` with `rejected` → `refused`: `code` is the text of `receipt.reject.message` before the first `": "` when it matches `^[a-z_]+$` (else `refused`), `message` is the text after it (else the whole message). `failed` → `failed` with `Not delivered: <reason>`. `ambiguous` → `failed` with `Delivery is unknown.`. `pending` → poll `GET /api/commands/<encodeURIComponent(command_id)>` every `pollMs` (default 1000) until the state is not `pending`, then classify the same way; still `pending` after `limitMs` (default 30000) → `pending`.
2. **Wiring:** `StationActions` (`web/src/board/pairing.ts`) gets `readonly usageRecount?: RecountCall | undefined`. `RuntimeApp.tsx` sets it in `actions` when `owner && canAdmin && !guest`: `usageRecountCall((p) => connection.get(p), (p, b) => connection.post(p, b))`. `App.tsx` passes `actions?.usageRecount` to `FleetHome` as `recount`; `FleetHome` passes it to `HomePanel` as `recount`; `HomePanel` passes it to both `UsageMeters` (`HomePanel.tsx:183`, `:277`) as `recount`. `UsageMeters` renders `<UsageRecount sessions={sessions} recount={recount} />` only when `recount` is defined.
3. **Eligible sessions:** `provider` is `claude` or `codex`, and `splitUsage(summary)` gives `current === null` (the whole total is an old count). Of these, sessions with `summary.state` `exited` or `lost` are sent; the others are counted as "may still run" and are not sent. No row when no session is eligible.
4. **Look:** one row, the text `Old counts` (`.fd-label`) and a plain button `Recount from provider records`.
5. **Confirm dialog** (`.fd-dialog-head`, title `Recount the old counts?`, description: `Fleet Deck reads the provider records on each station (Claude transcripts, Codex session files) and replaces the old count of every Claude and Codex session that ended. A record can miss a killed run or Codex sub-agents, so a recounted total shows "at least". Properties keeps the old count.`). Buttons: `Recount` (`.fd-primary`) and `Cancel`.
6. **Run:** one `recount(key)` per sent session, at most 4 at a time. While it runs the button shows `Loading` with `Recounting <done> of <n>`.
7. **Result line** (`role="status"`), computed at each render from the call results and the `sessions` prop; the parts that are not 0, joined by a space:
   - `Recounted <n> sessions.` / `Recounted 1 session.`: `accepted` (or a `pending` that later appears), and the session's `summary.usage_recount.command_id` equals the result's `commandId`, with source `claude_transcript` or `codex_rollout`.
   - `<n> have no provider record: their totals are now unknown.`: as above, with source `none`.
   - `<n> wait for their new totals.`: `accepted` or `pending` whose summary does not show that `command_id` yet.
   - `<n> may still run and keep their old counts.`: not sent (state), or refused `session_running`.
   - `<n> hold usage of another session in their record and keep their old counts.`: refused `other_history`.
   - `<n> stations need a newer Fleet Deck.`: `not_supported`, counted per station.
   - `<n> could not be recounted.`: every other `refused` or `failed`; the first message follows in parentheses.
8. **Properties:** when `summary.usage_recount` is set, the "At API prices" row gets the aside `recounted` with the title `Recounted from the provider record on <date of at>. A killed run and Codex sub-agent threads are not in that record, so the real total can be higher. The old count was <usdText(previous.cost_usd) or "unknown">.`; with source `none` the aside is `no record` with the title `The provider has no record of this session. The total is unknown since <date>.`

## 5. Errors and edge cases

1. **A station restart while a resumed Claude process runs under a shim.** The usage state (`ClaudeCore.usage`) is in the checkpoint, so the restored core counts on from the right base. The config's `resumed: None` does not matter any more (fixes §1.2.4).
2. **A Claude checkpoint from a station before this change.** It has `last_total_cost` and no `usage`. `restore` sets `ProcessUsage::legacy(last_total_cost)`: the first result counts its turn tokens (`tokens_partial`) and the cost growth over `last_total_cost` (unknown and `cost_partial` when it is `None`); later results use the per-model rule.
3. **A Codex checkpoint from a station before this change.** It has no `usage`. `restore` sets `ThreadUsage::legacy()`: the first update of each thread counts its `last` block and seeds the thread's entry (§3.4 rule 2), during a turn and between turns. No update is taken as a replay, and no whole thread total is counted.
4. **A session that runs at the upgrade.** Its first new-rule event moves its stored total into `usage_legacy` (§3.1 fold rule 3). The board shows the new total as partial and the old count apart. It is not recountable (`counted_since_upgrade`); the old count stays visible (§9, open item e).
5. **A model switch in a process.** Claude: a new model key counts from zero (§3.2 rule 3). Codex live: each update is priced at the model of that moment. Codex recount: each interval is priced at the model of the `turn_context` before it.
6. **`/clear` or another reset of the CLI count.** A lower field means a new count; the model's whole current value is the delta. The cost cursor follows the same rule.
7. **Claude frames that alternate between shapes with and without `modelUsage`.** The cost cursor moves at every result; a result without `modelUsage` clears the token base, and the next one with it counts only its turn tokens (§3.2 rules 4 and 5). Nothing is counted twice.
8. **Claude interrupted or failed turns.** Their `result` carries `modelUsage`; the delta counts the tokens that the API billed.
9. **Workflows (ultracode, B12) and background agents.** They run in the same CLI process, so their usage is in `modelUsage` at the next `result`. Not recorded live in this draft (owner cost); the build verifies it once (task T3 acceptance item 4).
10. **A Codex sub-agent that a resumed parent knew before.** A replay of a sub-agent thread is not seen in the recordings. If one comes, it is counted once more (§9, open item b).
11. **Codex fork.** The new thread's first update before `turn/started` is the parent's total (§1.4.7): rule 3 of §3.4 counts nothing.
12. **Codex `total` absent.** The HEAD `last` rule (§3.4 rule 1).
13. **An unknown base or an unpriced model.** The emitted usage has `tokens_partial` or `cost_partial`; the fold keeps the flag, and the board shows `at least` (§3.6). A known cost never looks complete when a part of the usage has no cost.
14. **Recount of an imported or moved session, or of a conversation that two sessions share.** Refused `other_history` (§4.2 step 3.c): its record holds usage of other executions, which would count the same usage twice in the fleet total.
15. **Recount while a run may still exist.** A `lost` shim row without proof, a launch of the same conversation, a terminal hold, a queued resume: refused `session_running`. The per-session lock of `stop_prepare` keeps a resume, a reconcile or a stop of the session out of the recount's prepare and decision.
16. **Recount while the owner resumes the session.** The resume waits for the per-session lock; a resume decided first leaves a queued effect: `session_running`. An event in between: `session_changed`.
17. **A paired deck that is older** sees `usage.set` as an unknown kind and keeps the old total (L16), and adds new-rule and old-rule usage together. Only a deck with the route sends the command, so only an owner with a second, older deck sees this (§9, open item c).
18. **The transcript is edited by an agent** (it can write files under `~/.claude` in a permissive mode). A forged `cost-state` changes only the shown numbers of that session (threat note T3, §8).

## 6. Compatibility

| Peer | Behavior |
| --- | --- |
| New station, older deck | `usage.updated` has optional new fields; an older deck ignores them (serde ignores unknown fields of a struct variant) and adds every part, as today. A snapshot from the new station drops `usage_legacy` and `usage_recount` on the older deck. An older deck never sends `session.usage_recount`. |
| Older station, new deck | Events have no `scheme`: every total of that station is an old count on the board (it is counted with the old rules). `session.usage_recount` → 409 `not_supported`; the board counts it under "need a newer Fleet Deck". |
| Older checkpoint, new station | §5.2 (Claude), §5.3 (Codex). |
| Stored summaries | No SQL migration. A summary without `scheme` is an old count; the first new-rule event sets it apart (§3.1). A summary without `usage_recount` reads as before. |
| Web board, older deck | The new TS fields are optional; the board shows the parts that exist. |

## 7. Out of scope

1. Claude prices in the table (the CLI reports Claude's cost; DL-444).
2. `account/usage/read` thread estimates of Codex (null for the owner's account, §1.4.9).
3. The usage that a Claude process records after its last `result` (§3.3, open item a).
4. A plan type detector ("subscription" vs "API key"): the label "at API prices" and its title stay for every cost.
5. A chat line for `usage.set` (Properties shows the recount).
6. Fleet-wide history graphs of usage.
7. The `spikes/` adapters.
8. A recount of Codex sub-agent rollouts (the child rollout files of a parent thread). The recounted Codex total is marked partial instead.

## 8. Risk class: high

Reasons: a repair of stored totals (`usage.set` replaces the stored summary value on the station and the deck), a change of the meaning of a stored number (`input_tokens`, with the scheme marker), a new command path (`session.usage_recount`), a new event kind in the public schema, and the read of provider files. An adversary DESIGN REVIEW comes before the build, and a PRE-PR REVIEW before the merge.

Threat notes:

- **T1, path and file trust.** The recount opens the provider file only through the station's trusted locator (no-follow walk, owner uid, regular file, the history limits). The resume baseline in the providers crate opens one fixed path below the provider home with `O_NOFOLLOW`, checks the owner uid, the file type and the link count, and reads numbers only. Neither reads a path from the board.
- **T2, authority.** `session.usage_recount` is owner-only (board login, scope `admin`), checked by the deck and audited with a submitted row and an outcome row; the station accepts it only from a paired deck with origin `Board`, as `ReleaseShell`.
- **T3, forged records.** An agent that can write its own transcript can change the baseline or the recount of its own session. The effect is limited to the shown usage numbers of that session; no authority, file or process depends on them.
- **T4, data loss.** The recount replaces only an old count, never a new-rule total. The replaced total stays in `usage_recount.previous` in the summary (durable, not subject to event retention), and Properties shows it. An old count that is set apart at the upgrade stays in `usage_legacy`. Nothing is deleted.
- **T5, races.** The recount holds the per-session lock of `stop_prepare` (resume, reconcile and stop wait), uses the resume rule for the end of every execution of the session and its conversation, and rechecks it, the live map, the `BUSY` rule and the `session_seq` in the writer transaction (§4.2 step 3). A resume is refused until the previous execution is proven ended, so no `cost-state` is written between the baseline read and the spawn.
- **T6, denial of service.** The baseline scan has a 32 MiB cap and a 2 s deadline, the recount scan the history limits and 3 s; the board sends at most 4 recounts at a time and polls a pending command once a second for at most 30 s.

## 9. Open items after the build

- a. The usage that Claude records after the last `result` of a process (a background agent stopped with the process) is in the next restored record and is never counted by the live rule; a recount includes it.
- b. A Codex sub-agent thread replayed after a parent resume would be counted again (not observed).
- c. A second, older deck keeps the old totals after a recount and adds new-rule and old-rule usage together.
- d. Sessions of an older station stay old counts until that station is updated.
- e. A session that ran after the upgrade keeps its old count apart for good (it is not recountable: the record cannot be split at the upgrade time).
- f. A recounted total is a lower bound: killed Claude runs and Codex sub-agent threads are not in it. A conversation that the owner continued in a terminal (DL-140) is in the Claude record.

## 10. Build plan

Every Rust command runs through the queue: `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. The guards run in every task: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`. Every module stays under 800 lines.

Order: T1 → (T2 ∥ T3) → T4 → T5 ∥ T6 (T5 and T6 share no file).

### T1: protocol: usage fields, scheme, old count, `usage.set`, `session.usage_recount` (backend, about 420 lines)

- **Owned files:** `crates/fleetdeck-proto/src/event_types.rs`, `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_kinds.rs`, `crates/fleetdeck-proto/src/fold.rs`, `crates/fleetdeck-proto/src/fold_usage_tests.rs` (new), `crates/fleetdeck-proto/src/session.rs`, `crates/fleetdeck-proto/src/command.rs`, `crates/fleetdeck-proto/src/version.rs`, `crates/fleetdeck-proto/src/ts.rs`, `crates/fleetdeck-proto/src/fold_tests.rs`, `crates/fleetdeck-proto/src/fold_tests_more.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `crates/fleetdeck-providers/src/claude/map.rs` (only the `Usage` literal: `..Usage::default()`), `crates/fleetdeck-providers/src/codex/prices.rs` (only the `Usage` literal), `crates/fleetdeck-station/src/manager/decide.rs` (one arm: `SessionCommand::UsageRecount { .. } => Err(super::usage_recount::refuse_direct())`), `crates/fleetdeck-station/src/manager/usage_recount.rs` (new; in T1 only `pub fn refuse_direct() -> DecideError` that returns `reject(RejectCode::InvalidState, "usage_recount_direct: The session manager decides a usage recount.")`), `crates/fleetdeck-station/src/manager/mod.rs` (only the `mod usage_recount;` line), `web/src/session/commands.ts` (one map entry), `web/src/gen/**` (generated).
- **Changes:** §3.1 (the six `Usage` fields, `USAGE_SCHEME`, `is_empty`, `has_tokens`, `LegacyUsage`, `SessionSummary.usage_legacy`, `fold_usage` rules 1-5, the `add_usage` changes; `add_usage` becomes `pub`), §4.1 (`UsageSource`, `UsageRecount` with `command_id` and `previous`, `Event::UsageSet` and its fold, `SessionSummary.usage_recount`, `SessionCommand::UsageRecount` with `method()` and `session_id()`, `feature::USAGE_RECOUNT` in `ALL`). Export the new types to TS. `fold.rs` gets `#[cfg(test)] #[path = "fold_usage_tests.rs"] mod usage_tests;` (`fold_tests.rs` has 798 lines: no new test there).
- **Tests (in `fold_usage_tests.rs`):**
  - `cache_parts_add_and_unknown_stays_unknown`: two scheme-2 events with cache parts `Some(3)` and `None` give `Some(3)`; two `None` give `None`.
  - `the_latest_price_table_wins`: `prices` `Some("a")` then `None` then `Some("b")` gives `Some("b")`.
  - `an_empty_update_changes_nothing`: a `Usage::default()` update on an old total and on a new total leaves both unchanged and creates no `usage_legacy`.
  - `the_first_scheme_event_sets_the_old_count_apart`: an old total `{input 100, cost 1.0}`, then a scheme-2 event `{input 5, cost 0.1}` at `at = 7` → `usage == {scheme 2, input 5, cost 0.1}`, `usage_legacy == {usage: {input 100, cost 1.0}, until: 7}`.
  - `a_new_session_counts_with_the_scheme_from_the_start`: an empty summary and a scheme-2 event → no `usage_legacy`.
  - `an_old_event_after_the_change_goes_to_the_old_count`.
  - `partial_flags_stay_once_set`: `tokens_partial` and `cost_partial` OR over three events.
  - `a_cost_after_unpriced_tokens_is_partial` and `unpriced_tokens_after_a_cost_are_partial`: `{tokens, cost None}` + `{tokens, cost 0.2}` gives `cost 0.2, cost_partial`; the other order the same.
  - `usage_set_replaces_the_total_and_marks_the_recount`: after two `usage.updated` and one `usage.set`, `usage` equals the set value, `usage_legacy == None`, `usage_recount == Some({ at, source, command_id, previous })`; the rate limits are unchanged.
  - `a_usage_set_event_round_trips_and_has_its_kind`: JSON round trip, kind `usage.set`, in `Event::KINDS`.
  - `an_old_usage_json_reads_with_no_new_fields`: `{"input_tokens":1,"output_tokens":2,"cost_usd":0.1,"cost_inferred":false}` reads with the new fields `None`/`false` and writes back without them.
  - In `command.rs` tests (or a new `command_usage_tests.rs`): `usage_recount_has_its_method_and_session`.
- **Acceptance:** `cargo nextest run -p fleetdeck-proto --no-fail-fast`; `cargo check -p fleetdeck-providers -p fleetdeck-station -p fleetdeck-deck`; the guards; `cd web && bun run typecheck`.

### T2: one price table and Codex per-thread totals (backend, about 520 lines)

- **Depends on:** T1.
- **Owned files:** `crates/fleetdeck-providers/src/prices.rs` (new, from `codex/prices.rs`), `crates/fleetdeck-providers/src/codex/prices.rs` (deleted), `crates/fleetdeck-providers/src/lib.rs`, `crates/fleetdeck-providers/src/codex/mod.rs`, `crates/fleetdeck-providers/src/codex/thread_usage.rs` (new), `crates/fleetdeck-providers/src/codex/tests_usage.rs` (new), `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/src/codex/checkpoint.rs`, `crates/fleetdeck-providers/tests/codex_core_replay.rs`, `crates/fleetdeck-providers/tests/codex_driver_replay.rs`, `web/src/testing/fixtures/codex-*.events.json` (regenerated with `FD_UPDATE_FIXTURES=1`).
- **Changes:** §3.4 (with `ThreadUsage::legacy` and the restore rule) and §3.5.
- **Tests:**
  - `prices.rs`: `a_listed_model_costs_its_four_parts` (exact sum for 1,000,000 of each part of `gpt-5-codex`); `a_dated_model_id_matches_its_row` (`gpt-5-2025-08-07`); `an_unlisted_model_has_no_price` (`gpt-daybreak-blue-latest`, a Claude id).
  - `tests_usage.rs`: `the_total_grows_by_the_turn` (the three updates of `codex-run1` give deltas whose `totalTokens` sum is 43,625); `a_resume_replay_counts_nothing` (an update before `turn/started` after `thread/resume` adds nothing; the next two add 15,085 and 15,108); `a_fork_replay_counts_nothing` (the fixture shape of `fork_thread.jsonl` lines 44-58); `a_sub_agent_thread_is_counted` (the `codex-run3-subagent` updates give 42,276 tokens, the sub-agent never ends the turn); `a_lower_total_is_a_new_count`; `without_total_the_last_block_counts`; `input_is_new_input` (13,519 input with 11,264 cached → `input_tokens 2,255`, `cache_read_tokens 11,264`); `every_update_has_scheme_2`; `an_unlisted_model_is_cost_partial`; `the_usage_state_survives_a_checkpoint`.
  - Legacy checkpoint (`tests_usage.rs`): `a_legacy_checkpoint_restored_during_a_turn_counts_the_next_call` (a version-1 checkpoint JSON without `usage`, with a running turn; the next update with `total` 30,000 and `last` 15,000 adds 15,000, and the one after it adds the growth of `total`); `a_legacy_checkpoint_restored_between_turns_does_not_count_the_thread_total` (no running turn; `turn/started`, then an update with `total` 60,000 and `last` 15,100 adds 15,100); `a_legacy_sub_agent_update_counts_its_last_block`.
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/codex/) | test(/prices/) | binary(codex_core_replay) | binary(codex_driver_replay)'`; the guards.

### T3: Claude per-model totals and the resume baseline (backend, about 600 lines)

- **Depends on:** T1. Runs in parallel with T2.
- **Owned files:** `crates/fleetdeck-providers/src/claude/process_usage.rs` (new), `crates/fleetdeck-providers/src/claude/cost_state.rs` (new), `crates/fleetdeck-providers/src/claude/tests_process_usage.rs` (new), `crates/fleetdeck-providers/src/claude/mod.rs`, `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/checkpoint.rs`, `crates/fleetdeck-providers/src/claude/driver.rs`, `crates/fleetdeck-providers/src/claude/durable_launch.rs`, `crates/fleetdeck-providers/src/claude/tests.rs`, `crates/fleetdeck-providers/src/claude/tests_usage.rs`, every other file with a `claude::CoreConfig` literal (only the added `usage_base: UsageBase::Zero` line: `crates/fleetdeck-providers/tests/*.rs`, `crates/fleetdeck-station/src/manager/*` test files, `crates/fleetdeck-cli/tests/shim_*.rs`), `web/src/testing/fixtures/claude-*.events.json` (regenerated).
- **Changes:** §3.2 and §3.3. The two tests of `0e6c3600` in `tests_usage.rs:159-202` are replaced by the tests below (the rule "a resumed process adds 0" is wrong).
- **Tests (`tests_process_usage.rs`, frames from `~/fleetdeck-briefs/backlog/usage-cost/claude/`):**
  - `one_process_adds_the_growth_per_model`: the four results of `claude-run1` sum to its last `modelUsage` (18 input, 178 output, 49,743 cache read, 48,491 cache write, $0.006649605).
  - `a_subagent_appears_in_the_next_result`: result 3 adds 2 input and 4 output more than its `result.usage`.
  - `a_resume_counts_from_the_restored_record`: base `Restored(cost-state 1)`, the two results of `claude-run2-resume` add $0.00018825 and $0.00031662 (sum with run 1: $0.007154475).
  - `a_resume_after_a_kill_counts_from_the_older_record`: base `Restored(cost-state 3)`, the first result of `claude-run5-after-kill` adds $0.00018895.
  - `an_unknown_base_counts_the_turn_tokens_and_no_cost`: base `Unknown`, the first result gives the `result.usage` tokens, `tokens_partial`, `cost_usd: None`, `cost_partial`; the second adds the growth with no flag.
  - `a_legacy_checkpoint_counts_the_cost_growth`: a checkpoint JSON with `last_total_cost: 0.5` and no `usage`; a result with total 0.8 adds 0.3, the turn tokens and `tokens_partial`.
  - `fallback_results_advance_the_cost_cursor`: base `Zero`, two results without `modelUsage` with totals 0.10 and 0.20 add 0.10 and 0.10.
  - `alternating_shapes_do_not_count_twice`: base `Zero`; results with `modelUsage` (M1), without (total 0.3), with (M2): the sum of the three deltas has cost = the last `total_cost_usd`, and tokens = M1 tokens + the turn tokens of results 2 and 3.
  - `every_delta_has_scheme_2`.
  - `a_new_model_counts_from_zero` and `a_lower_model_total_is_a_new_count`.
  - `the_usage_state_survives_a_checkpoint`.
  - `cost_state.rs`: `the_last_record_of_the_session_wins` (two records and a record of another session id); `no_record_is_a_zero_base`; `a_missing_file_is_unknown`; `a_link_is_unknown` (a symlink in place of the file); `a_record_past_the_limit_is_unknown` (`max_scan` 1 KiB); `project_dir_name_replaces_every_non_alphanumeric_character`.
  - `durable_launch.rs`: `a_durable_resume_reads_the_restored_base` (a temp `CLAUDE_CONFIG_DIR` with a transcript) and `a_new_durable_launch_has_a_zero_base`.
- **Acceptance:** `cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/claude/) | binary(claude_core_replay) | binary(claude_driver_replay)'`; `cargo check -p fleetdeck-station -p fleetdeck-cli --tests`; the guards; live check (architect, once): a Fleet Deck Claude session on Lighting with one turn, Stop, Resume, one turn: the Properties cost equals the last `cost-state` `totalCostUSD` of its transcript.

### T4: station recount (backend, about 650 lines)

- **Depends on:** T1, T2, T3.
- **Owned files:** `crates/fleetdeck-station/src/manager/usage_recount.rs` (new), `crates/fleetdeck-station/src/manager/usage_recount_tests.rs` (new), `crates/fleetdeck-station/src/manager/submit_route.rs` (new), `crates/fleetdeck-station/src/manager/mod.rs`, `crates/fleetdeck-station/src/manager/decide.rs`, `crates/fleetdeck-station/src/manager/stop_plan.rs` (one match arm in `stop_prepare`), `crates/fleetdeck-station/src/store/usage_recount.rs` (new: `RecountDb`, `Decision::recount_db`, §4.2 step 4), `crates/fleetdeck-station/src/store/mod.rs` (the `mod` line), `crates/fleetdeck-station/src/store/resumes.rs` (only `const BUSY` → `pub(super) const BUSY`), `crates/fleetdeck-station/src/history/usage_record.rs` (new), `crates/fleetdeck-station/src/history/mod.rs` (the `mod` line), the station feature list file (where `feature::SHELL_COMMAND` is advertised).
- **Changes:** §4.2.
- **Tests (`usage_recount_tests.rs`, `history/usage_record.rs` tests):**
  - `a_claude_session_takes_the_transcript_total` (temp home, a transcript with two `cost-state` records, an old-count summary → `usage.set` with the last record, scheme 2, both partial flags, source `claude_transcript`, `previous` = the old summary usage, `command_id` = the command's).
  - `a_codex_session_prices_each_interval_by_its_model` (a rollout with `turn_context` `gpt-5`, one `token_count`, `turn_context` `gpt-5-mini`, a second `token_count` → the cost is the first interval at `gpt-5` plus the growth at `gpt-5-mini`).
  - `an_unlisted_interval_keeps_the_known_cost_partial` (`gpt-5` then `gpt-daybreak-blue-latest`: cost of the first interval, `cost_partial`); `an_unlisted_model_gives_an_unknown_cost` (only `gpt-daybreak-blue-latest`: `cost_usd: None`).
  - `a_token_count_without_info_is_skipped`.
  - `no_record_sets_an_unknown_total` (no file → scheme 2, all parts `None`, source `none`).
  - `an_untrusted_file_is_refused` (a symlink → `record_unreadable`, no event).
  - `a_new_rule_total_is_refused` (`summary.usage.scheme == Some(2)` → `counted_since_upgrade`, no event); `an_old_count_set_apart_is_refused` (`usage_legacy` set → `counted_since_upgrade`).
  - `an_imported_session_is_refused` (a `session_transcripts` row → `other_history`); `a_moved_session_is_refused`; `a_shared_conversation_is_refused` (two sessions with one native id).
  - `a_live_session_is_refused` (`session_running`); `a_lost_row_without_proof_is_refused` (a shim row `lost` with reason `adapter_checkpoint_unreadable` → `session_running`); `a_queued_resume_is_refused`; `a_terminal_hold_is_refused`.
  - `a_session_that_changed_is_refused` (an event appended between prepare and decide → `session_changed`).
  - `another_provider_is_refused` (`unsupported_provider`).
  - `the_recount_waits_for_the_session_lock` (a held `stops().locks` for the session delays the decision).
  - `the_direct_decide_refuses_a_recount`.
  - `the_station_advertises_usage_recount`.
- **Acceptance:** `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(/usage_recount/) | test(/usage_record/) | test(/decide/) | test(/resumes/) | test(/stop_plan/)'`; the guards.

### T5: deck route rules and recount audit (backend, about 220 lines)

- **Depends on:** T1 (T4 for the end-to-end test).
- **Owned files:** `crates/fleetdeck-deck/src/http/methods_table_rest.rs`, `crates/fleetdeck-deck/src/http/command_routes.rs`, `crates/fleetdeck-deck/src/http/recount_audit.rs` (new), `crates/fleetdeck-deck/src/http/mod.rs` (the `mod recount_audit;` line), `crates/fleetdeck-deck/tests/board_usage_recount.rs` (new).
- **Changes:** §4.3.
- **Tests (`board_usage_recount.rs`):** `a_token_cannot_recount` (403 `owner_only`, one audit row `refused` with target `<station>/<session>` and `stage=refused code=owner_only`); `a_station_without_the_feature_answers_not_supported` (409, refusal row with the session target); `the_owner_recount_is_audited_and_folded` (a fake station that accepts and sends a `usage.set` event; the deck summary has the new usage and `usage_recount`; rows `stage=submitted` (`ok`) and `stage=outcome` (`ok`), both with target `<station>/<session>`); `a_station_refusal_gets_a_refused_outcome_row` (the fake station rejects with `session_running: …` → the outcome row is `refused` with `code=session_running`); `an_unanswered_recount_gets_outcome_unobserved` (the fake station never answers, `stop_watch.limit` 200 ms → the outcome row is `failed` with `stage=outcome_unobserved`); `a_duplicate_command_writes_no_second_outcome`.
- **Acceptance:** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'binary(board_usage_recount) | test(/methods/)'`; the guards.

### T6: board (ux, about 600 lines)

- **Depends on:** T1 (types), T5 (route behavior). Load the `fleet-design-system` skill first.
- **Owned files:** `web/src/board/usage.ts`, `web/src/board/UsageMeters.tsx`, `web/src/board/UsageGlance.tsx`, `web/src/board/UsageRecount.tsx` (new), `web/src/board/usageRecountCall.ts` (new), `web/src/board/usageRecount.test.tsx` (new), `web/src/board/usageRecountCall.test.ts` (new), `web/src/board/usage.test.tsx`, `web/src/board/fleet.ts`, `web/src/board/fleet.test.tsx`, `web/src/board/pairing.ts`, `web/src/board/HomePanel.tsx`, `web/src/app/FleetHome.tsx`, `web/src/App.tsx` (one prop; the file has 789 lines), `web/src/runtime/RuntimeApp.tsx`, `web/src/session/PropertiesPanel.tsx`, `web/src/session/properties.test.tsx`, `web/src/compare/ComparePage.tsx`, `web/src/compare/ComparePage.test.tsx`, `web/src/testing/summary.ts`, `web/src/styles/home.css` (only if a layout rule is needed; tokens only).
- **Changes:** §3.6 and §4.4, with the exact texts there.
- **Tests:**
  - `usage.test.tsx`: `split_usage_of_a_new_an_old_and_a_mixed_session`; `fleet_usage_sums_only_new_rule_usage_and_cache_parts`; `an_old_count_session_is_unknown_and_a_mixed_one_partial`; `total_text_says_at_least_for_partial_parts` (the four forms of §3.6 item 3); `cost_text_says_at_least_for_a_partial_cost`; `the_input_label_is_new_input_for_every_provider`; `an_inferred_cost_title_names_the_price_table`.
  - `properties.test.tsx`: `the_tokens_row_has_four_parts_in_order`; `a_missing_part_is_left_out`; `a_partial_tokens_row_says_at_least`; `an_old_count_row_for_an_old_session`; `an_old_count_row_with_its_date_for_a_mixed_session`; `a_recounted_session_shows_recounted_and_the_previous_cost`; `a_session_without_record_shows_no_record`.
  - `usageRecountCall.test.ts`: `an_accepted_receipt_is_accepted`; `a_rejected_receipt_keeps_its_code` (`session_running: This session …` → code `session_running`); `not_supported_is_its_own_result`; `a_pending_command_is_polled_until_done`; `a_pending_command_past_the_limit_stays_pending`; `failed_and_ambiguous_are_failed`.
  - `usageRecount.test.tsx`: `the_row_is_absent_without_old_counts`; `the_row_is_absent_without_the_call` (a member or a token login); `cancel_sends_nothing`; `recount_sends_one_command_per_ended_old_count_session_and_skips_running_ones`; `a_result_waits_for_the_matching_summary` (accepted, then the `sessions` prop gets `usage_recount.command_id` → counted under its source); `the_result_line_counts_each_outcome` (claude source, `none`, waiting, running, `other_history`, 409, failed); `at_most_four_commands_run_at_once`.
- **Acceptance:** `cd web && bun run test src/board src/session/properties src/compare` (through `heavy.sh`); `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; the guards; gallery shots of the Usage tab (with and without old counts) and Properties (new, mixed, recounted) in both themes, desktop and phone.

## 11. Decisions made in this draft

1. **Deltas stay; running totals are the source** (DL-440). Rejected: store the last running total per process or thread in the summary (changes the fold contract for older decks, loses a killed process, counts a fork's parent twice).
2. **`input_tokens` means new input for both providers** (DL-441). Rejected: keep each provider's meaning and the `inputLabel` switch (the same word meant two counts).
3. **Claude source is `result.modelUsage` per model, with a separate cost cursor** (DL-442). Rejected: `result.usage` per turn (no subagents, no cost); `total_cost_usd` alone (no tokens per kind); one cursor for tokens and cost (a frame without `modelUsage` would stop the cost cursor and count costs twice).
4. **The Claude resume baseline is the last `cost-state` of the transcript, read by the adapter before the spawn** (DL-443). Rejected: the owner proposal to give the stored cost to the CLI (no such option); "the first resumed result adds 0" (HEAD, loses a turn); a station-stored "last counted" value (wrong after a `SIGKILL`, and the events are pruned after 7 days).
5. **The baseline lives in the checkpointed core state, not in the config** (DL-443). Rejected: `config.resumed` (lost on a shim restore, §1.2.4).
6. **An unreadable baseline counts the turn tokens and no cost for the first result, marked partial.** Rejected: count the whole total (a large overcount); count nothing (B21 again); no mark (a later cost looks complete).
7. **Codex source is `tokenUsage.total` per thread, with the pre-turn replay as a base** (DL-445). Rejected: `last` (counts the replay of every resume and fork; misses a dropped notification).
8. **Codex sub-agent threads are counted and priced at the session model** (DL-445). Rejected: drop them (HEAD); price them by a model that the core does not know.
9. **One price table in `fleetdeck-providers/src/prices.rs`, Codex rows only, dated, cache rates added** (DL-444). Rejected: Claude rows (the CLI prices Claude, and a second price would disagree); a price table in the deck (the station infers at the source, as DL-161).
10. **No new row for `gpt-daybreak-blue-latest`; the `gpt-5.6-luna` placeholder stays** (owner question 1). Rejected: invent a rate.
11. **B20 is an automatic separation of old counts plus an owner recount of whole old counts, with a new `usage.set` event** (DL-447, DL-449). Rejected: a SQL migration that recomputes from stored events (the events are gone or wrong, §1.6.8); a reset to zero (false); a reset to unknown for every session (loses correct data that the provider has); a recount that replaces new-rule totals (it loses killed runs and Codex sub-agents that live counting counted, review round 1).
12. **The recount is an owner command per session through the generic command route, run from the Usage tab, with a typed board call that polls and a deck outcome audit row** (DL-448). Rejected: an automatic run at upgrade (changes stored data without the owner); a new deck bulk route (a second audit and receipt path); `sessionActions(...).run` (private, loses the delivery state and the codes).
13. **A session without a provider record becomes "unknown"** (owner question 2). Rejected: keep the old, too high total.
14. **A session is recounted only when every execution of it and of its conversation is proven ended, under the session lock** (the resume rule). Rejected: the live map alone (a `lost` shim can still drive its provider).
15. **An imported or moved session, or a shared conversation, is not recounted** (DL-447, execution scope). Rejected: include the conversation before the import (counts the same usage twice in the fleet total); subtract an unknown import-time value.
16. **Every recounted total is a lower bound (`tokens_partial`, `cost_partial`).** Rejected: claim an exact total (a killed Claude run and Codex sub-agents are not in the record, and the station cannot prove a clean exit).
17. **The Codex recount prices each `token_count` interval at the model of the `turn_context` before it.** Rejected: the last model for the whole total (a model switch re-prices earlier turns).
18. **The new rules carry `scheme: 2`, and the fold never mixes old and new parts** (DL-449). Rejected: a silent meaning change of `input_tokens` (an old Codex count includes the cached input); mark every old total unknown (loses the old number that the owner may want to see).
19. **"At API prices" stays the cost label; no plan detection** (DL-446). Rejected: a "subscription" word from `apiKeySource` (the board has no reliable plan fact).
20. **No chat line for `usage.set`.** Rejected: a timeline line (noise in the chat; Properties shows it).
21. **The recount scans the whole provider file under a 3 s deadline; the live baseline scans at most 32 MiB in 2 s.** Rejected: one shared limit (the live path is on the resume path and must stay short); 5 s (equal to the deck's receipt wait).

## 12. Owner questions

1. **Codex prices.** Your Codex sessions use `gpt-daybreak-blue-latest`, which has no price, so they show tokens and no cost. Do you want a price for it (and are the placeholder rates of `gpt-5.6-luna` correct)? *Recommended default:* no row for `gpt-daybreak-blue-latest` (cost unknown), keep the `gpt-5.6-luna` placeholder as DL-161 decided.
2. **Sessions without a provider record.** When the recount finds no transcript or rollout (for example a session whose files were deleted), should the stored total become unknown, or keep the old (too high) value? *Recommended default:* unknown.
3. **Where the recount button sits.** The Usage tab of the fleet home, or Settings → each station? *Recommended default:* the Usage tab (one action for the fleet).

## Decision log entries (to append)

## DL-440: Usage events stay deltas; provider running totals are the source (B14) (design agent, 2026-10-23)

- `usage.updated` keeps the delta contract of `fold.rs` `add_usage`. Each adapter keeps the last running total of its process (Claude, per model) or thread (Codex) and emits the growth. Older decks stay correct. A session total is the usage of the executions that Fleet Deck ran for it. Design `docs/v2/design/usage-cost.md` §3.

## DL-441: `input_tokens` is new input for both providers; cache reads and writes are separate counts (B14) (design agent, 2026-10-23)

- `Usage` gets `cache_read_tokens`, `cache_write_tokens` (optional). Codex input = `inputTokens − cachedInputTokens − cacheWriteInputTokens`. The board says "new input", "cache read", "cache write", "output". The meaning applies to usage with `scheme: 2` only (DL-449). Design §3.1, §3.6.

## DL-442: Claude usage comes from `result.modelUsage` per model (B14) (design agent, 2026-10-23)

- The cumulative per-model totals contain subagents, background agents and the cache, and their `costUSD` is Claude's own list price. A separate cost cursor moves at every result. `result.usage` is used only when the token base is not known or `modelUsage` is absent, and such a delta is marked `tokens_partial`. Replaces the cost rule of `0e6c3600`. Design §3.2.

## DL-443: The Claude resume baseline is the last `cost-state` record of the transcript (B21) (design agent, 2026-10-23)

- The CLI restores its totals from the last `cost-state` record (written at a clean exit, not at `SIGKILL`; a fork restores the parent's). The adapter reads it before the spawn (no-follow, owner uid, 32 MiB, 2 s) and stores it in the checkpointed core state. Unreadable: the first result counts the turn tokens and no cost, marked partial. Fixes the "resumed first result adds 0" rule of `0e6c3600` and the shim-restore overcount. Design §3.3.

## DL-444: One dated price table for inferred costs, Codex rows only (B14) (design agent, 2026-10-23)

- `crates/fleetdeck-providers/src/prices.rs`, `PRICE_TABLE_VERSION = "2026-10-23"`, rates per 1M tokens for input, cache read, cache write and output. An inferred cost carries the table date in `Usage.prices`. No Claude rows. Design §3.5.

## DL-445: Codex usage comes from `tokenUsage.total` per thread, sub-agents included (B14) (design agent, 2026-10-23)

- An own-thread update before the first `turn/started` of a process (the resume or fork replay) is a base and adds nothing. Sub-agent threads add their own growth, priced at the session model. A core restored from an older checkpoint counts the `last` block of the first update of each thread. Design §3.4.

## DL-446: Every cost stays "at API prices"; no plan detection (B14) (design agent, 2026-10-23)

- The existing label and title ("A subscription does not bill it") are the estimate text. Design §2 (B14.4).

## DL-447: Old counts are recounted from the provider record with a `usage.set` event (B20) (design agent, 2026-10-23, revision 2)

- The events cannot rebuild the totals (7-day retention, 500 deck rows, old meanings). Only a total that is entirely an old count is recounted, and only for a session whose conversation started in it on this station (no import, no move, no shared conversation) and whose every execution is proven ended (the resume rule, under the session lock). The station reads the last Claude `cost-state` or prices the Codex rollout interval by interval through the trusted history locator and appends `usage.set { usage, previous, source, command_id }`; the folds replace and keep `previous` in the summary. A recounted total is a lower bound. A session without a record becomes unknown. Design §4.

## DL-448: The recount is an owner command per session, `session.usage_recount`, feature `usage_recount.v1` (B20) (design agent, 2026-10-23, revision 2)

- Generic command route, scope `admin`, owner login only, with a submitted and an outcome audit row per session. The board runs it from the Usage tab, four at a time, after a confirm, with its own typed call that polls a pending command and matches the summary by command id. Design §4.2-§4.4.

## DL-449: Usage accounting scheme 2; old counts are set apart; partial totals are marked (B14, B20) (design agent, 2026-10-23, revision 2)

- Every usage of the new rules carries `scheme: 2`. The fold never adds new-rule and old-rule parts together: the first new-rule event moves an old total into `SessionSummary.usage_legacy`, and a later old-rule event goes there too. `tokens_partial` and `cost_partial` mark a lower bound; the fold keeps them and marks a cost partial when a part with tokens has no cost. The board sums only new-rule usage, says "at least" for partial parts and shows the old count apart. Design §3.1, §3.6.

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Your Codex sessions run gpt-daybreak-blue-latest, which has no price, so they show tokens but no cost. Do you want to give rates for it, and are the gpt-5.6-luna placeholder rates (gpt-5 rates) correct? — **The recommended default: No row for gpt-daybreak-blue-latest (cost unknown); keep the gpt-5.6-luna placeholder per DL-161**
2. When the recount finds no provider record for a session (transcript or rollout deleted), should its stored total become unknown or keep the old, too-high value? — **The recommended default: Unknown**
3. Where should the Recount button sit: the Usage tab of the fleet home, or Settings for each station? — **The recommended default: The Usage tab (one fleet-wide action)**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge. (Revision 2: this track still needs no migration; V47 and V50 stay unused.)

## Revision 2 (design review round 1)

Report: `~/fleetdeck-briefs/backlog/usage-cost/adv-design-r1.md` (verdict FAIL, 2 BLOCKER, 9 MAJOR). Each finding was checked against the code on `00055138`.

| # | Finding | Verified | What changed |
| --- | --- | --- | --- |
| 1 | BLOCKER: a Claude recount replaces a correct live total with a record that misses a killed run; `previous` is lost through retention. | Yes (§1.4.4; `store/log.rs` prune; `TIMELINE_CAP`). | The recount replaces only a total that is entirely an old count (§4.2 step 3.b, `counted_since_upgrade`); a live-counted total is never replaced. A recounted total is always marked as a lower bound, because the station cannot prove a clean exit of every Claude run (a shim `exited` row records no signal). `previous` is kept durably in `SessionSummary.usage_recount.previous`, and an old count set apart at the upgrade stays in `usage_legacy`. |
| 2 | BLOCKER: a Codex recount deletes counted sub-agent usage. | Yes (§1.4.8; HEAD drops other threads, `codex/map.rs:368`). | Same rule: a new-rule total, which is the only kind that contains sub-agent usage, is never recounted. An old Codex count never contained sub-agent usage (HEAD and `7333db5d` dropped it). The recounted Codex total is marked partial, and a recount of child rollouts is out of scope (§7.8). |
| 3 | MAJOR: the recount changes the scope and counts fork ancestry twice. | Partly: `session.fork` is refused by the station (`decide.rs:136`), but an import, a move and two sessions on one conversation have the same problem. | The scope is stated (execution scope, §2 B14.2). The recount refuses `other_history` for an import (`session_transcripts` row), a move (`moved_to`, `move_in`) and a shared native id (§4.2 step 3.c). §5.10 of revision 1 (include the earlier conversation) is removed. |
| 4 | MAJOR: the live-map check does not prove that the execution ended. | Yes (`manager/shims.rs:215` keeps a shim alive and marks it `lost`; `store/resumes.rs:1-19`). | The recount uses the resume rule: `execution_exclusion` with `Purpose::Resume` and reboot revalidation in the prepare; the same `exclusion` (own rows, other sessions and launches of the conversation, move holds), `native_session_held`, the `BUSY` rule and the reboot-row match in the decision (`RecountDb`); and the per-session lock of `stop_prepare` against resume, reconcile and stop (§4.2 steps 1-3). |
| 5 | MAJOR: an older Codex checkpoint gets an invalid usage state. | Yes (`codex/checkpoint.rs:59` restores without reopening the thread). | `CodexCore.usage` is `Option<ThreadUsage>`; `restore` maps `None` to `ThreadUsage::legacy()`, which counts the `last` block of the first update of each thread (§3.4 rule 2, §5.3). T2 tests the upgrade during a turn and between turns. |
| 6 | MAJOR: the Claude fallback never moves its cost cursor. | Yes (`claude/map.rs:540` moves it today; rule 4 of revision 1 did not). | `ProcessUsage.cost_cursor` moves at every result with a cost; a result without `modelUsage` clears the token base, so alternating shapes count nothing twice (§3.2 rules 2-5, §5.7). T3 tests both cases. |
| 7 | MAJOR: the Codex recount prices all history at the last model. | Yes (`codex/map.rs:308` changes the model per turn). | `usage_record::codex_usage` scans forward and prices each `token_count` interval at the model of the `turn_context` before it; an unlisted interval keeps the cost partial (§4.2, Codex record). |
| 8 | MAJOR: a missing cost becomes a complete-looking total. | Yes (`fold.rs:367-372`, `web/src/board/usage.ts:85`). | `Usage.tokens_partial` and `Usage.cost_partial`; the fold keeps them and marks a cost partial when a part with tokens has no cost (§3.1). The board says `at least` (§3.6 items 2, 3, 6, 7). |
| 9 | MAJOR: the new meaning of `input_tokens` mixes with old totals. | Yes (`codex/prices.rs:61`, `fold.rs:374`). | `Usage.scheme` (`USAGE_SCHEME = 2`) and `SessionSummary.usage_legacy`: the fold sets an old total apart at the first new-rule event and never adds the two (§3.1 fold rules, DL-449). This works for a running session too, with no repair. The board sums only new-rule usage and shows the old count apart. |
| 10 | MAJOR: the board command API cannot do the recount flow. | Yes (`commands.ts:232` `run` is private; `outcomeOf` treats `pending` as ok; `RECEIPT_WAIT` 5 s at `command_routes.rs:38`). | A typed `usageRecountCall` with the delivery state and the reject code, polling of `GET /api/commands/<id>`, the wiring from `RuntimeApp` to `UsageMeters`, and a result line that matches `summary.usage_recount.command_id` (§4.1, §4.4). The station scan deadline is 3 s, under the 5 s receipt wait (§4.2 step 2.e, §4.3 item 4). |
| 11 | MAJOR: the generic audit row does not record the outcome. | Yes (`command_routes.rs:67-73` writes `Ok` with the station target before the wait at line 85). | `recount_audit.rs`: the generic rows get the session target and `stage=submitted` / `stage=refused`; a watcher writes one `stage=outcome` row from the terminal receipt or delivery state, or `stage=outcome_unobserved` (§4.3 item 3). T5 tests refusal, timeout and duplicate. |

Other changes for consistency: §2 (B20 answers), §5 (new edge cases 2-4, 7, 13-15), §6, §7.8, §8 (T2, T4, T5, T6), §9 (items d-f), §10 (owned files, tests and acceptance of T1-T6; task sizes), §11 (decisions 3, 6, 11, 12, 14-18, 21), DL-441, DL-442, DL-443, DL-445, DL-447, DL-448, and the new DL-449 (the last number of this track's block DL-440..449). No task id changed; no task split.

Findings not fixed: none. Every finding was a BLOCKER or a MAJOR with confidence 0.8 or no stated confidence; all eleven are resolved above. One part of finding 3 is not done as proposed: the design does not keep a separate "conversation-history total" next to the execution total. It refuses the recount for sessions whose record holds other executions instead, which keeps one scope for every total.
