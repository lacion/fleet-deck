# The reasoning effort next to the model (B18)

- **Status:** Draft, revision 3 (2026-10-23), after the adversary DESIGN REVIEW rounds 1 and 2 (the changes are listed at the end). Code facts are on `v2` HEAD `497b2fbc`, checked again on `00055138`.
- **Owner request (B18):** "wherever the board shows the model of a session, it also shows the reasoning effort (for example `Opus 5.5 · high`)".
- **Scope:** Phase 1 shows the effort (read only). Phase 2 lets the owner choose the effort at spawn and in a session (it writes). Phase 2 is a separate build with its own review (§9).
- **Risk:** Phase 1 is normal, except task E2 (high: the Claude answer that carries the effort also carries the settings `env` values, credentials included; §8). Phase 2 is high (a new command path).
- **Evidence:** live recordings of `claude` 2.1.308 and `codex` 0.163.0 in `/home/luismorales/fleetdeck-briefs/backlog/effort/recordings/` (scrubbed; the drivers are the `drive_*.py` scripts beside them). The Codex 0.163.0 app-server schema is in `.../effort/scratch/codex-schema/`.

## 1. Problem

The board shows the model of a session in 10 places, but it never shows the reasoning effort. The owner cannot see if a session runs at `low` or at `max`. The station does not know the effort either: no event and no summary field carries it.

## 2. Verified facts (HEAD `497b2fbc`)

### 2.1 Protocol and fold

1. `SessionSummary` has `mode` and `model` and no effort field (`crates/fleetdeck-proto/src/session.rs:87-88`).
2. The model event is `model.changed`, not `session.model_changed` (`crates/fleetdeck-proto/src/event.rs:463-464`, `event_kinds.rs:96`). The mode event is `mode.changed` (`event.rs:461-462`).
3. The fold sets `summary.model` from `model.changed` (`fold.rs:153`) and from `session.started` (`fold.rs:65`).
4. `Event::KINDS` has 61 entries (`event_kinds.rs:8`). The test `every_kind_has_a_sample_and_round_trips` compares the samples with `KINDS` in order (`event_tests.rs:376-379`).
5. `Event::kind_name` (`event_kinds.rs:75-140`) and `SessionSummary::apply` (`fold.rs`) match every variant without a wildcard: a new variant needs an arm in both.

### 2.2 Codex provider

1. `SessionSpec.effort` and `CodexAdapterConfig.effort` exist (`codex/mod.rs:196`, `codex/driver.rs:90-91`). Only tests set them (`tests/live_smoke.rs:191,280,685`, `station/src/manager/coordinator_live_tests.rs:161`, `cli/tests/live_bridge.rs:244`). In production the value is always `None` (`driver.rs:123`, `driver.rs:285`, `durable_launch.rs:61`).
2. **Correction of the backlog:** the effort is **not** sent on `thread/start`. `open_thread` sends no effort (`codex/core.rs:317-350`). Only `turn/start` sends it, as `effort` (`core.rs:400-402`) and inside `collaborationMode.settings.reasoning_effort` (`core.rs:416`). `set_collaboration` has no production caller.
3. The thread answer handler `on_thread` reads `model` and ignores `reasoningEffort` (`codex/map.rs:224-259`). One handler serves `thread/start`, `thread/resume` and `thread/fork` (`map.rs:160`).
4. `thread/settings/updated` is in `IGNORED_NOTIFICATIONS` (`map.rs:31`).
5. A model change is emitted as `model.changed` when the next turn is accepted (`map.rs:307-311`).
6. `codex/map.rs` has 768 lines and `codex/core.rs` 782 lines; the module limit is 800 (`tools/module-size.sh`). New code goes into a new file.

### 2.3 Claude provider

1. The Claude provider has no effort code. `Launch` has no effort and `cli_args` passes no `--effort` (`claude/args.rs:69-115`).
2. The `init` frame sets the model and the mode only (`claude/map.rs:107-110`). Claude sends `init` again for each turn, and a changed model in a later `init` emits no event (`map.rs:117-120`). This is a separate gap (§10.3).
3. The host already uses a polled control request for the plan limits: `get_usage` after the first `init` and after a `result`, at most one open, at least 5 min apart, stopped at the first error (`claude/usage.rs:23-41`, `map.rs:153`, `map.rs:579`). Its answer is routed before the generic error branch (`map.rs:688-690`), so an older CLI that refuses it adds no error to the chat.
4. The Claude driver writes every stdout frame (except token deltas) to the raw-frame log before it maps the frame (`claude/driver_io.rs:131-140`). The log is a file under the station data directory (`rawlog.rs:1-15`, `station/src/manager/adapters.rs:59-73`). It has no redaction.
5. `claude/map.rs` has 743 lines, `claude/core.rs` 685, `claude/tests.rs` 794.
6. **Checkpoint.** `ClaudeCore` derives `Serialize`/`Deserialize`, and its whole state (with `host_requests`) is the adapter checkpoint, version 1 (`claude/core.rs:164`, `claude/checkpoint.rs:17-20,39-46`). The version check reads only the version (`providers/src/checkpoint.rs:84-93,111-115`); a state that does not decode fails the restore, and the station marks the shim lost (`station/src/manager/shim_station.rs:220`). `ClaudeCore` has no `deny_unknown_fields`: a build ignores a key that it does not know, and a field with `#[serde(default)]` decodes from an older state. A new `HostRequest` variant does not decode in an older build. `usage_asked: Option<Instant>` is `#[serde(skip)]` (`core.rs:207-210`). The Codex core has the same model (`codex/core.rs:34-35`, `codex/checkpoint.rs:19-22`).
7. **Models.** The `initialize` answer has per model `value` and `resolvedModel` (`value: "haiku"`, `resolvedModel: "claude-haiku-5-5"`, `recordings/claude-stream-effort-low.jsonl` line 5), and `init` reports the resolved id (`"model": "claude-haiku-5-5"`, line 7). `model_choices` keeps only `value` (`claude/map.rs:647-662`); `advertised_models` keeps both in a flat list (`map.rs:666-680`). The `get_settings` answer has `applied.model` as the resolved id, with a context suffix when there is one (`claude-opus-5-5[1m]`, `claude-sonnet-5-5` after `set_model sonnet`). No advertised `value` has a `[1m]` suffix.

### 2.4 Board

`shortModel` (`web/src/ui/format.ts:22`) renders the model in 10 places, not 2 as the backlog says:

| Place | File:line |
| --- | --- |
| Session header (`Who`) | `web/src/session/HeaderBadges.tsx:85` |
| Composer model chip | `web/src/session/SessionView.tsx:642` |
| Properties, Model row | `web/src/session/PropertiesPanel.tsx:503` |
| Sidebar session row | `web/src/app/Sidebar.tsx:145` |
| Fleet strip (the session cards of the fleet home) | `web/src/board/FleetStrip.tsx:230` |
| Needs-you rail | `web/src/board/NeedsYouRail.tsx:224` |
| Compare page | `web/src/compare/ComparePage.tsx:228` |
| Wall race pane | `web/src/wall/RacePane.tsx:60` |
| Model change notice in the chat | `web/src/chat/notices.ts:82` (from `session/timeline.ts:506-509`) |
| Session settings model menu, settings field editor | `session/SessionSettings.tsx:56,149`, `settings/FieldEditor.tsx:592` (model choosers, not facts of a session) |

- The wall tiles show no model. A wall display gets a card from an allowlist without the model (`crates/fleetdeck-deck/src/http/display_view.rs:60-96`), so the race pane on a display shows "default" (§10.4).
- `SessionView.tsx` has 797 lines and `app/Sidebar.tsx` 774: an edit there must not add lines.

### 2.5 Station command path (for Phase 2)

- `session.set_model`: `SessionCommand::SetModel` (`proto/src/command.rs:192-195`), `decide.rs:300-302`, `EffectPayload::SetModel` (`station/src/store/payload.rs:174`), the confirm on `model.changed` in `observe` (`store/effects.rs:724-729`), `target_base.rs:292`, `shim_claude.rs:84`, `shim_codex.rs:83`, the deck table `methods_table_rest.rs:500`.
- A resume and a shim restore build the `SessionSpec` from `summary.model` (`manager/resume.rs:247-256`, `manager/shim_configs.rs:16-25`). No other station path copies `summary.model` into a spec.
- `SessionCommand::Import` carries a `SessionSpec` too (`command.rs:160-167`, `station/src/manager/decide.rs:109-125`). `MoveIn` carries no spec (`command.rs:317-330`).
- The import pin is checked twice: at HTTP admission (`deck/src/http/command_routes.rs:202-221`) and at every send and resend (`deck/src/link/router.rs:449-472`, `Negotiated` sends, `Down` waits, `Without` settles the command as refused).
- The station confirms an interrupt by its effect id: the host passes `ControlId(effect_id)` (`target_base.rs:272`) and the provider echoes it in `interrupt.result` (`store/effects.rs:698-720`). The station appends its own facts in the start decision (`session.batch`, `session.issue`, `decide.rs:594-607`).
- The station `decide` match on `SessionCommand` has no wildcard (`decide.rs:68`): a new command variant does not build without its arm.
- The Codex `HostRequest::TurnStart` keeps only `input_id` (`codex/core_types.rs:82-84`); `on_turn_accepted` takes `pending_mode` and `pending_model` at the answer (`codex/map.rs:300-311`), not at the send.
- `ModelChoice` literals are in `proto/src/event_types.rs`, `proto/src/fold_tests.rs`, `proto/src/event_tests.rs`, both provider `map.rs` and both `tests_models.rs` (`rg -l 'ModelChoice \{' crates`).
- `store/effects.rs` has 795 lines.
- The proto `SessionSpec` (`command.rs:31-57`) is built as a struct literal at 87 places in `crates/`.

## 3. Live evidence (answers to the backlog questions)

### 3.1 Where does the real value come from?

**Claude 2.1.308.**

1. `claude --help` has `--effort <level>` "(low, medium, high, xhigh, max)".
2. The `init` frame has **no** effort level. It has `per_turn_effort_active: true` (`recordings/claude-p-default.jsonl`). The CLI schema describes that flag as a prompt-cache mode ("whether an effort change keeps the prompt cache"), not the level. `system/per_turn_effort_changed` reports only that flag.
3. The `initialize` answer lists per model `supportsEffort` and `supportedEffortLevels` (for example Opus 5.5: `low, medium, high, xhigh, max`; Claude 4.5 Opus: `low, medium, high`) (`recordings/claude-stream-effort-low.jsonl`, frame 4). It has no current effort.
4. The control request `get_settings` answers `{effective, sources, applied}`. `applied` is `{model, effort, advisor, ultracode, ultracodeRequested, ultracodeAvailable}`; the CLI schema types `applied.effort` as `"low"|"medium"|"high"|"xhigh"|"max"` or `null`. Recorded values:

   | Case | `applied.effort` |
   | --- | --- |
   | `--model haiku`, no `--effort`, settings `effortLevel: high` | `medium` (the model default; the settings file says `high`) |
   | `--model haiku --effort low` | `low` |
   | no flags (settings model `claude-opus-5-5[1m]`, `modelSettings.claude-opus-5-5.effortLevel: high`) | `high` |
   | after the user message `/effort medium` | `medium` |
   | after `/effort auto` | `medium` (the resolved value; `auto` is not reported) |
   | after `apply_flag_settings {"settings":{"effortLevel":"low"}}` | `low` |
   | after `set_model sonnet` | `low` (kept) |
   | after `/effort xhigh` | `xhigh` |

   The CLI answers `get_settings` before the first turn too (`claude-flag-effort-low.jsonl`).
5. **The `effective` and `sources` parts of the `get_settings` answer hold the whole settings, with the `env` table.** On this machine that table has `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_BASE_URL`. The scrubbed recordings keep only `applied`. Any use of `get_settings` must keep the rest out of every log (§5.3, §8).
6. No frame announces an effort change. `/effort x` gives a synthetic `assistant` frame with `local_command_run: {"command":"effort","args":"x"}` and a normal `result` frame. `apply_flag_settings` answers `{}` and sends `system/commands_changed`. So the host must ask.
7. Reading the settings files at the station is wrong: the effort depends on the model default, `modelSettings`, `CLAUDE_CODE_EFFORT_LEVEL`, an organization cap and `maxEffortLevel` (CLI schema text). Case 1 of the table shows the error.

**Codex 0.163.0** (`recordings/codex-*.jsonl`).

