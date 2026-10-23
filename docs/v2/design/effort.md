# The reasoning effort next to the model (B18)

- **Status:** Draft, revision 1 (2026-10-23), for the architect. Code facts are on `v2` HEAD `497b2fbc`.
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
- A resume and a shim restore build the `SessionSpec` from `summary.model` (`manager/resume.rs:247-256`, `manager/shim_configs.rs:16-25`).
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

- Phase 1 shows it in every place where the model of a session shows (§2.4), the timeline notice included. The wall tiles show no model, so they show no effort. The wall display allowlist stays without model and effort (§6 decision 11).
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

   /// True for an effort id that a provider may report: 1 to 16 of `a-z`,
   /// `0-9`, `-`, `_` (`low`, `xhigh`, `max`, `ultra`).
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
   - In the `SessionStarted` arm: `self.effort = None;` (a new process reports its own effort at once; a stale value from an earlier process is never shown).
6. `make ts-types` regenerates `web/src/gen/event/Event.ts` and `web/src/gen/session/SessionSummary.ts`.

### 4.2 Codex mapping (`fleetdeck-providers`)

New file `crates/fleetdeck-providers/src/codex/effort.rs` (`mod effort;` in `codex/mod.rs`), with `impl CodexCore`:

1. A new field in `CodexCore` (`core.rs`): `pub(super) thread_effort: Option<String>`, `None` in `new`.
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
2. `core.rs`:
   - `HostRequest::Settings` (doc: "`get_settings`: the applied effort (B18)").
   - Fields: `settings_seq: u64`, `settings_asked: Option<Instant>`, `settings_again: bool`, `settings_stopped: bool`, `effort: Option<String>` (all zero or `None` in `new`).
3. `effort.rs`:

   ```rust
   /// The id prefix of the host's `get_settings` requests.
   pub const SETTINGS_ID_PREFIX: &str = "fd-settings-";
   /// After this time an unanswered request no longer blocks a new one.
   const SETTINGS_STALE: Duration = Duration::from_secs(60);
   ```

   - `pub(super) fn settings_request(&mut self) -> Vec<Output>` (on `ClaudeCore`): no output when `settings_stopped` or `!started`. When a `Settings` request is open and was asked less than `SETTINGS_STALE` ago: set `settings_again = true`, no output. When it is older: remove the old id from `host_requests` (a late answer is then ignored as an unknown id). Else `settings_seq += 1`, id `fd-settings-{settings_seq}`, insert `HostRequest::Settings`, set `settings_asked`, write `control::get_settings(&id)`.
   - `pub(super) fn on_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output>`: when `!resp.ok` or `resp.response["applied"]` is not an object: `settings_stopped = true`, one `tracing::debug!` with the error text, no output (no `error` event). Else `let effort = effort_of(applied.get("effort").and_then(Value::as_str));`. Output: `EffortChanged { effort }` when `effort != core.effort` (then `core.effort = effort`). Then, if `std::mem::take(&mut core.settings_again)`, append `core.settings_request()`.
   - `pub fn raw_view(frame: &Value) -> Cow<'_, Value>`: when `frame["type"] == "control_response"` and `frame["response"]["request_id"]` is a string that starts with `SETTINGS_ID_PREFIX`, return an owned copy with `response.response` replaced by `{"applied": <response.response.applied or null>}`; every other key of `response` (`subtype`, `request_id`, `error`) stays. Every other frame is returned borrowed and unchanged.
4. `map.rs` (net +6 lines at most):
   - `on_system` `init`, after `out.extend(core.usage_request());` (`map.rs:153`): `out.extend(core.settings_request());`.
   - `on_result`, after `out.extend(core.usage_request());` (`map.rs:579`): `out.extend(core.settings_request());`. This covers `/effort`, `/model` and every other turn.
   - `on_control_response`, next to the `Usage` routing (`map.rs:688-690`): `if kind == HostRequest::Settings { return effort::on_response(core, resp); }`.
   - The `HostRequest::SetModel` success arm (`map.rs:735-739`): after the `ModelChanged` output, append `core.settings_request()` (a model change can change or clamp the effort).
   - Add `HostRequest::Settings` to the `Interrupt(_) | Usage => Vec::new()` arm (`map.rs:741`).