1. The answers of `thread/start`, `thread/resume` and `thread/fork` have `reasoningEffort` (string or `null`); `thread.reasoningEffort` and the notification `thread/started` carry it too. With `model_reasoning_effort = "high"` in `~/.codex/config.toml` and no effort from the host, `thread/start` answers `"high"`.
2. `turn/start` with `"effort":"low"` gives `thread/settings/updated` (before the `turn/start` answer) with `threadSettings.effort: "low"` and `threadSettings.collaborationMode.settings.reasoning_effort: "low"`. The recorded `spikes/s2-codex/fixtures/multi_turn.jsonl` has the same frames (`high` at start, `low` after the first turn).
3. The thread keeps the effort: a later `thread/resume` of that thread answers `"low"` (`codex-resume-after-low.jsonl`).
4. `thread/start` takes `config: {"model_reasoning_effort": "low"}` and answers `"low"`. It accepts any string (`"bogus"` answered `"bogus"`), and `null` fails ("reasoning_effort must not be empty"). `thread/resume` and `thread/fork` have the same `config` parameter (schema).
5. `model/list` gives per model `defaultReasoningEffort` and `supportedReasoningEfforts[].reasoningEffort` (for example `low, medium, high, xhigh, max, ultra`).
6. The app-server has no request that changes the thread settings without a turn (`ClientRequest` list).

**Conclusion.** Claude: the `get_settings` answer, field `applied.effort`, asked by the station. Codex: the thread answer (`reasoningEffort`) and `thread/settings/updated` (`threadSettings.effort`). The station reads no config file.

### 3.2 A new event so that a change shows at once?