5. `driver_io.rs` `feed_line`: log `effort::raw_view(&v)` instead of `v` (`shared.log(RawDir::Out, &effort::raw_view(&v))`). The mapping still gets the full frame; the core reads only `applied.effort` and keeps nothing else.
6. Replays: the core writes one `get_settings` after the first `init` of each process. Each recorded fixture gets that write as an `in` record right after its `fd-usage-1` record (22 records, the same method as commit `0060c90a` for `get_usage`). No fixture answers it, so the request stays open and no other `get_settings` is written in a replay.

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
4. `chat/notices.ts`: `export const effortNotice = (effort: string): NoticeText => ({ text: \`Effort changed to ${effort}\`, title: \`Reasoning effort: ${effort}\` });`
5. `session/timeline.ts` `buildTimeline`: a local `let effort: string | null | undefined;` (undefined = no report yet). Case `'effort.changed'`: when `effort !== undefined && e.effort !== null && e.effort !== effort`, add `{ type: 'notice', key, ...effortNotice(e.effort), tone: 'info' }`; then `effort = e.effort`. So the first report after a start, a repeat after a resume, and a `null` add no notice.
6. CSS: no change. The strip, header and rail model cells already cut with an ellipsis (`home.css:756-771`); the `title` keeps the whole text.

### 4.5 Errors and edge cases

| Case | Behavior |
| --- | --- |
| Claude CLI without `get_settings`, or a connection that refuses it ("get_settings is not available on this connection") | Error answer: no more asks in this process, no event, no chat error. The board shows the model alone. |
| Claude model without effort (`applied.effort: null`) | `effort.changed {effort: null}` only if a value was shown; the board shows the model alone. |
| `/effort auto` | Claude reports the resolved value; it shows. |
| A provider value that fails `valid_effort` (for example `"bogus"` from a Codex config, or text over 16 characters) | Treated as `null`. |
| A Codex sub-agent thread changes its settings | Ignored (`own_thread`). |
| Station restart with a restored shim | The new core has no effort; its next report emits the same value again; the fold and the timeline show no change. |
| Process restart (resume) | `session.started` clears `summary.effort`; Codex reports in the thread answer, Claude after `init` (one control round trip). |
| Owner changes the effort in a terminal that holds the session (DL-140) | The board shows the new value after the next driven turn. |
| A `get_settings` answer never comes | After 60 s a new ask replaces it. |

### 4.6 Compatibility

- Phase 1 adds no command and no feature flag. An older deck decodes `effort.changed` as an unknown kind and drops the unknown summary field. An older station sends neither: `effort` is absent and the board shows the model alone, as today. Stored summaries decode with `effort: None`.
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
  - `effort::tests::valid_effort_accepts_provider_ids_and_refuses_others`: `low`, `xhigh`, `max`, `ultra`, `x_1-2` pass; `""`, `High`, `very high`, a 17-character id fail.
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
- **Owned files:** `crates/fleetdeck-providers/src/claude/effort.rs` (new), `crates/fleetdeck-providers/src/claude/mod.rs`, `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/control.rs`, `crates/fleetdeck-providers/src/claude/driver_io.rs`, `crates/fleetdeck-providers/src/claude/tests_effort.rs` (new), `crates/fleetdeck-providers/src/claude/tests_usage.rs`, `crates/fleetdeck-providers/tests/claude_core_replay.rs`, `crates/fleetdeck-providers/tests/claude_driver_replay.rs`, `crates/fleetdeck-providers/tests/claude_sdk_reference.rs`, `spikes/s1-claude/fixtures/rust/*.jsonl` (the 19 files, one inserted record per process), `spikes/s1-claude/fixtures/rust/effort_settings.jsonl` (new).
- **Changes:** §4.3 items 1 to 6, and:
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
  - `raw_view_keeps_only_applied`: the frame of fixture item 3 gives `{"type":"control_response","response":{"subtype":"success","request_id":"fd-settings-1","response":{"applied":{...}}}}`; its JSON text has no `fd-test-secret`; an error answer keeps `error`; a `get_usage` answer and an `assistant` frame are returned unchanged and borrowed.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(effort) | test(usage) | binary(claude_core_replay) | binary(claude_driver_replay) | binary(claude_sdk_reference) | binary(claude_driver)'`
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
  - `codex_core_replay::multi_turn_reports_the_effort_of_the_thread_then_of_the_turn` (new, in `tests/codex_core_replay.rs`): the `effort.changed` events of `multi_turn` are `Some("high")` then `Some("low")`, and both come after `session.started`.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(effort) | binary(codex_core_replay) | binary(codex_driver_replay) | binary(codex_schema) | binary(codex_driver_sim)'`
  - the guards.

### E4. Board: `Opus 5.5 · high`, the Effort row and the change notice (ux, about 200 lines)

- **Depends on:** E1 (the generated types). Can run in parallel with E2 and E3.
- **Owned files:** `web/src/ui/format.ts`, `web/src/ui/format.test.ts`, `web/src/chat/notices.ts`, `web/src/session/timeline.ts`, `web/src/session/timeline.test.ts`, `web/src/session/HeaderBadges.tsx`, `web/src/session/PropertiesPanel.tsx`, `web/src/session/properties.test.tsx`, `web/src/session/SessionView.tsx`, `web/src/app/Sidebar.tsx`, `web/src/board/FleetStrip.tsx`, `web/src/board/NeedsYouRail.tsx`, `web/src/compare/ComparePage.tsx`, `web/src/wall/RacePane.tsx`.
- **Changes:** §4.4 items 1 to 5. Follow `docs/v2/DESIGN-SYSTEM.md` (§5 Models: the model and its effort are not colored; no mono). Add one line to DESIGN-SYSTEM §5 Models through the architect, not in this task (the file is not owned).
- **Tests:**
  - `format.test.ts`: `modelText('claude-opus-5-5[1m]', 'high')` is `Opus 5.5 · high`; `modelText('gpt-6.1-sol', 'xhigh')` is `GPT-6.1 Sol · xhigh`; `modelText('claude-opus-5-5', undefined)`, `null` and `''` are `Opus 5.5`; `modelText(null, 'high')` is `default · high`; `modelTitle('claude-opus-5-5[1m]', 'high')` is `claude-opus-5-5[1m] · reasoning effort high`.
  - `timeline.test.ts`: the first `effort.changed` adds no notice; `high` then `low` adds "Effort changed to low" with the title "Reasoning effort: low"; the same value twice adds nothing; `null` adds nothing; `low` then `null` then `low` adds nothing.
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
7. An effort is passed on only when it is 1 to 16 of `a-z0-9_-`. Rejected: pass any string (Codex echoes any config text, for example `bogus`); a fixed list of levels (Codex has `ultra`, a later CLI can add a level).
8. An unknown effort shows nothing. Rejected: "default" (wrong when the provider does not report; the board never says "unknown").
9. Compact places show `Opus 5.5 · high` through one helper `modelText`; Properties gets its own Effort row. Rejected: a badge or a color for the effort (models are never colored, DESIGN-SYSTEM §1); `Opus 5.5 · high` also in the Properties Model row (the row has the context aside already).
10. The chat shows "Effort changed to x" only for a change after a first report. Rejected: a notice for every report (each start and resume would add a line); no notice (the owner asked to see a `/effort` change at once).
11. The wall display gets no effort. Rejected: add model and effort to the display allowlist (a display allowlist change is its own security decision, §10.4).
12. Choosing the effort is Phase 2 with its own command, capability and station feature (§9). Rejected: put it in Phase 1 (it is a new command path: high risk; the display is useful alone).

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
- Recommended review: adversary CODE REVIEW of the E2 branch (the redaction and the "nothing else kept" rule), with the check command of E2.