Yes: `effort.changed` (named like `model.changed` and `mode.changed`; the backlog's `session.effort_changed` follows no existing name). Claude reports a `/effort` change at the end of that command's turn (one control round trip after its `result`). Codex reports a change when the next turn starts (it has no other moment, §3.1 Codex item 6).

### 3.3 Show it on the cards, the wall tiles and the spawn dialog? Choose it?

- Phase 1 shows it in every place where the model of a session shows (§2.4). The chat shows an effort change as its own notice; the model change notice keeps the model alone (§4.4 item 4, decision 15). The wall tiles show no model, so they show no effort. The wall display allowlist stays without model and effort (§6 decision 11).
- The spawn dialog and the composer get an effort choice in Phase 2 (§9). Both providers can set it: Claude `--effort` at launch and `apply_flag_settings {effortLevel}` in a session; Codex `config.model_reasoning_effort` at thread open and `effort` on `turn/start`.

### 3.4 Unknown effort: nothing or "default"?

Nothing. The model then shows alone ("Opus 5.5"), as today. "default" would be wrong: the station always knows the real value when the provider reports one (Claude reports the resolved value, also after `/effort auto`), and "unknown" is not a word of the board (DESIGN-SYSTEM §5).

## 4. Design, Phase 1 (display)

### 4.1 Protocol (`fleetdeck-proto`)

1. New module `crates/fleetdeck-proto/src/effort.rs`, `pub mod effort;` in `lib.rs`:

   ```rust
   //! The reasoning effort of a session (B18, docs/v2/design/effort.md).

   /// The longest effort text that the station passes on.
   pub const MAX_EFFORT_CHARS: usize = 16;

   /// True for an effort id that the station passes on: 1 to 16 of `a-z`,
   /// `0-9`, `-`, `_` (`low`, `xhigh`, `max`, `ultra`). A syntax check only:
   /// it keeps spaces, markup and long text out of the store and the board.
   /// It does not check that the model offers the level (`bogus` passes):
   /// the board shows what the provider reports. A chosen level is checked
   /// against the advertised levels in Phase 2 (§9.2).
   #[must_use]
   pub fn valid_effort(text: &str) -> bool {
       (1..=MAX_EFFORT_CHARS).contains(&text.len())
           && text
               .bytes()
               .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
   }

   /// The provider's value as the station passes it on: a valid id, else
   /// `None` (not shown).
   #[must_use]
   pub fn effort_of(text: Option<&str>) -> Option<String> {
       text.filter(|t| valid_effort(t)).map(str::to_owned)
   }
   ```

2. New event variant in `event.rs`, after `ModelChanged`:

   ```rust
   /// The reasoning effort that the provider reports for the session (B18):
   /// a change, or the first report after a start. `None`: the provider
   /// reports no effort (the model has none). The fold keeps it in
   /// `summary.effort`. Older peers decode this kind as unknown.
   #[serde(rename = "effort.changed")]
   EffortChanged { effort: Option<String> },
   ```

   Wire: `{"kind":"effort.changed","effort":"high"}` or `{"kind":"effort.changed","effort":null}`. TypeScript (generated): `{ "kind": "effort.changed", effort: string | null, }`.
3. `event_kinds.rs`: `KINDS` becomes `[&'static str; 62]` with `"effort.changed"` appended **at the end** (after `"session.move_undone"`); `kind_name` gets `Self::EffortChanged { .. } => "effort.changed"`.
4. `SessionSummary` (`session.rs`), after `model`:

   ```rust
   /// The reasoning effort that the provider reports, from the last
   /// `effort.changed` (B18). Absent: not reported (an older station, a
   /// provider or model without effort, or no report since the start).
   #[serde(default, skip_serializing_if = "Option::is_none")]
   #[ts(optional)]
   pub effort: Option<String>,
   ```

   `SessionSummary::new` sets `effort: None`. TypeScript: `effort?: string`.
5. Fold (`fold.rs`):
   - `Event::EffortChanged { effort } => self.effort.clone_from(effort),`
   - In the `SessionStarted` arm: `self.effort = None;` (a new process reports its own effort at once; a stale value from an earlier process is never shown). This field is the observed value only. The owner's choice, which a resume must keep, is a separate field of Phase 2 (`effort_choice`, §9.1); `session.started` does not clear it.
6. `make ts-types` regenerates `web/src/gen/event/Event.ts` and `web/src/gen/session/SessionSummary.ts`.

### 4.2 Codex mapping (`fleetdeck-providers`)

New file `crates/fleetdeck-providers/src/codex/effort.rs` (`mod effort;` in `codex/mod.rs`), with `impl CodexCore`:

1. A new field in `CodexCore` (`core.rs`): `#[serde(default)] pub(super) thread_effort: Option<String>`, `None` in `new`. The `#[serde(default)]` keeps the checkpoint readable in both directions (§2.3 item 6, §4.3.1): a pre-E3 checkpoint restores with `None`, and a pre-E3 build ignores the key.
2. `fn effort_seen(&mut self, value: Option<&str>) -> Vec<Output>`: `let effort = fleetdeck_proto::effort::effort_of(value);` if `effort == self.thread_effort` return no output; else set `self.thread_effort = effort.clone()` and return `[Output::Emit(Event::EffortChanged { effort })]`.
3. `pub(super) fn effort_of_thread_answer(&mut self, r: &Value) -> Vec<Output>`: the value is `r["reasoningEffort"]` as a string, else `r["thread"]["reasoningEffort"]` as a string, else `None`; returns `effort_seen(value)`. Because `thread_effort` starts as `None`, a `null` answer emits nothing.
4. `pub(super) fn on_settings_updated(&mut self, p: &Value) -> Vec<Output>`: `effort_seen(p.pointer("/threadSettings/effort").and_then(Value::as_str))`.
5. `map.rs`:
   - `on_thread`: after `out.extend(identity);` and before the `model/list` request add `out.extend(self.effort_of_thread_answer(r));`. The order on the wire is `session.started`, then `effort.changed`, then `session.models`.
   - `on_notification`: add `"thread/settings/updated" if self.own_thread(p) => self.on_settings_updated(p),` and the next arm `"thread/settings/updated" => Vec::new(),` (a sub-agent thread). Move `"thread/settings/updated"` from `IGNORED_NOTIFICATIONS` to `HANDLED_NOTIFICATIONS` (the schema test `tests/codex_schema.rs` checks both lists).
6. No other change: `turn/start` keeps its `effort` parameter as it is.

### 4.3 Claude mapping (`fleetdeck-providers`)

New file `crates/fleetdeck-providers/src/claude/effort.rs` (`mod effort;` in `claude/mod.rs`), modeled on `usage.rs`.

1. `control.rs`: `pub fn get_settings(request_id: &str) -> Value { control_request(request_id, "get_settings", json!({})) }`.
2. `core.rs`: new fields of `ClaudeCore`, after `last_total_cost`. **No new `HostRequest` variant**: an older build cannot decode one in a checkpoint (§4.3.1).

   ```rust
   /// Counter of the `fd-settings-N` request ids (B18).
   #[serde(default)]
   pub(super) settings_seq: u64,
   /// The `N` of the open `get_settings` request. It is not in
   /// `host_requests`, so an older build restores this checkpoint (§4.3.1).
   #[serde(default)]
   pub(super) settings_open: Option<u64>,
   /// When the open request was written. Not in the checkpoint: a restored
   /// open request counts as stale.
   #[serde(skip)]
   pub(super) settings_asked: Option<std::time::Instant>,
   /// A `get_settings` is due when the open one is answered.
   #[serde(default)]
   pub(super) settings_again: bool,
   /// The CLI refused `get_settings` or answered in another shape.
   #[serde(default)]
   pub(super) settings_stopped: bool,
   /// The last effort that this core emitted (a valid id, §4.1).
   #[serde(default)]
   pub(super) effort: Option<String>,
   ```

   `new` sets `0`, `None`, `None`, `false`, `false`, `None`.
3. `effort.rs`:

   ```rust
   /// The id prefix of the host's `get_settings` requests.
   pub const SETTINGS_ID_PREFIX: &str = "fd-settings-";
   /// After this time an unanswered request no longer blocks a new one.
   const SETTINGS_STALE: Duration = Duration::from_secs(60);
   ```

   - `pub(super) fn settings_request(&mut self) -> Vec<Output>` (on `ClaudeCore`):
     1. No output when `settings_stopped` or `!started`.
     2. When `settings_open` is `Some` and `settings_asked` is `Some(at)` with `at.elapsed() < SETTINGS_STALE`: set `settings_again = true`, no output.
     3. Else (none open; or one open that is stale; or one open with `settings_asked == None`, which is a restored core): `settings_seq += 1`, `settings_open = Some(settings_seq)`, `settings_asked = Some(Instant::now())`, `settings_again = false`, write `control::get_settings(&format!("fd-settings-{settings_seq}"))`. The replaced request is forgotten: its late answer has another `N` and is ignored.
   - `fn settings_seq_of(request_id: &str) -> Option<u64>`: the `N` after `SETTINGS_ID_PREFIX`, else `None`.
   - `pub(super) fn on_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output>` (called for every `control_response` whose id starts with `SETTINGS_ID_PREFIX`):
     1. When `settings_seq_of(&resp.request_id) != core.settings_open`: no output (a late answer of a replaced request).
     2. `settings_open = None`, `settings_asked = None`.
     3. When `!resp.ok` or `resp.response["applied"]` is not an object: `settings_stopped = true`, one `tracing::debug!` with the error text only, no output (no `error` event).
     4. Else `let effort = effort_of(applied.get("effort").and_then(Value::as_str));`. Output `EffortChanged { effort }` when `effort != core.effort` (then `core.effort = effort`).
     5. Then, if `std::mem::take(&mut core.settings_again)`, append `core.settings_request()`.
   - `pub fn raw_view(frame: &Value) -> Cow<'_, Value>`: when `frame["type"] == "control_response"` and `frame["response"]["request_id"]` is a string that starts with `SETTINGS_ID_PREFIX`, return an owned copy with `response.response` replaced by `{"applied": <response.response.applied or null>}`; every other key of `response` (`subtype`, `request_id`, `error`) stays. Every other frame is returned borrowed and unchanged.
4. `map.rs` (net +6 lines at most):
   - `on_system` `init`, after `out.extend(core.usage_request());` (`map.rs:153`, the first-`init` branch only): `out.extend(core.settings_request());`.
   - `on_result`, after `out.extend(core.usage_request());` (`map.rs:579`): `out.extend(core.settings_request());`. This covers `/effort`, `/model` and every other turn.
   - `on_control_response` (`map.rs:683`), as its first statement, before the `host_requests` lookup: `if resp.request_id.starts_with(effort::SETTINGS_ID_PREFIX) { return effort::on_response(core, resp); }`.
   - The `HostRequest::SetModel` success arm (`map.rs:735-739`): after the `ModelChanged` output, append `core.settings_request()` (a model change can change or clamp the effort).
5. `driver_io.rs` `feed_line`: log `effort::raw_view(&v)` instead of `v` (`shared.log(RawDir::Out, &effort::raw_view(&v))`). The mapping still gets the full frame; the core reads only `applied.effort` and keeps nothing else.
6. Replays: the core writes one `get_settings` after the first `init` of each process. Each recorded fixture gets that write as an `in` record right after its `fd-usage-1` record (22 records, the same method as commit `0060c90a` for `get_usage`). No fixture answers it, so the request stays open and no other `get_settings` is written in a replay.

### 4.3.1 Checkpoint compatibility (Claude and Codex)

The new core state goes into the adapter checkpoint (§2.3 item 6). The rules:

1. **The version stays 1** (`CHECKPOINT_VERSION`, `CHECKPOINT_READS` of both providers do not change). A version 2 would make the previous build refuse the checkpoint at a rollback, and the station would mark the live shim lost. Every new field decodes in both directions, so no version change is necessary.
2. **Every new field has `#[serde(default)]`**, so a checkpoint of the previous build restores: no open settings request, `effort: None`, `settings_seq: 0`. The restored core asks after the next `result` or `set_model` answer (the restored core is `started`, so no first `init` comes). The first answer emits the effort again; the fold and the timeline show no change (§4.4 item 5).
3. **The `Instant` is skipped** (`#[serde(skip)]`, the same as `usage_asked`). A restored open request has `settings_asked == None` and counts as stale: the next ask replaces it. An answer to the restored open request that comes before the next ask is used.
4. **No new `HostRequest` variant, and no `deny_unknown_fields` on `ClaudeCore` or `CodexCore`.** The open settings request is the field `settings_open`, not a `host_requests` entry. The previous build restores a new checkpoint, ignores the new keys, and treats a late `fd-settings-N` answer as an unknown id (`map.rs:684-687`: no output). It then asks nothing more; the summary keeps the last effort until the process ends.
5. The request counter `settings_seq` is in the checkpoint, so the ids do not repeat after a restore. After a rollback and a new upgrade the counter starts at 0 again; a repeated id can only match the one open request of the new core, and its answer is a real reading of the CLI.
6. The checkpoint holds only the validated effort id of the settings answer, never `effective` or `sources` (§8).
7. Phase 2 obeys the same rules (§9.5).

### 4.4 Board (`web/`)

1. `web/src/ui/format.ts`:

   ```ts
   /** The model and the reasoning effort of a session: `Opus 5.5 · high`; the model alone when
    * the provider reports no effort (B18). */
   export function modelText(model: string | null, effort?: string | null): string {
     const name = shortModel(model);
     return effort === undefined || effort === null || effort === '' ? name : `${name} · ${effort}`;
   }

   /** The `title` of a model with its effort: the raw model id, then the effort in words. */
   export function modelTitle(model: string | null, effort?: string | null): string {
     const id = model ?? 'default model';
     return effort === undefined || effort === null || effort === '' ? id : `${id} · reasoning effort ${effort}`;
   }
   ```

   The effort id shows as the provider names it (`low`, `medium`, `high`, `xhigh`, `max`, `ultra`), lower case (§7 question 1).
2. Use `modelText(s.model, s.effort)` instead of `shortModel(s.model)` in: `HeaderBadges.tsx:85` (and `title={modelTitle(s.model, s.effort)}` at `:83`), `SessionView.tsx:642`, `Sidebar.tsx:145`, `FleetStrip.tsx:230`, `NeedsYouRail.tsx:224` (`w.card.summary`), `ComparePage.tsx:228`, `RacePane.tsx:60`. Replace the `shortModel` import with `modelText` where `shortModel` has no other use in the file (in `SessionView.tsx` and `Sidebar.tsx` the import line is changed in place; no line is added).
3. `PropertiesPanel.tsx`: the Model row stays. After it, when `s.effort` is a non-empty string: `<Row label="Effort"><span title="The reasoning effort that the provider reports">{s.effort}</span></Row>`.
4. `chat/notices.ts`: `export const effortNotice = (effort: string): NoticeText => ({ text: \`Effort changed to ${effort}\`, title: \`Reasoning effort: ${effort}\` });`. `modelNotice` does not change: the model change notice keeps the model alone. Reason: at the moment of `model.changed` the effort of the new model is not known yet. Claude reports it one control round trip after the `set_model` answer or the `/model` turn; Codex sends `thread/settings/updated` before the `turn/start` answer, so `effort.changed` comes before `model.changed`. When both change, the chat shows two notices in the order of the events ("Effort changed to low", "Model changed to Sonnet 5.5" for Codex; the reverse order for Claude). When only the model changes and the effort stays, only the model notice shows. The compact model places show `Opus 5.5 · high` at all times (item 2).
5. `session/timeline.ts` `buildTimeline`: two locals, `let lastEffort: string | undefined;` (the last non-null effort of the timeline) and `let effortReported = false;` (the current process reported an effort).
   - New case `'session.started'`: `effortReported = false;` (no item; the case only resets the state).
   - Case `'effort.changed'`:
     1. When `e.effort === null`: `effortReported = true`; nothing else (`lastEffort` stays).
     2. Else, when `effortReported && lastEffort !== undefined && e.effort !== lastEffort`: add `{ type: 'notice', key, ...effortNotice(e.effort), tone: 'info' }`.
     3. Then `effortReported = true; lastEffort = e.effort;`.
   - So: the first report of each process adds no notice (also when it differs from the last process); a repeat adds nothing; `null` adds nothing; `low` → `null` → `low` adds nothing; `high` → `null` → `low` in one process adds "Effort changed to low"; a restored shim (no `session.started`) that reports the same value adds nothing.
6. CSS: no change. The strip, header and rail model cells already cut with an ellipsis (`home.css:756-771`); the `title` keeps the whole text.

### 4.5 Errors and edge cases

| Case | Behavior |
| --- | --- |
| Claude CLI without `get_settings`, or a connection that refuses it ("get_settings is not available on this connection") | Error answer: no more asks in this process, no event, no chat error. The board shows the model alone. |
| Claude model without effort (`applied.effort: null`) | `effort.changed {effort: null}` only if a value was shown; the board shows the model alone. |
| `/effort auto` | Claude reports the resolved value; it shows. |
| A provider value that fails `valid_effort` (for example `"bogus value"`, `"High"`, or text over 16 characters) | Treated as `null`. |
| A syntactically valid value that is not a real level (Codex echoes `model_reasoning_effort = "bogus"` from a config) | Shown as reported (`GPT-6.1 Sol · bogus`): it is what the provider reports. Phase 1 does not judge the level (§4.1, decision 7). |
| A Codex sub-agent thread changes its settings | Ignored (`own_thread`). |
| Station restart with a restored shim | The restored core keeps `effort` and `settings_seq` from the checkpoint; an open request counts as stale and the next ask replaces it (§4.3.1). A checkpoint of the previous build restores with no effort; the next report emits the same value again; the fold and the timeline show no change. |
| Station rollback to the previous build with a live shim | The previous build restores the checkpoint, ignores the new keys and the late `get_settings` answer, and asks no more. The board keeps the last effort until the process ends (§4.3.1 item 4). |
| Process restart (resume) | `session.started` clears `summary.effort`; Codex reports in the thread answer, Claude after `init` (one control round trip). |
| Owner changes the effort in a terminal that holds the session (DL-140) | The board shows the new value after the next driven turn. |
| A `get_settings` answer never comes | After 60 s a new ask replaces it. |

### 4.6 Compatibility

- Phase 1 adds no command and no feature flag. An older deck decodes `effort.changed` as an unknown kind and drops the unknown summary field. An older station sends neither: `effort` is absent and the board shows the model alone, as today. Stored summaries decode with `effort: None`.
- Adapter checkpoints stay version 1 and decode in both directions (§4.3.1).
- The board helpers accept `undefined` (an older deck).

### 4.7 Out of scope

- The coordinator tools (`fleet_view` `MeItem.model`, `crates/fleetdeck-deck/src/store/fleet_view_build.rs:346`) and the MCP session lists do not get the effort.
- The wall display allowlist (`display_view.rs:60-96`) stays without model and effort.
- Claude `ultracode` and fast mode (they are in the same `applied` answer; B12 decides about ultracode).
- The effort of a sub-agent (Codex `collabAgentToolCall.reasoningEffort`, Claude subagents).
- A default effort per provider instance in `station.toml`.

## 5. Build plan, Phase 1

Every heavy command runs as `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <cmd>` (written `heavy <cmd>` below). Every task also runs the guards: `heavy cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`.

Order: E1, then E2, E3 and E4 in parallel (no shared file).

### E1. Protocol: `effort.changed` and `summary.effort` (backend, about 180 lines)

- **Owned files:** `crates/fleetdeck-proto/src/effort.rs` (new), `crates/fleetdeck-proto/src/lib.rs`, `crates/fleetdeck-proto/src/event.rs`, `crates/fleetdeck-proto/src/event_kinds.rs`, `crates/fleetdeck-proto/src/session.rs`, `crates/fleetdeck-proto/src/fold.rs`, `crates/fleetdeck-proto/src/event_tests.rs`, `crates/fleetdeck-proto/src/fold_effort_tests.rs` (new), `web/src/gen/event/Event.ts`, `web/src/gen/session/SessionSummary.ts`.
- **Changes:** §4.1 items 1 to 6. In `fold.rs` wire the new tests: `#[cfg(test)] #[path = "fold_effort_tests.rs"] mod effort_tests;` (`fold_tests.rs` has 798 lines: no test goes there). In `event_tests.rs` add one sample `Event::EffortChanged { effort: Some("high".into()) }` at the end of `samples()` (keep the file under 800 lines).
- **Tests:**
  - `event_tests::every_kind_has_a_sample_and_round_trips` (existing) passes with 62 kinds.
  - `effort::tests::valid_effort_accepts_provider_ids_and_refuses_others`: `low`, `xhigh`, `max`, `ultra`, `x_1-2` and `bogus` pass (a syntax check only); `""`, `High`, `very high`, `bogus value`, a 17-character id fail.
  - `fold_effort_tests::effort_changed_sets_and_clears_the_effort`: `Some("high")` sets it, `None` clears it.
  - `fold_effort_tests::session_started_clears_the_effort`: a value, then `session.started`, gives `None`.
  - `fold_effort_tests::a_summary_without_effort_decodes_and_serializes_without_it`: JSON of a summary without `effort` decodes to `None`; a summary with `None` serializes without the key.
  - `fold_effort_tests::effort_changed_with_null_round_trips`: `{"kind":"effort.changed","effort":null}` decodes to `EffortChanged { effort: None }`.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(effort) | test(every_kind_has_a_sample) | binary(ts_drift)'`
  - `heavy make ts-types` gives no diff after the commit.
  - the guards.

### E2. Claude: the applied effort through `get_settings`, with a redacted raw log (backend, high risk, about 500 lines)

- **Depends on:** E1.
- **Owned files:** `crates/fleetdeck-providers/src/claude/effort.rs` (new), `crates/fleetdeck-providers/src/claude/mod.rs`, `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/control.rs`, `crates/fleetdeck-providers/src/claude/driver_io.rs`, `crates/fleetdeck-providers/src/claude/tests_effort.rs` (new), `crates/fleetdeck-providers/src/claude/checkpoint_pre_effort.json` (new), `crates/fleetdeck-providers/src/claude/tests_usage.rs`, `crates/fleetdeck-providers/tests/claude_core_replay.rs`, `crates/fleetdeck-providers/tests/claude_driver_replay.rs`, `crates/fleetdeck-providers/tests/claude_sdk_reference.rs`, `spikes/s1-claude/fixtures/rust/*.jsonl` (the 19 files, one inserted record per process), `spikes/s1-claude/fixtures/rust/effort_settings.jsonl` (new).
- **Changes:** first make the fixture of item 0, then §4.3 items 1 to 6 with the checkpoint rules of §4.3.1, and:
  0. Before any other change, at the base commit: add a scratch test in `claude/tests_effort.rs` that prints `serde_json::to_string_pretty(&running_core().checkpoint().unwrap().state)` after an `init` (so `fd-usage-1` is open in `host_requests`). Save the output as `claude/checkpoint_pre_effort.json`. Replace the scratch test with the real tests. The file is the checkpoint of the previous build.
  1. `tests_usage.rs`: the test `the_usage_request_is_written_after_init_and_asks_to_skip_the_behaviors` compares only the `get_usage` writes (filter by subtype), because `init` now also writes `get_settings`.
  2. `claude_core_replay.rs:248`: the write count becomes 84 + 22 + the writes of the new fixture; update the comment. `claude_sdk_reference.rs:95-96`: also skip writes with `request.subtype == "get_settings"` ("The SDK never asked `get_settings` (B18): only the core does."), and skips the `out` records whose `response.request_id` starts with `fd-settings-` (the SDK sent no such request).
  3. New fixture `effort_settings.jsonl`: a copy of `simple.jsonl` (header, spawn, records) with, after the `fd-settings-1` `in` record, an `out` record `{"type":"control_response","response":{"subtype":"success","request_id":"fd-settings-1","response":{"effective":{"env":{"ANTHROPIC_AUTH_TOKEN":"fd-test-secret-0001"}},"sources":[{"source":"userSettings","settings":{"env":{"ANTHROPIC_AUTH_TOKEN":"fd-test-secret-0001"}}}],"applied":{"model":"claude-sonnet-5-5","effort":"high","advisor":null,"ultracode":false}}}}`; after the `result` record the `in` record of `fd-settings-2` and an answer with `"effort":"xhigh"` (same secret).
  4. `claude_driver_replay.rs`: the raw-log check compares each read frame with `claude::effort::raw_view(frame)` (make `raw_view` and `SETTINGS_ID_PREFIX` `pub` through `claude/mod.rs`), and asserts that no raw-log file of any fixture contains the text `fd-test-secret`.
- **Tests (`tests_effort.rs`):**
  - `init_writes_one_get_settings_after_get_usage`: the writes after the first `init` are `get_usage` (`fd-usage-1`) then `{"type":"control_request","request_id":"fd-settings-1","request":{"subtype":"get_settings"}}`; a second `init` writes no `get_settings` while one is open.
  - `an_answer_emits_effort_changed_once`: answer `applied.effort: "high"` gives `[EffortChanged { effort: Some("high") }]`; the same value again after the next `result` gives nothing.
  - `a_result_asks_again_and_a_change_is_emitted`: after a `result`, `fd-settings-2` is written; its answer `"medium"` gives `EffortChanged { Some("medium") }`.
  - `a_null_effort_clears_a_shown_value`: `"high"` then `null` gives `EffortChanged { None }`.
  - `an_error_answer_stops_the_asks_without_an_error_event`: answer `{"subtype":"error","error":"Unsupported control request subtype: get_settings"}` emits nothing; later `result` frames write no `get_settings`.
  - `an_answer_without_applied_stops_the_asks`.
  - `an_invalid_effort_is_not_passed_on`: `"High Effort"` gives no event (or `None` after a value).
  - `a_request_while_one_is_open_is_sent_after_its_answer`: open `fd-settings-1`, a `result` (no write), the answer of `fd-settings-1` emits and writes `fd-settings-2`.
  - `a_stale_request_is_replaced_after_60_seconds`: set `settings_asked` 61 s back; a `result` writes `fd-settings-2`; a late answer to `fd-settings-1` is ignored.
  - `no_get_settings_before_the_session_started`: a `set_model` answer before the first `init` writes no `get_settings`.
  - `a_model_change_asks_for_the_effort`: after `init`, a `set_model` success answer gives `ModelChanged` and the write `fd-settings-2` (after the first answer came).
  - `a_pre_effort_checkpoint_restores_and_asks_after_the_next_result` (upgrade): `ClaudeCore::restore(config(), &AdapterCheckpoint { provider: Claude, version: 1, state: <checkpoint_pre_effort.json> })` succeeds; `effort` is `None`, `settings_open` is `None`; a `result` frame writes `fd-settings-1`.
  - `an_open_settings_request_survives_a_checkpoint`: after `init` (`fd-settings-1` open), checkpoint and restore; the answer to `fd-settings-1` emits `EffortChanged { Some("high") }`.
  - `a_restored_open_request_counts_as_stale`: after `init`, checkpoint and restore; a `result` writes `fd-settings-2` at once (no 60 s wait); a late answer to `fd-settings-1` emits nothing; the answer to `fd-settings-2` emits.
  - `the_checkpoint_stays_readable_by_the_previous_build` (rollback): a core after `init` with `fd-settings-1` open and an effort `high` shown: (a) `state["host_requests"]` has no key that starts with `fd-settings-`; (b) every value of `state["host_requests"]` decodes as a test-local copy of the previous enum (`enum PreEffortHostRequest { Initialize, Interrupt(Option<String>), SetMode(String), SetModel(Option<String>), Usage }`, with the same serde derives); (c) the state without the keys `settings_seq`, `settings_open`, `settings_again`, `settings_stopped`, `effort` restores; (d) the checkpoint `version` is 1.
  - `the_checkpoint_has_no_settings_text`: after an answer of the item 3 fixture, the JSON text of the checkpoint has no `fd-test-secret` and no `effective`.
  - `raw_view_keeps_only_applied`: the frame of fixture item 3 gives `{"type":"control_response","response":{"subtype":"success","request_id":"fd-settings-1","response":{"applied":{...}}}}`; its JSON text has no `fd-test-secret`; an error answer keeps `error`; a `get_usage` answer and an `assistant` frame are returned unchanged and borrowed.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(effort) | test(usage) | test(checkpoint) | binary(claude_core_replay) | binary(claude_driver_replay) | binary(claude_sdk_reference) | binary(claude_driver)'`
  - the guards.

### E3. Codex: the effort from the thread answer and `thread/settings/updated` (backend, about 300 lines)

- **Depends on:** E1. Can run in parallel with E2 and E4.
- **Owned files:** `crates/fleetdeck-providers/src/codex/effort.rs` (new), `crates/fleetdeck-providers/src/codex/mod.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/tests_effort.rs` (new), `crates/fleetdeck-providers/tests/codex_core_replay.rs`, `web/src/testing/fixtures/codex-subagent.events.json`, `web/src/testing/fixtures/codex-tool_call_approve.events.json`, `web/src/testing/fixtures/codex-question_rpc.events.json`.
- **Changes:** §4.2. Then regenerate the board fixtures with `FD_UPDATE_FIXTURES=1` (`heavy env FD_UPDATE_FIXTURES=1 cargo nextest run -p fleetdeck-providers -E 'test(the_board_timeline_fixtures_equal_the_canonical_replay)'`).
- **Tests (`tests_effort.rs`):**
  - `a_thread_start_answer_reports_its_effort`: an answer with `"reasoningEffort":"high"` gives `session.started`, then `EffortChanged { Some("high") }`, then the `model/list` write.
  - `a_null_effort_in_the_answer_emits_nothing`.
  - `the_thread_object_is_the_fallback`: no top-level field, `thread.reasoningEffort: "low"` gives `Some("low")`.
  - `settings_updated_of_the_own_thread_emits_a_change_once`: `threadSettings.effort: "low"` emits; the same value again emits nothing.
  - `settings_updated_of_a_sub_agent_thread_is_ignored`: another `threadId` emits nothing and is not counted as an unknown frame.
  - `an_invalid_effort_is_not_passed_on`: `"bogus value"` gives no event.
  - `resume_and_fork_answers_report_the_effort` (the same handler).
  - `a_pre_effort_codex_checkpoint_restores`: the state of a current checkpoint without the key `thread_effort` restores with `thread_effort == None`; a later `thread/settings/updated` with `low` emits `Some("low")`.
  - `the_codex_checkpoint_keeps_the_thread_effort`: after a `high` report, checkpoint and restore; the same value again emits nothing.
  - `codex_core_replay::multi_turn_reports_the_effort_of_the_thread_then_of_the_turn` (new, in `tests/codex_core_replay.rs`): the `effort.changed` events of `multi_turn` are `Some("high")` then `Some("low")`, and both come after `session.started`.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(effort) | test(checkpoint) | binary(codex_core_replay) | binary(codex_driver_replay) | binary(codex_schema) | binary(codex_driver_sim)'`
  - the guards.

### E4. Board: `Opus 5.5 · high`, the Effort row and the change notice (ux, about 200 lines)

- **Depends on:** E1 (the generated types). Can run in parallel with E2 and E3.
- **Owned files:** `web/src/ui/format.ts`, `web/src/ui/format.test.ts`, `web/src/chat/notices.ts`, `web/src/session/timeline.ts`, `web/src/session/timeline.test.ts`, `web/src/session/HeaderBadges.tsx`, `web/src/session/PropertiesPanel.tsx`, `web/src/session/properties.test.tsx`, `web/src/session/SessionView.tsx`, `web/src/app/Sidebar.tsx`, `web/src/board/FleetStrip.tsx`, `web/src/board/NeedsYouRail.tsx`, `web/src/compare/ComparePage.tsx`, `web/src/wall/RacePane.tsx`.
- **Changes:** §4.4 items 1 to 5 (`modelNotice` does not change). Follow `docs/v2/DESIGN-SYSTEM.md` (§5 Models: the model and its effort are not colored; no mono). Add one line to DESIGN-SYSTEM §5 Models through the architect, not in this task (the file is not owned).
- **Tests:**
  - `format.test.ts`: `modelText('claude-opus-5-5[1m]', 'high')` is `Opus 5.5 · high`; `modelText('gpt-6.1-sol', 'xhigh')` is `GPT-6.1 Sol · xhigh`; `modelText('claude-opus-5-5', undefined)`, `null` and `''` are `Opus 5.5`; `modelText(null, 'high')` is `default · high`; `modelTitle('claude-opus-5-5[1m]', 'high')` is `claude-opus-5-5[1m] · reasoning effort high`.
  - `timeline.test.ts`: the first `effort.changed` adds no notice; `high` then `low` adds "Effort changed to low" with the title "Reasoning effort: low"; the same value twice adds nothing; `null` adds nothing; `low` then `null` then `low` adds nothing; `high` then `null` then `low` adds "Effort changed to low"; `high`, `session.started`, `low` adds nothing (the first report of a new process); `high`, `session.started`, `high`, `low` adds one notice ("low"); `model.changed` then `effort.changed` (a change) gives the two notices in that order, and the model notice text has no effort.
  - `properties.test.tsx`: a summary with `effort: 'high'` shows the row "Effort" with "high"; without `effort` the row is absent.
  - One render test of the header: `Who` shows "Opus 5.5 · high" with the title of `modelTitle`.
- **Acceptance:**
  - `cd web && heavy bun run typecheck && bun run lint`
  - `cd web && heavy bun run test src/ui src/chat src/session src/app src/board src/compare src/wall`
  - `sh tools/design-check.sh`
  - the guards.
  - Gallery shots of the session view and the fleet home, both themes, desktop and phone (fleet-design-system skill §3 step 3); the strip model cell at 1440px and at 390px has no overflow.

## 6. Decisions made in this draft

1. The event is `effort.changed` with `effort: Option<String>`. Rejected: `session.effort_changed` (no existing event uses that form; the model and mode events are `model.changed` and `mode.changed`); a `String` field (a model without effort could not clear a shown value).
2. The effort is a separate event, not a field of `session.started`. Rejected: a new `session.started` field (it is built at many places, and Claude knows the value only after a control round trip).
3. `session.started` clears `summary.effort`. Rejected: keep the last value (a resumed process with an older CLI would show a stale value).
4. Claude source: `get_settings` → `applied.effort`, asked after the first `init`, after each `result` and after a `set_model` answer, one open at a time, stopped at the first error. Rejected: read the settings files at the station (wrong for case 1 of §3.1); the `init` frame (it has no level); parse the `/effort` reply text (no initial value; English text); ask on a timer (no event to ask on; asks while nothing changes).
5. The Claude driver logs the `get_settings` answer with `applied` only. Rejected: no raw log for that answer at all (the log then misses a frame that the mapping read; the replay check of the log loses its rule); scrub only known key names (an unknown key with a secret would pass).
6. Codex source: the thread answer and `thread/settings/updated`. Rejected: read `~/.codex/config.toml` (profiles, `-c` overrides and per-model defaults make it wrong; the answer is authoritative); `thread/started` (it repeats the answer).
7. An effort is passed on only when it is 1 to 16 of `a-z0-9_-`. This is a syntax check, not a level check: `bogus` passes and shows, because the provider reports it. Rejected: pass any string (spaces, markup and long text would reach the store and the board); a fixed list of levels (Codex has `ultra`, a later CLI can add a level); a check against the advertised levels in Phase 1 (the display must show what the provider runs with, also when the list is unknown or late). Phase 2 checks a chosen level against the advertised levels of the current model (§9.2).
8. An unknown effort shows nothing. Rejected: "default" (wrong when the provider does not report; the board never says "unknown").
9. Compact places show `Opus 5.5 · high` through one helper `modelText`; Properties gets its own Effort row. Rejected: a badge or a color for the effort (models are never colored, DESIGN-SYSTEM §1); `Opus 5.5 · high` also in the Properties Model row (the row has the context aside already).
10. The chat shows "Effort changed to x" only for a change after a first report. Rejected: a notice for every report (each start and resume would add a line); no notice (the owner asked to see a `/effort` change at once).
11. The wall display gets no effort. Rejected: add model and effort to the display allowlist (a display allowlist change is its own security decision, §10.4).
12. Choosing the effort is Phase 2 with its own command, capability and station feature (§9). Rejected: put it in Phase 1 (it is a new command path: high risk; the display is useful alone).
13. The new core state is in `#[serde(default)]` fields of the core, never in a new `HostRequest` variant; the `Instant` is skipped; the checkpoint version stays 1 (§4.3.1). Rejected: a `HostRequest::Settings` variant (the previous build cannot decode it, so a rollback loses the shim); checkpoint version 2 (the previous build refuses it, the same loss); no checkpoint field (a restored core would ask with a used id).
14. The timeline keeps the last effort and a "reported in this process" flag; `session.started` resets the flag (§4.4 item 5). Rejected: one variable for both (a `null` report or a new process then adds a wrong notice).
15. The model change notice keeps the model alone; an effort change has its own notice (§4.4 item 4). Rejected: put the effort into the model notice (the effort of the new model is not known at `model.changed`: the notice would show the old effort or wait for an event that can fail to come).
16. Phase 2 keeps the chosen effort (`summary.effort_choice`, from `effort.chosen` and from `effort.set_result` with outcome `accepted`) apart from the observed effort (`summary.effort`). Resume and restore pass the choice. Rejected: resume from `summary.effort` (cleared by `session.started`; a process that stops before its report, or a CLI that does not report, loses the choice); a station-only column (a migration, and the board cannot see the choice).
17. A `set_effort` effect settles only on `effort.set_result` with its own effect id (the interrupt pattern). The first settling result is `accepted` (the provider accepted the change); a later verification result of the same effect is a no-op for the effect, because `confirm` matches only `claimed` and `outcome_unknown` (`store/effects.rs:626`). Rejected: confirm on `effort.changed` with the effort as correlation (an ack is no observation; a clamp never matches; an unchanged level emits no event; two choices of the same level are not told apart).
18. Claude verifies a change with a `get_settings` read written after the `apply_flag_settings` answer. Rejected: emit the requested level from the empty ack (§3.1 Claude item 6: the ack carries nothing; caps and defaults change the level). The ack is evidence of acceptance, not of the level: it emits `effort.set_result` with outcome `accepted` (decision 22), never `effort.changed`.
19. Codex sends the pending pick with its turn request and settles it from that request. Rejected: take `pending_effort` at the answer (a choice made while the request is open would be reported as sent).
20. An effort command is gated at HTTP admission and at every send and resend (the import pin pattern). Rejected: admission only (a reconnect to an older station sends a spec whose effort the station ignores).
21. `ModelChoice.resolved` and `choice_for` match the summary model to its advertised choice, with the context suffix removed as a last step. Rejected: look up `value == s.model` (Claude reports the resolved id, so the levels never show).
22. The choice persists when the provider **accepts** the change, not when it is verified. Each provider emits `effort.set_result` with outcome `accepted` exactly once per accepted change (Claude: the success ack of `apply_flag_settings`; Codex: the success answer of the `turn/start` that carried the pick). Only `accepted` (and `effort.chosen`) sets `summary.effort_choice`; the verification outcomes (`applied`, `adjusted`, `unverified`) never change it. So a process that ends between the ack and the verifying read keeps the choice for the resume, and no `effort.changed` is fabricated. Rejected: set the choice from the verification result (a process that ends before the read loses an accepted choice: round 2 BLOCKER); set the choice from every confirming outcome (two accepted changes, then the late verification of the first, would put the older level back); set the choice at the station decide (a refused or not offered level would persist).

## 7. Owner questions (each has a default, so the build can go on)

1. **Words of the levels.** Show the provider ids (`low`, `medium`, `high`, `xhigh`, `max`, `ultra`), or words ("extra high")? *Default:* the ids, lower case, as the providers and their CLIs name them.
2. **Phase 2.** Build the effort choice (spawn dialog and composer) after Phase 1, or not now? *Default:* build it after Phase 1 merges, after an adversary DESIGN REVIEW of §9.
3. **Wall display.** Should a wall display show model and effort (a change of the display allowlist)? *Default:* no; a separate item if wanted.

## 8. Risk and threat notes

- Phase 1, E1, E3, E4: normal (additive read-only fields; no command).
- **E2: high.** The `get_settings` answer carries the effective settings of the user, with the `env` table (recorded: `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`). Threats and their controls:
  1. *The secret lands in the raw-frame log on disk.* Control: `raw_view` before `shared.log` (§4.3 item 5); the replay asserts that no log holds `fd-test-secret`.
  2. *The secret reaches an event, the station store, the deck or the board.* Control: the core reads only `applied.effort`, passes it through `valid_effort`, and keeps nothing else; the events carry a 16-character id at most.
  3. *The secret reaches a tracing line.* Control: the debug line of a refused ask logs only `resp.error`; no code logs `resp.response`.
  4. *The CLI process memory and the shim buffer hold the answer.* Accepted: the same process already holds those settings; the shim buffer is in memory (`host/shim/output.rs`).
  5. *A future fixture recording.* The recorder writes raw frames: a recorder that asks `get_settings` must scrub the answer the same way. E2 writes its fixture by hand with a fake secret.
  6. *The secret reaches the adapter checkpoint (the station database).* Control: the core keeps only the validated effort id and the request counters (§4.3.1 item 6); the test `the_checkpoint_has_no_settings_text` checks the checkpoint text.
  7. *A rollback or a restore loses a live shim.* Control: §4.3.1 (version 1, defaulted fields, no new `HostRequest` variant) and the upgrade and rollback tests of E2 and E3.
- Recommended review: adversary CODE REVIEW of the E2 branch (the redaction and the "nothing else kept" rule), with the check command of E2.
- **Phase 2: high** (a new command path). Threats and their controls:
  1. *An older station launches without the chosen effort.* Control: the gate at admission and at every send and resend (§9.2 item 1, E8 tests).
  2. *The board shows or the station confirms a level that the provider does not run.* Control: no observation from an ack (an ack gives `accepted`, never `effort.changed`); `effort.set_result` with the effect id; a verifying read after the change (§9.2 items 4, 6, 7).
  3. *A resume forgets the choice.* Control: `summary.effort_choice`, set by `effort.chosen` and by the `accepted` result at the provider's ack, before any verification (§9.2 items 5 and 6, decision 22). A process that ends between the ack and the verifying read keeps the choice.
  4. *A rollback loses a live shim.* Control: §9.5.
  5. *A chosen level carries text into the CLI arguments or a control request.* Control: `valid_effort` at the station decide (1 to 16 of `a-z0-9_-`), and `--effort` is one argument (no shell).
- Recommended review for Phase 2: adversary PRE-PR REVIEW of each of E6, E7 and E8.

## 9. Design, Phase 2 (choose the effort) — high risk, adversary DESIGN REVIEW before the build

Phase 2 keeps two values apart:

- the **observed** effort, `summary.effort`, from `effort.changed` only (Phase 1). It is cleared by `session.started`.
- the **chosen** effort, `summary.effort_choice`, the owner's choice that a resume and a restore pass on. `session.started` does not clear it.

A provider acknowledgement never becomes an observation, but it is the moment the choice persists: the provider emits `effort.set_result` with outcome `accepted`, which sets `summary.effort_choice` and settles the effect. A later verification result (`applied`, `adjusted`, `unverified`) only reports what the provider runs. The station settles a `set_effort` effect only on a correlated `effort.set_result` from the provider.

### 9.1 Shapes

**Proto (E5).**

- `fleetdeck_proto::command::SessionSpec.effort: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]`). Every struct literal adds `effort: None` (mechanical).
- `Capabilities.set_effort: bool` (`#[serde(default)]`), `Operation::SetEffort` (degradation `Hide`, like `SetModel`; `Operation::ALL` becomes 13 entries). Claude and Codex: `true` (set in E6 and E7; E5 sets `false` in every literal).
- `ModelChoice` (`event_types.rs:224`) gets three fields, each `#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]`:

  ```rust
  /// The model id that `value` selects (Claude `resolvedModel`); `None`
  /// when `value` is the id itself (Codex).
  pub resolved: Option<String>,
  /// The effort levels of the model. `None`: not advertised; empty: the
  /// model has no effort setting.
  pub efforts: Option<Vec<String>>,
  /// The provider's default level (Codex `defaultReasoningEffort`).
  pub default_effort: Option<String>,
  ```

  Claude: `resolved` = `resolvedModel`; `efforts` = `supportedEffortLevels` when it is an array, `Some(vec![])` when `supportsEffort` is `false`, else `None`; `default_effort` = `None`. Codex: `resolved` = `None`; `efforts` = `supportedReasoningEfforts[].reasoningEffort`; `default_effort` = `defaultReasoningEffort`. Each level goes through `valid_effort`; an invalid entry is dropped. E5 sets all three to `None` in the provider mappings; E6 and E7 fill them.
- `fleetdeck_proto::effort` (E5) gets the model matching that the providers and the board use:

  ```rust
  /// The model id without a trailing context suffix:
  /// `claude-opus-5-5[1m]` → `claude-opus-5-5`; other ids unchanged.
  #[must_use]
  pub fn base_model(id: &str) -> &str;

  /// The advertised choice of `model` (the summary model or the provider's
  /// current model). In list order, the first choice where:
  /// 1. `value == model`; else
  /// 2. `resolved == Some(model)`; else
  /// 3. `base_model(value)` or `base_model(resolved)` equals `base_model(model)`.
  /// `None` when no choice matches.
  #[must_use]
  pub fn choice_for<'a>(models: &'a [ModelChoice], model: &str) -> Option<&'a ModelChoice>;

  /// True when `a` and `b` name the same model: equal, or one choice has
  /// both among its `value` and `resolved` (`sonnet` and `claude-sonnet-5-5`).
  #[must_use]
  pub fn same_model(models: &[ModelChoice], a: &str, b: &str) -> bool;
  ```

  The levels of a context variant are the levels of its base model (the CLI advertises no `[1m]` value, §2.3 item 7).
- Two new events, appended at the end of `KINDS` after `effort.changed` (64 kinds), with arms in `kind_name` and the fold:

  ```rust
  /// The provider's answer to a `session.set_effort` (B18 Phase 2): the
  /// evidence that settles the station's effect. Older peers decode this
  /// kind as unknown.
  #[serde(rename = "effort.set_result")]
  EffortSetResult {
      /// The station's effect id (the host's control id); `None` without one.
      control_id: Option<String>,
      /// The level that the owner chose.
      requested: String,
      /// The effort that the provider reports after the change; `None`:
      /// not reported.
      effort: Option<String>,
      outcome: EffortSetOutcome,
      /// The provider's text of a refusal.
      #[serde(default, skip_serializing_if = "Option::is_none")]
      #[ts(optional)]
      message: Option<String>,
  },
  /// The owner chose an effort for a new session; the station appends it
  /// in the start decision (B18 Phase 2). Older peers decode this kind as
  /// unknown.
  #[serde(rename = "effort.chosen")]
  EffortChosen { effort: String },
  ```

  `EffortSetOutcome` (`event_types.rs`, `#[serde(rename_all = "snake_case")]`): `Accepted` (the provider accepted the change; no level is reported yet; `effort` is `None`; emitted exactly once per accepted change, before its verification result), `Applied` (the provider reports the requested level), `Adjusted` (the provider reports another level or none: a default, a cap or a clamp), `Unverified` (the provider accepted the change but reports no effort), `Refused` (the provider refused the change), `Superseded` (a newer choice replaced it before it was sent).