## 9. Design, Phase 2 (choose the effort) — high risk, adversary DESIGN REVIEW before the build

### 9.1 Shapes

- `fleetdeck_proto::command::SessionSpec.effort: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]`). Every struct literal in `crates/` adds `effort: None` (87 places, mechanical).
- `SessionCommand::SetEffort { session_id: SessionId, effort: String }`, method `session.set_effort` (`command.rs` name table and session-id table, `mutation.rs` list).
- `Capabilities.set_effort: bool` (`#[serde(default)]`), `Operation::SetEffort` (degradation `Hide`, like `SetModel`). Claude and Codex: `true`.
- `ModelChoice.efforts: Option<Vec<String>>` and `ModelChoice.default_effort: Option<String>` (both `#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]`). Claude: `supportedEffortLevels` (no default). Codex: `supportedReasoningEfforts[].reasoningEffort` and `defaultReasoningEffort`. Each value through `valid_effort`.
- Station feature `feature::SESSION_EFFORT = "session_effort.v1"` in `feature::ALL`. `StationView.session_effort: bool` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")] #[ts(optional, as = "Option<bool>")]`), set in `deck/src/local.rs` like `coordinator_live`.
- `LiveSession::set_effort(&mut self, effort: String) -> Result<()>` with the default body `Err(Unsupported::new(Operation::SetEffort).into())`; `ProviderError::EffortNotOffered(String)` with the text "effort `{0}` is not offered by this model".

### 9.2 Behavior

1. **Deck** (`http/command_routes.rs`, the pattern of the import pin at `:202-221`): a `session.start` whose `spec.effort` is set, and every `session.set_effort`, need `feature::SESSION_EFFORT` on the station link; else HTTP 409 `effort_unsupported`, text "This station cannot set the effort. Update it." Table entry `cmd("session.set_effort", Scope::Drive)` (not a member command).
2. **Station decide:** `valid_effort` on `spec.effort` (start) and on `effort` (set); else reject `InvalidArgument` "use 1-16 of a-z, 0-9, `-`, `_`". `SetEffort` → `live_session(.., Operation::SetEffort)` → `EffectPayload::SetEffort { effort }` (kind `set_effort`, correlation = the effort). Add `set_effort` to `OWNER_WORK` (`store/auto_continue.rs:25`).
3. **Effect confirm:** move `observe` out of `store/effects.rs` into `store/effects_observe.rs` (pure move; `effects.rs` has 795 lines), then add `Event::EffortChanged { effort: Some(e) } => confirm("kind = 'set_effort' AND correlation = ?3", &[&e.as_str()])`.
4. **Resume and restore:** `manager/resume.rs` and `manager/shim_configs.rs` pass `effort: summary.effort.clone()` (the same rule as the model: a resumed session keeps what it had).
5. **Claude:** `Launch.effort` → `--effort <e>` after `--model` (`args.rs`; the replay parser `tests/common` learns the flag). `set_effort` → `apply_flag_settings {"settings":{"effortLevel": e}}` as `HostRequest::SetEffort(e)`; a success answer emits `EffortChanged { Some(e) }` (the confirm), sets `core.effort`, and calls `settings_request()` (a clamp then shows the real value). The request is refused with `EffortNotOffered` when the advertised `supportedEffortLevels` of the current model are known and do not have `e`. The flag layer lives only in the process: the resume rule of item 4 keeps it.
6. **Codex:** the per-session effort is `spec.effort.or(config.effort)` (`driver.rs:285`, `durable_launch.rs:61`). `open_thread` adds `"config": {"model_reasoning_effort": e}` when it is set (so the thread answer reports it at once). `set_effort` stores `pending_effort` (refused with `EffortNotOffered` when the `model/list` efforts of the thread model are known and do not have `e`). `turn_start` sends `pending_effort.or(spec.effort)` as `effort` and in `collaborationMode`. On the turn answer (`on_turn_accepted`): `pending_effort.take()` → `spec.effort = Some(e)`, `thread_effort = Some(e)`, emit `EffortChanged { Some(e) }` (the confirm; a duplicate of a `thread/settings/updated` report is harmless). Like the model, a change while idle is applied with the next prompt.
7. **Board:**
   - Composer: the model chip popover (`ModelMenu`) gets a group "Effort" under the models, with one radio row per level of the current model (`s.models` entry of `s.model`, its `efforts`); the current `s.effort` is checked. Shown when `caps.set_effort` and the levels are known. A choice calls `actions.setEffort(level)`.
   - Spawn (`spawn/AgentFields.tsx`): a `<select>` with the label "Effort": first "Default" (value empty: no `effort` in the spec), then the levels of the chosen model; with "Default model", the levels that every known model of that provider instance offers. Hidden when the station's `session_effort` is false or no level is known.
   - Error text of a refused set: the deck or station message, in the existing command error line.