- `SessionSummary.effort_choice: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]`). Fold: `EffortChosen { effort }` sets it; `EffortSetResult` with outcome `Accepted` sets it to `Some(requested)`; no other event changes it (`session.started` does not; `Applied`, `Adjusted`, `Unverified`, `Refused` and `Superseded` do not). `EffortSetResult` does not change `summary.effort`. Reason: the verification of an older change can come after a newer change was accepted (Claude verifies in id order after both acks); only the acceptance order is the choice order (decision 22).
- Station feature `feature::SESSION_EFFORT = "session_effort.v1"` in `feature::ALL` (`version.rs`). `StationView.session_effort: bool` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")] #[ts(optional, as = "Option<bool>")]`).

**Command (E8).** `SessionCommand::SetEffort { session_id: SessionId, effort: String }`, method `session.set_effort` (`command.rs` method and session-id tables, `mutation.rs` list). It is in E8, not E5, because the station `decide` match has no wildcard (§2.5): the variant and its decision come in one task.

**Providers (E6).** `LiveSession::set_effort(&mut self, effort: String, control_id: Option<ControlId>) -> Result<()>` (async, like `set_model`), default body `Err(Unsupported::new(Operation::SetEffort).into())`. `ProviderError::EffortNotOffered(String)` with the text "effort `{0}` is not offered by this model".

### 9.2 Behavior

1. **Deck gate (E8).** An *effort command* is a `session.start` or `session.import` whose `spec.effort` is `Some`, or a `session.set_effort`. The predicate `effort_command(envelope: &Value) -> bool` is in `link/router.rs`, next to `pinned_import`.
   - Admission (`http/command_routes.rs`, next to the import pin check at `:202-221`): when `link_feature(station, SESSION_EFFORT)` is `Without`, refuse with HTTP 409 `effort_unsupported`, text "This station cannot set the effort. Update it." `Negotiated` and `Down` are admitted (a station that is down is checked at the send).
   - Dispatch (`link/router.rs`, next to the import pin check at `:449-472`), at **every send and resend**: `Negotiated` sends; `Down` does not send (the command waits for a link or its expiry, as an import pin); `Without` settles the command `Refused { code: "effort_unsupported", message: <the same text> }`. So a command admitted on a new station link and sent after a reconnect to an older station is refused, never sent without its effort.
   - Table entry `cmd("session.set_effort", Scope::Drive)` (not a member command).
   - Other deck start paths (MCP `spawn`, `start_in_project`, batches, workflows) build specs with `effort: None` in this phase. If a later path sets it, the dispatch gate still applies.
2. **Station decide (E8).** `valid_effort` on `spec.effort` (start and import) and on `effort` (set); else reject `InvalidCommand` "use 1-16 of a-z, 0-9, `-`, `_`". A start or import with `spec.effort = Some(e)` appends `effort.chosen { effort: e }` in the start decision, next to `session.batch` (`decide.rs:594-607`; a redelivered start adds nothing). `SetEffort` → `live_session(.., Operation::SetEffort)` → `EffectPayload::SetEffort { effort }` (kind `set_effort`, correlation = the effort). Add `set_effort` to `OWNER_WORK` (`store/auto_continue.rs:25`).
3. **Dispatch to the provider (E8).** `target_base.rs`, `shim_claude.rs`, `shim_codex.rs`: `EffectPayload::SetEffort { effort }` → `set_effort(effort.clone(), Some(ControlId(effect.effect_id.as_str().to_owned())))`, the same control id rule as the interrupt (`target_base.rs:272`). An `Err` (for example `EffortNotOffered`) settles the effect at the same place and in the same way as an `Err` of `set_model` (`target_base.rs:292`).
4. **Effect settle (E8).** Move `observe` out of `store/effects.rs` into `store/effects_observe.rs` (pure move; `effects.rs` has 795 lines). Then add:
   - `EffortSetResult { control_id: Some(id), outcome: Accepted | Applied | Adjusted | Unverified, .. }` → `confirm("kind = 'set_effort' AND effect_id = ?3", &[&id.as_str()])`. `Accepted` comes first and confirms; the verification result of the same effect then matches no open row (the `confirm` filter is `state IN ('claimed', 'outcome_unknown')`, `effects.rs:626`) and changes nothing. A verification outcome without an earlier `Accepted` cannot come from E6 or E7; it confirms in the same way.
   - `EffortSetResult { control_id: Some(id), outcome: Refused, .. }` → state `retired_not_dispatched`, reason `REASON_REFUSED`, for `kind = 'set_effort' AND effect_id = ?3` in `claimed` or `outcome_unknown` (the pattern of the refused interrupt, `effects.rs:709-720`).
   - `outcome: Superseded` → the same with the new reason `REASON_SUPERSEDED = "superseded_by_a_newer_choice"`.
   - `effort.changed` confirms nothing.
   - An effect without any result stays `claimed` until its process ends; `close_process_effects` then makes it `outcome_unknown` (the same as a `set_model` today). That case is a change that the provider never accepted (the CLI ended before its ack; a Codex pick not yet sent with a turn); the choice is not persisted and the owner chooses again. An effect whose `Accepted` came is `confirmed` before the process ends, also when its verifying read never comes.
5. **Resume and restore (E8).** `manager/resume.rs` and `manager/shim_configs.rs` pass `effort: summary.effort_choice.clone()`: the owner's choice, never the observed value. The choice lives through the window between a launch and its first report, and through a process that never reports (an older CLI, a refused `get_settings`): `session.started` clears only the observed value. A session without a choice (spawn "Default") resumes with `None`. When the provider cannot verify the choice, the resume still passes it, and the board shows the model alone (observed unknown). The choice changes in a session at the provider's acceptance (`Accepted`, item 6 and 7), not at the verification: a Claude CLI that exits after the `apply_flag_settings` ack and before the `get_settings` answer resumes with the new level, and the summary gets no `effort.changed` for it (the new process reports its own value). An `Adjusted` choice resumes with the requested level; the provider clamps it again and the observed value shows.
6. **Claude (E6).** New code goes into `claude/effort_set.rs` (new); `map.rs` gets at most +6 lines.
   - Launch: `Launch.effort` → `--effort <e>` after `--model` (`args.rs`; the replay parser `tests/common` learns the flag). A `claude` that does not know `--effort` exits at the launch with its own error; the start fails visibly (accepted: every supported CLI has the flag, §3.1).
   - New core fields, all `#[serde(default)]` (§9.5): `model_choices: Vec<ModelChoice>` (from the `initialize` answer, with `resolved` and `efforts`), `applied_model: Option<String>` (from the last settings answer; `None` after a successful `set_model`), `model_set_seq: u64` (the `settings_seq` at the last successful `set_model`), `reported_model: Option<String>` (the model of the last emitted `session.started` or `model.changed`), `effort_seq: u64`, `effort_sets: BTreeMap<u64, EffortSet>` with

     ```rust
     #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
     pub(super) struct EffortSet {
         pub(super) requested: String,
         pub(super) control_id: Option<String>,
         /// `None`: waits for the `apply_flag_settings` answer. `Some(n)`:
         /// applied; waits for a `get_settings` answer with an id above `n`.
         pub(super) verify_after: Option<u64>,
     }
     ```

     The id of a set is `fd-effort-{effort_seq}`. Its answer is routed by the prefix `fd-effort-` in `on_control_response` before the `host_requests` lookup, like `fd-settings-` (§4.3 item 4). No `HostRequest` variant.
   - `set_effort(e, control_id)`: the current model is `applied_model.or(model)`. When `choice_for(&model_choices, current)` has `efforts: Some(list)` and `list` does not contain `e`: `Err(EffortNotOffered(e))`, nothing written. Else write `control_request("fd-effort-N", "apply_flag_settings", {"settings": {"effortLevel": e}})` and insert `EffortSet { requested: e, control_id, verify_after: None }`.
   - The answer of `fd-effort-N`:
     1. Error: remove the set; emit `EffortSetResult { outcome: Refused, effort: None, message: resp.error }`.
     2. Success, always first: emit `EffortSetResult { control_id, requested, effort: None, outcome: Accepted }`. This event persists the choice (§9.1 fold) and confirms the effect (item 4) before any verification, so a process that ends now keeps the choice for the resume.
     3. Success and `settings_stopped`: then remove the set; emit `EffortSetResult { outcome: Unverified, effort: None }`.
     4. Success otherwise: then `verify_after = Some(settings_seq)` and `settings_request()`. If a read is open, `settings_again` is set and its successor has a higher id; a stale open read is replaced at once. So the verifying read is always written after the change was applied.
     5. An ack never sets `core.effort` and never emits `effort.changed`.
   - A `get_settings` answer with id `fd-settings-S` (after the Phase 1 steps of §4.3 item 3):
     1. `applied_model` = `applied.model` when it is a string of 1 to 200 characters without control characters and `S > model_set_seq`. A read with `S <= model_set_seq` was written before the last model change: its model is old, so steps 1 and 2 do nothing for it.
     2. When `reported_model` is `Some(r)` and `!same_model(&model_choices, applied_model, r)`: set `model` and `reported_model` to `applied_model`; emit `ModelChanged { model: applied_model }`. When `reported_model` is `None` (a restored older checkpoint): set it, emit nothing. This closes the gap of §10.3: a `/model` change shows after the end of its turn, so the summary model is current for the effort choices.
     3. For every set with `verify_after = Some(n)` and `n < S`, in id order: remove it; emit `EffortSetResult { effort: observed, outcome: Applied if observed == Some(requested) else Adjusted }`.
   - When the asks stop (an error answer or another shape, §4.3 item 3 step 3): every set with `verify_after = Some(_)` is removed with `EffortSetResult { outcome: Unverified, effort: None }`.
   - `reported_model` is set where `session.started` is emitted (`map.rs:124-137`, the `init` model) and in the `SetModel` success arm.
   - The `SetModel` success arm also sets `applied_model = None` and `model_set_seq = settings_seq`, before its `settings_request()`. So `set_effort` checks the new model until a read that was written after the change answers, also when the asks stopped.