### 9.3 Build plan, Phase 2 (sequential: E5 → E6 → E7 → E8 → E9; E8 can run in parallel with E7)

| Id | Title | Kind | Owned files | Depends on |
| --- | --- | --- | --- | --- |
| E5 | Proto: `SessionSpec.effort`, `SetEffort`, `set_effort` capability, `ModelChoice.efforts`, `session_effort.v1`, `StationView.session_effort`, every `SessionSpec`/`Capabilities` literal | backend | `crates/fleetdeck-proto/src/{command,mutation,capability,event_types,board}.rs`, `crates/fleetdeck-proto/src/version.rs`, `web/src/gen/**` (regenerated), every file listed by `rg -l 'SessionSpec \{|Capabilities \{' crates` | E1–E4 merged |
| E6 | Providers: `LiveSession::set_effort`, Claude `--effort` and `apply_flag_settings`, Claude model efforts | backend | `crates/fleetdeck-providers/src/adapter.rs`, `src/claude/{args,core,map,effort,control,driver,durable_launch,host_cmd}.rs`, `src/claude/tests_effort_set.rs` (new), `tests/common/mod.rs`, `tests/claude_*.rs` as needed | E5 |
| E7 | Providers: Codex per-session effort, thread `config`, `pending_effort`, model efforts | backend | `crates/fleetdeck-providers/src/codex/{core,core_types,effort,map,driver,durable_launch,host_cmd}.rs`, `src/codex/tests_effort_set.rs` (new) | E6 |
| E8 | Station and deck: decide, payload, `effects_observe.rs`, target and shim dispatch, resume, the deck gate and table, `local.rs` | backend | `crates/fleetdeck-station/src/manager/{decide,target_base,shim_claude,shim_codex,resume,shim_configs}.rs`, `crates/fleetdeck-station/src/store/{payload,effects,effects_observe,auto_continue}.rs`, `crates/fleetdeck-deck/src/http/{command_routes,methods_table_rest}.rs`, `crates/fleetdeck-deck/src/local.rs` | E6 |
| E9 | Board: the Effort group of the model popover, the spawn Effort field, `actions.setEffort` | ux | `web/src/session/{SessionSettings,Composer,SessionView}.tsx` (no new line in `SessionView.tsx`: it has 797), `web/src/spawn/{AgentFields,SpawnForm}.tsx`, `web/src/spawn/EffortField.tsx` (new), the session actions file, their tests | E7, E8 |

The architect writes the full briefs of E5–E9 after the adversary DESIGN REVIEW of this section.

## 10. Backlog corrections and findings

1. B18 says the event to add is "like `session.model_changed`": the model event is `model.changed` (§2.1 item 2).
2. B18 says Codex sends the effort "on `thread/start` and each turn": it sends it on `turn/start` only (§2.2 item 2).
3. **Finding (separate gap):** a Claude model change through the `/model` slash command emits no `model.changed`; the core updates `core.model` from the next `init` silently (`claude/map.rs:107-120`). The board then shows the old model. The `get_settings` answer of E2 has `applied.model`; a later item can use it.
4. **Finding:** the wall race pane on a display shows "default" as the model, because the display card has no model (`display_view.rs:60-96`, `RacePane.tsx:60`, `shortModel(null)`).
5. B18 lists two board places for the model; there are ten (§2.4).

## Decision log entries (to append)

- **DL-480 (B18, effort display):** New event `effort.changed { effort: Option<String> }` and `SessionSummary.effort` (optional, absent when not reported). `session.started` clears it. No feature flag: older peers drop the unknown kind and field. (docs/v2/design/effort.md §4.1)
- **DL-481 (B18, Claude source):** The Claude core asks `get_settings` after the first `init`, after each `result` and after a `set_model` answer (one open, a stale one replaced after 60 s, stopped at the first error, no chat error) and passes on only `applied.effort`. The settings files are not read. (§4.3)
- **DL-482 (B18, redaction):** The Claude driver writes a `get_settings` answer to the raw-frame log with `applied` only; the `effective` and `sources` parts (they hold the settings `env`, credentials included) never reach a log, an event or the store. (§4.3, §8)
- **DL-483 (B18, Codex source):** The Codex core reports the effort from the `thread/start|resume|fork` answer (`reasoningEffort`, else `thread.reasoningEffort`) and from `thread/settings/updated` of its own thread. `config.toml` is not read. (§4.2)
- **DL-484 (B18, validation):** An effort id is 1 to 16 of `a-z0-9_-` (`fleetdeck_proto::effort::valid_effort`); any other value counts as not reported. (§4.1)
- **DL-485 (B18, board):** The board shows `Opus 5.5 · high` through `modelText` wherever a session's model shows; Properties has an Effort row; an unknown effort shows nothing (never "default"). The ids show as the providers name them. (§4.4)
- **DL-486 (B18, chat):** The chat adds "Effort changed to x" only for a change after a first report. (§4.4 item 5)
- **DL-487 (B18, wall):** The wall display allowlist stays without model and effort. (§6 decision 11)
- **DL-488 (B18, Phase 2):** Choosing the effort is a separate high-risk build: `session.set_effort`, `SessionSpec.effort`, capability `set_effort`, station feature `session_effort.v1`; it needs an adversary DESIGN REVIEW first. (§9)

## Architect decisions (2026-10-23)

The architect answered the owner questions of §Owner questions. These answers override the draft where they differ.

1. Show the effort levels as the provider ids (low, medium, high, xhigh, max, ultra) or as words ('extra high')? — **The recommended default: The provider ids, lower case, as the CLIs name them.**
2. Build Phase 2 (choose the effort in the spawn dialog and in the composer) after Phase 1, or not now? — **Yes (the owner asked for the whole backlog): build Phase 2 after Phase 1 merges, after its own adversary DESIGN REVIEW of §9.**
3. Should a wall display show model and effort? This needs a change to the display allowlist. — **The recommended default: No; make it a separate item if wanted.**

Migration numbers (architect, to avoid collisions between parallel tracks): station V43 is taken by the DL-402 fix (b0cdbf5f). Station: V44 resume-after-stop, V45 workflow-agents, V46 mcp-apps, V47 usage-cost. Deck: V48 wall-control, V49 the wake-key fix (G5), V50 usage-cost. A track uses only its own numbers; the merge order follows the numbers, else the architect renumbers at merge.