7. **Codex (E7).** New code goes into `codex/effort_set.rs` (new).
   - The per-session effort is `spec.effort.or(config.effort)` (`driver.rs:285`, `durable_launch.rs:61`). `open_thread` adds `"config": {"model_reasoning_effort": e}` when it is set (`thread/start`, `thread/resume`, `thread/fork`), so the thread answer reports it at once.
   - New core state, all `#[serde(default)]` (§9.5): `model_choices: Vec<ModelChoice>` (the last complete `model/list`, kept in `on_model_list`), `pending_effort: Option<EffortPick>` with `EffortPick { effort: String, control_id: Option<String> }`, and a new field of the existing variant: `HostRequest::TurnStart { input_id, #[serde(default)] effort: Option<EffortPick> }`.
   - `set_effort(e, control_id)`: the current model is `pending_model.or(thread_model).or(spec.model)`. When `choice_for(&model_choices, current)` has `efforts: Some(list)` without `e`: `Err(EffortNotOffered(e))`. When `pending_effort` is `Some(old)`: emit `EffortSetResult { control_id: old.control_id, requested: old.effort, effort: None, outcome: Superseded }`. Then `pending_effort = Some(EffortPick { effort: e, control_id })`. Nothing is written: like the model, the change goes with the next turn.
   - `turn_start`: `let pick = self.pending_effort.take();`. The sent effort is `pick.effort` or else `spec.effort`; it goes into `effort` and into `collaborationMode.settings.reasoning_effort`. The request is `HostRequest::TurnStart { input_id, effort: pick }`: the pick that was **sent** travels with its own request.
   - The `turn/start` answer, success (`on_turn_accepted`, which gets the request's `effort`): when it is `Some(p)`: `spec.effort = Some(p.effort)`; emit `EffortSetResult { control_id: p.control_id, requested: p.effort, effort: None, outcome: Accepted }` (the acceptance, which persists the choice), then `EffortSetResult { control_id: p.control_id, requested: p.effort, effort: thread_effort.clone(), outcome: Applied }` when `thread_effort == Some(p.effort)` (the `thread/settings/updated` of this turn came first, §3.1 Codex item 2), else `outcome: Unverified, effort: None`. `pending_effort` is not touched: a choice made while the request was open stays for the next turn.
   - The `turn/start` answer, error (`map.rs:166-174`): when the request's pick is `Some(p)`: if `pending_effort` is `None`, put `p` back (`pending_effort = Some(p)`), before `dispatch_follow_up()`, so the next turn sends it; else emit `Superseded` for `p`.
   - Codex reports no `Adjusted` and no `Refused` (it accepts any level, §3.1 Codex item 4); a refused turn keeps the choice pending.
8. **Board (E9).**
   - `web/src/session/effortChoice.ts` (new): `baseModel` and `choiceFor`, the TypeScript twins of §9.1 with the same cases in `effortChoice.test.ts`.
   - Composer: the model chip popover (`ModelMenu`) gets a group "Effort" under the models, with one radio row per level of `choiceFor(s.models, s.model)?.efforts`; the current `s.effort` is checked. Shown when `caps.set_effort` and the list is non-empty. A choice calls `actions.setEffort(level)` (`session/commands.ts`).
   - Spawn (`spawn/AgentFields.tsx`, `spawn/EffortField.tsx` new): a `<select>` with the label "Effort": first "Default" (value empty: no `effort` in the spec), then the levels of `choiceFor(models, chosenModel)`; with "Default model", the levels that every known model of that provider instance offers. Hidden when the station's `session_effort` is false or no level is known.
   - Timeline (`session/timeline.ts`, `chat/notices.ts`), case `'effort.set_result'`: `Accepted`, `Applied` and `Superseded` add no notice (an `effort.changed` notice shows a real change); `Adjusted` adds "Effort is {effort}, not {requested}" (tone `info`), or "This model does not use an effort setting" when `effort` is `null`; `Unverified` adds "Effort set to {requested}; the provider does not report it" (tone `info`); `Refused` adds "Effort not changed: {message}" or "Effort not changed." (tone `error`, as a refused interrupt).
   - Error text of a refused command (`effort_unsupported`, `InvalidCommand`): the deck or station message, in the existing command error line.

### 9.3 Build plan, Phase 2 (E5 → E6 → E7 and E8 in parallel → E9)

Every task runs the guards of §5, and the briefs give the exact commands of each acceptance. A compile error in a file that a task does not own stops the task: the worker reports a QUESTION.

| Id | Title | Kind | Owned files | Depends on |
| --- | --- | --- | --- | --- |
| E5 | Proto: `SessionSpec.effort`, `set_effort` capability and `Operation::SetEffort`, `ModelChoice.{resolved,efforts,default_effort}`, `base_model`/`choice_for`/`same_model`, `effort.set_result`, `effort.chosen`, `summary.effort_choice`, `session_effort.v1`, `StationView.session_effort`, and every mechanical literal | backend | `crates/fleetdeck-proto/src/{effort,command,capability,event,event_kinds,event_types,event_tests,fold,fold_effort_tests,session,board,version}.rs`, `crates/fleetdeck-proto/src/fold_tests.rs` (the `ModelChoice` literal only), `web/src/gen/**` (regenerated), and every file that `rg -l 'SessionSpec \{\|Capabilities \{\|ModelChoice \{' crates` prints (with `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/claude/tests_models.rs`, `crates/fleetdeck-providers/src/codex/tests_models.rs`, `crates/fleetdeck-proto/src/event_tests.rs`) for the new fields as `None`/`false` only | E1–E4 merged |
| E6 | Providers: `LiveSession::set_effort`, `EffortNotOffered`, Claude `--effort`, `apply_flag_settings` with the verifying read, `effort.set_result`, the model correction of §9.2 item 6, Claude model efforts | backend, high | `crates/fleetdeck-providers/src/adapter.rs`, `src/claude/{args,core,map,effort,effort_set,control,driver,durable_launch,host_cmd}.rs`, `src/claude/tests_effort_set.rs` (new), `tests/common/mod.rs`, `tests/claude_*.rs` as needed | E5 |
| E7 | Providers: Codex per-session effort, thread `config`, `pending_effort` with the pick in `TurnStart`, `effort.set_result`, model efforts | backend, high | `crates/fleetdeck-providers/src/codex/{core,core_types,effort,effort_set,map,driver,durable_launch,host_cmd}.rs`, `src/codex/tests_effort_set.rs` (new) | E6 |
| E8 | Station and deck: `SessionCommand::SetEffort`, decide, `effort.chosen`, payload, dispatch with the control id, `effects_observe.rs`, resume and restore from `effort_choice`, the deck gate at admission and at every send, the table, `local.rs` | backend, high | `crates/fleetdeck-proto/src/{command,mutation,command_tests}.rs` (the variant only), `web/src/gen/command/**` (regenerated), `crates/fleetdeck-station/src/manager/{decide,target_base,shim_claude,shim_codex,resume,shim_configs}.rs`, `crates/fleetdeck-station/src/manager/effort_tests.rs` (new), `crates/fleetdeck-station/src/store/{payload,effects,effects_observe,auto_continue}.rs`, `crates/fleetdeck-deck/src/http/{command_routes,methods_table_rest}.rs`, `crates/fleetdeck-deck/src/link/router.rs`, `crates/fleetdeck-deck/src/link/router_effort_tests.rs` (new), `crates/fleetdeck-deck/src/local.rs` | E6 (it calls `LiveSession::set_effort`); in parallel with E7 (no shared file) |
| E9 | Board: `effortChoice.ts`, the Effort group of the model popover, the spawn Effort field, `actions.setEffort`, the `effort.set_result` notices | ux | `web/src/session/{SessionSettings,Composer,SessionView}.tsx` (no new line in `SessionView.tsx`: it has 797), `web/src/session/{commands,timeline,effortChoice}.ts`, `web/src/chat/notices.ts`, `web/src/spawn/{AgentFields,SpawnForm}.tsx`, `web/src/spawn/EffortField.tsx` (new), their tests | E7, E8 |

Required tests (the briefs add the exact names):

- **E5:** `choice_for` cases: exact `value`; `resolved` (`claude-haiku-5-5` → the `haiku` choice); the context suffix (`claude-opus-5-5[1m]` → the choice with `resolved: claude-opus-5-5`); first match in list order (`default` and `opus` resolve to the same id); no match. `same_model("sonnet", "claude-sonnet-5-5")` is true; two different models are false. Fold: `effort.chosen` sets `effort_choice`; `Accepted` sets it to `requested`; `Applied`, `Adjusted`, `Unverified`, `Refused`, `Superseded` do not; `Accepted low`, `Accepted max`, then `Adjusted { requested: low }` leaves `max`; `Accepted low`, then `session.started` without any other effort event leaves `low`; `session.started` keeps it and clears `effort`; `effort.set_result` does not change `effort`. Round trips of both new kinds and of a `ModelChoice` without the new keys. Acceptance includes `heavy cargo check -p fleetdeck-proto -p fleetdeck-providers -p fleetdeck-station -p fleetdeck-deck -p fleetdeck-cli --tests` (the literals of every crate build).
- **E6:** a `set_effort` writes `apply_flag_settings` and emits nothing; its success ack emits exactly `[EffortSetResult { outcome: Accepted, effort: None, requested, control_id }]` (and the read write), and no `EffortChanged`; its success ack with a `get_settings` open writes no second read until the open one is answered, then writes a new one, and the answer of the **new** one (not the old one) emits `Applied` (after the `Accepted` of the ack); an answer `high` to a `max` request emits `Adjusted { effort: Some("high") }`; an answer `null` emits `Adjusted { effort: None }`; an error ack emits `Refused` with the message and no `Accepted`; an ack with `settings_stopped` emits `Accepted` then `Unverified`; an error answer of the verifying read emits `Unverified`; a level that the current model does not advertise gives `EffortNotOffered` and no write; the current model is the `resolved` id (`claude-haiku-5-5` matches the `haiku` choice); a `/model` turn whose settings answer has another model emits `model.changed` once; after `set_model sonnet` the answer `claude-sonnet-5-5` emits no `model.changed`. Checkpoint: an open `fd-effort-N` (both phases) survives a checkpoint and restore; a pre-E6 checkpoint restores; the rollback test of E2 extended with the E6 keys.
- **E7:** the interleaving: `set_effort low`, a prompt (the turn request carries `low`), `set_effort high` before the answer, the answer → `EffortSetResult { requested: low, outcome: Accepted }` then `EffortSetResult { requested: low }` (`Applied` after a `thread/settings/updated` of `low`), `pending_effort` is still `high`, and the next turn sends `high`; two `set_effort` calls before a turn → the first is `Superseded`; a refused turn puts its pick back when nothing newer waits, and emits `Superseded` when something newer waits; an accepted turn without a `low` report → `Accepted` then `Unverified`; a refused turn emits no `Accepted`; a level outside the `model/list` efforts of the current model → `EffortNotOffered`. Checkpoint: a `TurnStart` without the `effort` key decodes (pre-E7); a pending pick survives a restore.
- **E8:** admission: start, import and `set_effort` to a `Without` link → 409 `effort_unsupported`; a start without effort to the same link passes. Dispatch: a start with effort admitted while `Negotiated`, the link reconnects `Without` → the command settles `Refused effort_unsupported` and nothing is sent; `Down` → not sent, then sent when the link comes back `Negotiated`; the same for an import with effort and for `set_effort`. Station: an invalid effort is rejected; a start with effort appends `effort.chosen` once (a redelivery adds nothing); the effect settles `confirmed` on `Accepted`, `Applied`, `Adjusted` and `Unverified` with its own effect id (an `Applied` after `Accepted` leaves it `confirmed`), `retired_not_dispatched` on `Refused` and `Superseded`, and nothing on `effort.changed` or on a result with another effect id; resume and restore pass `effort_choice`, also when `summary.effort` is `None` after a `session.started` without a report. **Round 2 BLOCKER test** `an_accepted_choice_survives_an_exit_before_verification`: start with `effort: high` (`effort.chosen high`), `session.set_effort low`, the provider appends `effort.set_result { control_id: <effect id>, requested: low, outcome: accepted }`, then the process ends (`close_process_effects`) with no other effort event; assert the effect is `confirmed` (not `outcome_unknown`), `summary.effort_choice == Some("low")`, `summary.effort` has no value from `low` (no `effort.changed` was appended), and the resume `EffectPayload::Resume` spec has `effort: Some("low")`. A second case: `set_effort low` with no `Accepted` before the process ends → the effect is `outcome_unknown` and the resume spec has `effort: Some("high")`.
- **E9:** `choiceFor` the same cases as E5; the popover shows the levels of a Claude session whose `s.model` is a resolved id and of a `[1m]` model; each `effort.set_result` outcome gives the notice text of §9.2 item 8 (`accepted` adds no notice).

The architect writes the full briefs of E5–E9 after the adversary DESIGN REVIEW round 2 of this section.

### 9.4 Out of scope (Phase 2)

- A moved session (`MoveIn`, FG-9) carries no effort and no choice: it starts with the provider default.
- A composer choice "back to the default" (the levels only).
- The MCP and coordinator tools do not set the effort.

### 9.5 Checkpoint rules of Phase 2

E6 and E7 obey §4.3.1: every new core field has `#[serde(default)]`; no new `HostRequest` variant (Claude: `effort_sets` is its own field and its ids are routed by prefix; Codex: the pick is a defaulted field of the existing `TurnStart` variant); no `Instant` in the checkpoint; the version stays 1. A previous build that restores a checkpoint with an open set ignores the set. When the set was already acknowledged, its `Accepted` was emitted before the restore, so the choice and the confirmed effect stay. When the ack comes after the rollback, the previous build ignores it (an unknown id): no `Accepted`, the choice is not persisted, and the effect stays `claimed` until the process ends and becomes `outcome_unknown` (§9.2 item 4). `ModelChoice` and `SessionSpec` have no `deny_unknown_fields`, so their new keys in a checkpoint (`pending_models`, `HostRequest::ModelList`, the Codex `spec`) decode in both directions.

## 10. Backlog corrections and findings

1. B18 says the event to add is "like `session.model_changed`": the model event is `model.changed` (§2.1 item 2).
2. B18 says Codex sends the effort "on `thread/start` and each turn": it sends it on `turn/start` only (§2.2 item 2).
3. **Finding (separate gap):** a Claude model change through the `/model` slash command emits no `model.changed`; the core updates `core.model` from the next `init` silently (`claude/map.rs:107-120`). The board then shows the old model. The `get_settings` answer of E2 has `applied.model`. Phase 2 (E6) uses it: a settings answer whose model is not the reported model emits `model.changed` (§9.2 item 6), because the effort choices need the current model. Until E6, Phase 1 can show `Opus 5.5 · low` where `low` belongs to the new model.
4. **Finding:** the wall race pane on a display shows "default" as the model, because the display card has no model (`display_view.rs:60-96`, `RacePane.tsx:60`, `shortModel(null)`).
5. B18 lists two board places for the model; there are ten (§2.4).

## Decision log entries (to append)

- **DL-480 (B18, effort display):** New event `effort.changed { effort: Option<String> }` and `SessionSummary.effort` (optional, absent when not reported). `session.started` clears it. No feature flag: older peers drop the unknown kind and field. (docs/v2/design/effort.md §4.1)
- **DL-481 (B18, Claude source):** The Claude core asks `get_settings` after the first `init`, after each `result` and after a `set_model` answer (one open, a stale one replaced after 60 s, stopped at the first error, no chat error) and passes on only `applied.effort`. The settings files are not read. (§4.3)
- **DL-482 (B18, redaction):** The Claude driver writes a `get_settings` answer to the raw-frame log with `applied` only; the `effective` and `sources` parts (they hold the settings `env`, credentials included) never reach a log, an event or the store. (§4.3, §8)
- **DL-483 (B18, Codex source):** The Codex core reports the effort from the `thread/start|resume|fork` answer (`reasoningEffort`, else `thread.reasoningEffort`) and from `thread/settings/updated` of its own thread. `config.toml` is not read. (§4.2)
- **DL-484 (B18, validation):** An effort id is 1 to 16 of `a-z0-9_-` (`fleetdeck_proto::effort::valid_effort`); any other value counts as not reported. It is a syntax check only: a valid id that is not a real level (`bogus`) shows as reported. (§4.1)
- **DL-485 (B18, board):** The board shows `Opus 5.5 · high` through `modelText` wherever a session's model shows; Properties has an Effort row; an unknown effort shows nothing (never "default"). The ids show as the providers name them. (§4.4)
- **DL-486 (B18, chat):** The chat adds "Effort changed to x" only for a change after the first report of the current process; a `null` report keeps the last value. The model change notice keeps the model alone. (§4.4 items 4 and 5)
- **DL-487 (B18, wall):** The wall display allowlist stays without model and effort. (§6 decision 11)
- **DL-488 (B18, Phase 2):** Choosing the effort is a separate high-risk build: `session.set_effort`, `SessionSpec.effort`, capability `set_effort`, station feature `session_effort.v1`, events `effort.set_result` and `effort.chosen`, `summary.effort_choice`; it needs an adversary DESIGN REVIEW first. (§9)
- **DL-489 (B18, checkpoint):** The effort state of the provider cores is in `#[serde(default)]` fields, never in a new `HostRequest` variant; the `Instant` is skipped; the checkpoint version stays 1, so upgrade and rollback restore a live shim. (§4.3.1, §9.5)
- **DL-490 (B18, Phase 2 evidence):** A `set_effort` effect settles only on `effort.set_result` with its effect id: `accepted` (the provider's ack) confirms first; `applied`, `adjusted`, `unverified` report the verification (and confirm when nothing confirmed before); `refused`, `superseded` retire. Claude verifies with a `get_settings` read written after the change; Codex settles the pick that its turn request sent. An ack is never an observation. (§9.2)
- **DL-491 (B18, Phase 2 choice):** The owner's choice is `summary.effort_choice`, apart from the observed `summary.effort`; it is set by `effort.chosen` and by the `accepted` result at the provider's ack, never by a verification result; resume and restore pass the choice. (§9.2 item 5)
- **DL-492 (B18, Phase 2 gate):** An effort command needs `session_effort.v1` at admission and at every send and resend; `Down` waits, `Without` refuses. (§9.2 item 1)

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Show the effort levels as the provider ids (low, medium, high, xhigh, max, ultra) or as words ('extra high')? — **The recommended default: The provider ids, lower case, as the CLIs name them.**
2. Build Phase 2 (choose the effort in the spawn dialog and in the composer) after Phase 1, or not now? — **Yes (the owner asked for the whole backlog): build Phase 2 after Phase 1 merges, after its own adversary DESIGN REVIEW of §9.**
3. Should a wall display show model and effort? This needs a change to the display allowlist. — **The recommended default: No; make it a separate item if wanted.**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.

## Revision 2 (design review round 1)

The adversary DESIGN REVIEW round 1 (`/home/luismorales/fleetdeck-briefs/backlog/effort/adv-design-r1.md`, commit `00055138`) gave FAIL with 1 BLOCKER, 6 MAJOR and 3 MINOR findings. Each finding was checked against the code before the change. All 10 are fixed.

| # | Severity | Finding | Verified | What changed |
| --- | --- | --- | --- | --- |
| 1 | BLOCKER | The new Claude state has no compatible checkpoint form (`Option<Instant>`, missing fields in old checkpoints, `HostRequest::Settings` not decodable by the previous build). | Yes: `ClaudeCore` is the checkpoint (`claude/core.rs:164-165`, `claude/checkpoint.rs:17-20`); the version check does not decode the state; a failed restore marks the shim lost (`shim_station.rs:220`). | New §2.3 item 6 and §4.3.1. The settings state is in `#[serde(default)]` fields (`settings_open: Option<u64>` replaces the `HostRequest::Settings` variant); answers are routed by the id prefix before the `host_requests` lookup; `settings_asked` is `#[serde(skip)]` and a restored open request counts as stale; the version stays 1. Codex `thread_effort` is `#[serde(default)]`. E2 gets a fixture of the previous checkpoint and tests for upgrade, an open request across a restore, rollback decoding and "no settings text in the checkpoint"; E3 gets two checkpoint tests. §8 threats 6 and 7, decision 13, DL-489. Phase 2 follows the same rules (§9.5). |
| 2 | MAJOR | Codex can confirm an effort that was never sent (`pending_effort.take()` at the answer). | Yes: `TurnStart` keeps only `input_id` (`codex/core_types.rs:82-84`); the pending state is taken at the answer (`codex/map.rs:300-311`). | §9.2 item 7: `turn_start` takes the pick, and `HostRequest::TurnStart` carries it (`#[serde(default)] effort: Option<EffortPick>`); the answer settles that pick and leaves a newer pending choice; a second choice before a turn supersedes the first (`Superseded`); a refused turn puts the pick back or supersedes it. E7 tests the interleaving. Decision 19. |
| 3 | MAJOR | Claude reports the requested effort from the empty `apply_flag_settings` ack as an observation. | Yes: §3.1 Claude item 6 (the ack is `{}`); revision 1 emitted `EffortChanged { Some(e) }` from it and confirmed the effect on it. | §9.2 item 6: the ack emits nothing; a `get_settings` read written after the ack verifies the change; the new event `effort.set_result` (`Applied`, `Adjusted`, `Unverified`, `Refused`, `Superseded`) with the effect id as control id settles the effect (§9.2 item 4); `effort.changed` confirms nothing. Stopped reads give `Unverified`, not a false value. Decisions 17 and 18, DL-490. |
| 4 | MAJOR | The model lookup hides the effort picker for normal Claude sessions (`s.model` is the resolved id; `ModelChoice` has only `value`). | Yes: `claude/map.rs:647-662` keeps only `value`; the recording has `value: "haiku"`, `resolvedModel: "claude-haiku-5-5"` and `init` reports `claude-haiku-5-5`. | §2.3 item 7 (facts). §9.1: `ModelChoice.resolved`, and `base_model`, `choice_for` (value, then resolved, then without the `[1m]` suffix), `same_model` in `fleetdeck_proto::effort`, with a TypeScript twin. The providers validate against the current model (Claude `applied_model` or `model`; Codex `pending_model`, `thread_model`, `spec.model`). The stale `/model` summary (§10.3) is fixed in E6: a settings answer with another model emits `model.changed`. Decision 21. |
| 5 | MAJOR | The feature gate stops at HTTP admission; a reconnect to an older station and an import escape it. | Yes: the import pin is checked again at every send (`deck/src/link/router.rs:449-472`); `Import` carries a `SessionSpec` (`decide.rs:109-125`). | §9.2 item 1: an *effort command* is a start or import with `spec.effort`, or `set_effort`; the gate runs at admission and at every send and resend (`Negotiated` sends, `Down` waits, `Without` settles `Refused effort_unsupported`). E8 owns `link/router.rs` and tests the reconnect, the import and the offline case. Decision 20, DL-492. |
| 6 | MAJOR | Resume persistence depends on an observation that may never arrive. | Yes: `session.started` clears `summary.effort`; resume builds the spec from the summary only (`resume.rs:247-256`). | §9.1 and §9.2 item 5: `summary.effort_choice` (from `effort.chosen`, appended by the station in the start decision, and from a settled `effort.set_result`) is the owner's choice; `session.started` does not clear it; resume and restore pass it. With no verification, the choice still persists and the board shows the model alone. Decision 16, DL-491. |
| 7 | MAJOR | E5 cannot build alone: new `ModelChoice` fields break literals that E5 does not own. | Yes: literals in `proto/src/fold_tests.rs`, `proto/src/event_tests.rs`, both provider `map.rs`, both `tests_models.rs` (§2.5). | §9.3: E5 owns every file of `rg -l 'SessionSpec \{\|Capabilities \{\|ModelChoice \{' crates` for the new fields as `None`/`false`, and its acceptance runs `cargo check` of the dependent crates with `--tests`. `SessionCommand::SetEffort` moved from E5 to E8, because the station `decide` match has no wildcard. E8 now depends on E6 (it calls `set_effort`) and runs in parallel with E7. |
| 8 | MINOR | The timeline algorithm gives a notice for `low → null → low` and for the first report after a restart. | Yes: the timeline has no `session.started` case. | §4.4 item 5: `lastEffort` (non-null only) and `effortReported` (reset by a new `session.started` case). E4 tests the null and restart sequences. Decision 14, DL-486. |
| 9 | MINOR | `bogus` passes `valid_effort`, but §4.5 said it is treated as null. | Yes. | `valid_effort` is documented as a syntax check; §4.5 shows `bogus` as reported; the E1 test asserts that `bogus` passes and `bogus value` fails; decision 7 and DL-484 say so. Phase 2 checks chosen levels against the advertised levels. |
| 10 | MINOR | Model change notices do not show the effort although §3.3 promised all places. | Yes: `chat/notices.ts:80-84`. | Decision 15: the model notice keeps the model alone (the effort of the new model is not known at `model.changed`); the effort has its own notice; the event order of both providers is stated (§4.4 item 4); §3.3 says so; E4 tests the order. |

Not fixed: none. Finding 10 is resolved with a decision (no effort in the model notice) instead of the suggested change; the reason is in decision 15.

## Revision 3 (design review round 2)

The adversary DESIGN RE-REVIEW round 2 (`/home/luismorales/fleetdeck-briefs/backlog/effort/adv-design-r2.md`, commit `953af329`) gave FAIL with 1 BLOCKER. The other round 1 findings are fixed (resume persistence was partly fixed). The finding was checked against the code before the change.

| # | Severity | Finding | Verified | What changed |
| --- | --- | --- | --- | --- |
| 1 | BLOCKER | Resume loses an accepted Claude choice before verification: `effort_choice=high`, choose `low`, the `apply_flag_settings` ack comes, the CLI exits before the `get_settings` answer; the fold set the choice only from a verification result, so the resume passes `high`. | Yes: the resume spec is built from the summary only (`manager/resume.rs:247-256`); revision 2 set `effort_choice` only from `applied`/`adjusted`/`unverified`, which Claude emits after the verifying read; `close_process_effects` makes a `claimed` effect `outcome_unknown` at the process end (`store/effects.rs:738-758`); `confirm` matches only `claimed` and `outcome_unknown` (`effects.rs:626`). | New outcome `EffortSetOutcome::Accepted`, emitted exactly once per change that the provider accepts: Claude at the success ack of `apply_flag_settings` (§9.2 item 6, the first step of a success answer), Codex at the success answer of the `turn/start` that carried the pick (§9.2 item 7, before `applied`/`unverified`). The fold sets `effort_choice` **only** from `accepted` (and `effort.chosen`), never from a verification result, so a late verification of an older change cannot put back an older level (§9.1). `accepted` confirms the effect; the later verification result of the same effect is a no-op on the effect (§9.2 item 4). An ack still emits no `effort.changed` (no fabricated observation). The timeline adds no notice for `accepted` (§9.2 item 8). §9.2 item 5 states the exit-before-verification case; §9.5 states the rollback case (an ack after a rollback is not persisted). Tests: E5 fold (`accepted` sets the choice; verification outcomes do not; `accepted low`, `accepted max`, `adjusted low` leaves `max`), E6 (the ack emits `accepted` and no `effort.changed`), E7 (`accepted` before `applied`/`unverified`; none for a refused turn), E8 `an_accepted_choice_survives_an_exit_before_verification` (ack, exit before the settings answer, resume with the new level, no `effort.changed`; and the not-acknowledged case resumes with the old level), E9 (no notice). Decisions 16, 17, 18, new decision 22, §8 Phase 2 threats 2 and 3, DL-490, DL-491. Task ids and owned files do not change. |

Not fixed: none.

A related case, decided and not changed: a change that the provider never accepted (the Claude CLI ends before the ack; a Codex pick that waits for the next turn when the process ends) does not persist. Its effect becomes `outcome_unknown`, and the resume passes the previous choice. Reason: the station has no evidence that the provider took the level, and a level that the model refuses or does not offer must not persist; the owner chooses again.

## Build answers

- Pre-PR review (`adv-prepr-final.md`, MAJOR): a successful `set_model` left `applied_model` at the old model, so Haiku → Opus → `max` before the settings answer returned `EffortNotOffered`, and for the whole session when the asks had stopped. Fix: the `SetModel` success arm clears `applied_model` and records `model_set_seq`; a settings answer with an id at or below `model_set_seq` does not set `applied_model` or correct the model (§9.2 item 6). Tests: `claude/tests_effort_model.rs`.
