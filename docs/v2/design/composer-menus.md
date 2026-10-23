# Composer menus: `/` commands, Codex skills and `@` files (B11)

- **Status:** Draft 1 (2026-10-23), design agent of the backlog program. The architect decides.
- **Backlog item:** B11 (`docs/v2/BACKLOG.md`), owner report 2026-10-22.
- **Base:** `v2` at `497b2fbc`. Every file:line below is on that commit.
- **Recordings:** `/home/luismorales/fleetdeck-briefs/backlog/composer-menus/` (scrubbed: no account data, no e-mail address). `claude` 2.1.308, `codex` 0.163.0.
- **Goal:** `/` lists every command and skill that the session can run, at the positions where the provider runs them; Codex sessions get their skills; the menu scrolls past 8 rows; `@` shows files on the bare `@`; every empty state says what happens.

## 1. Problem and verified facts

### 1.1 The board (web)

1. `matchSlash` (`web/src/session/slash.ts:14-32`) reads the whole text with `/^\/(\S*)$/` (`slash.ts:19`). The menu opens only when the full text is `/` plus one word. A leading space (`' /c'`), a second word, a new line or a caret in the middle give no menu. The test `controls.test.tsx:113` asserts `' /c'` → `null`.
2. `MAX_SLASH_OPTIONS = 8` (`slash.ts:4`) cuts the list (`slash.ts:29`). The CSS already scrolls: `.fd-composer-menu` has `max-height: 240px; overflow-y: auto` (`web/src/styles/session.css:1474-1486`). So the cap, not the CSS, hides the other commands.
3. Order: the commands that start with the query, then the commands that contain it, both in the frame order (`slash.ts:23-28`). The Claude frame order puts the user skills and plugin commands first and the built-in commands last (recording, §2.1). With the empty query `/`, the 8 rows are 8 user skills; `/compact`, `/context` and `/init` never show.
4. The composer opens no `/` menu when the list is empty: `hasCommands` (`web/src/session/Composer.tsx:254-256`). It then shows the note "This session reports no commands. The text goes to the agent as a prompt." under the box (`Composer.tsx:527-531`).
5. The list is `s.slash_commands ?? slashCommands(events)` and `s.skills ?? []` (`web/src/session/SessionView.tsx:637-638`). The fold keeps the last event (`crates/fleetdeck-proto/src/fold.rs:228-231`). So a resumed session keeps its old list; only a session that never ran a turn has none.
6. `matchAt` (`web/src/session/at.ts:15-21`) works at the caret at any word start, also on the bare `@`. The search starts at 2 characters (`MIN_AT_QUERY = 2`, `at.ts:7`; the station refuses less: `MIN_QUERY_CHARS = 2`, `crates/fleetdeck-proto/src/files.rs:30`).
7. **Correction to the backlog:** the bare `@` (and `@a`) does open the menu, with the one status row "Type 2 or more letters to search files" (`Composer.tsx:275-279`; e2e `web/e2e/menus.spec.ts:134-135`). It shows no file. The menu does not open when the session has no file search (`searchFiles` is `null`: no `files` actions, or not `filesReady`, `SessionView.tsx:244, 334-338`), and never while the `/` or `#` menu is open (`Composer.tsx:258-260`).

### 1.2 Claude (station side)

1. The Claude core emits `session.slash_commands` once, from the first `system/init` (`crates/fleetdeck-providers/src/claude/map.rs:148-152`). Later inits return early (`map.rs:117-119`). The list is `slash_commands` minus `terminal_slash_commands`, without a leading `/` (`map.rs:229-238`). `skills` is the `skills` array (`map.rs:149`).
2. **The CLI sends no `init` before the first prompt.** Recorded (`raw-initialize.jsonl`): a process that gets only `initialize` sends hook frames, the `initialize` answer and one `system/commands_changed`, and no `init`. So a session that was started without a prompt has no list until its first turn.
3. **Correction to the backlog:** the list is available before the first turn. The `initialize` answer has `commands`: 141 objects `{name, description, argumentHint, aliases?, builtin?}` (recorded). The core sends `initialize` at every process start (`claude/driver.rs:209`) but reads only `models` and `current_permission_mode` from the answer (`map.rs:711-727`). The frame `system/commands_changed` (`{commands: [...]}`, same objects, 147 after the MCP servers connect) falls into `_ => Vec::new()` (`map.rs:183`).
4. Recorded list sizes (`command-lists.json`): `initialize` 141; `commands_changed` 147 (adds 6 MCP prompts `mcp__rea__*`); the first `init` 141, later inits 147; `skills` 106; `terminal_slash_commands` = `doctor`, `color`, `focus`, `reload-plugins`.
5. **Correction to the backlog:** "a skill that is not also in `slash_commands` never shows" is true for the code, but on 2.1.308 all 106 skills are in `slash_commands` (recorded). It is not a cause of the missing rows; facts 1.1.2 and 1.1.3 are.
6. The core holds the events of frames before the first `init` and releases them after `session.started`, except the answers to its own control requests (DL-141; `claude/core.rs:484-509`). The replay test allows only `mode.changed`, `model.changed`, `interrupt.result` and `error` before `session.started` (`crates/fleetdeck-providers/tests/claude_core_replay.rs:126-143`). `session.models` waits in `pending_models` (DL-151).
7. The terminal-only filter is careful, not required: `/color` runs in `-p` mode (recorded: "Session color set to: pink", `raw-terminal.jsonl`).

### 1.3 Codex (station side)

1. Codex emits no `session.slash_commands`. The word `skills` appears only in the fleet skill injection: `skills/extraRoots/set` and `skills/list {forceReload}` in the handshake (`codex/core_skills.rs`, `codex/driver_skill.rs`); the answer is dropped (`skill_answer`). `skills/changed` is in the ignored notifications (`codex/map.rs:55`).
2. The app-server 0.163.0 has `skills/list` (`SkillsListParams {cwds?, forceReload?}` → `{data: [{cwd, skills: SkillMetadata[], errors}]}`; `SkillMetadata {name, description, enabled, scope, path, ...}`) and the notification `skills/changed`. The pinned schema 0.160.0 has them too (`spikes/s2-codex/schema/0.160.0/`). There is **no** method that lists the TUI slash commands or the custom prompts (full method list in `codex-ts/ClientRequest.ts` of the recording folder).
3. The app-server does not run slash commands. Recorded (`raw-codex-skill.jsonl`): `turn/start` with the text `/compact` sends the text to the model ("This chat only contains requests to reply OK"). `/status` also became a model call (`raw-codex.jsonl`).
4. The app-server loads a skill that the text names with `$name`, anywhere in the text. Recorded A/B (`raw-codex-skill-ab.jsonl`, input tokens of each turn): `$not-a-skill` +1,515 (a one-time growth of turn 2, also seen in the first run), `$find-skills` +1,367 (the 5,472-byte `SKILL.md`), the bare word `find-skills` +22.
5. Codex replays: recordings hold no `model/list`, so `CoreConfig.request_models` (false in replays and checkpoint placeholders) gates the catalog request (DL-151; `codex/mod.rs:264-266`, `codex/map.rs:257-259`).

### 1.4 Where Claude accepts a command (recorded, `raw-turns.jsonl`, `raw-midskill.jsonl`, `raw-blocks.jsonl`)

| Prompt text sent to the CLI | Result |
| --- | --- |
| `/context` | the CLI runs the command (no model call, cost 0) |
| `Say OK. /context` | plain text to the model |
| `  /context` (leading spaces) | plain text to the model |
| `Say OK.\n/context` | plain text to the model |
| `/find-skills Reply with the single word OK.` | the CLI expands the skill; the rest is `command-args` |
| `Reply with the single word OK. /find-skills` | plain text; the **model** chose to call the `Skill` tool (not guaranteed) |
| `/nosuchcommand-xyz` | plain text to the model, which says that the command is not installed (a paid turn) |
| content blocks `[shell results..., {text: "/context"}]` | the CLI runs the command (the last text block counts) |

The board trims the text before it sends it (`inputOf`, `Composer.tsx:99-109`). So for the board, "the start of the prompt" is the first character that is not a space. Shell chips (B8) and images make a block array; the command still runs (last row).

## 2. Answers to the questions of B11

1. **Which `/` positions does the provider accept?** Claude: only the first word of the prompt (after the board's trim). A `/word` anywhere else is plain text: a built-in command never runs; a skill name can make the model load the skill, by its own choice (§1.4). Codex: no `/` position; the app-server sends `/word` to the model (§1.3.3). Codex runs a skill named with `$name` anywhere (§1.3.4).
2. **Where does the list come from before the first turn, and for Codex?** Claude: the `initialize` answer at process start (141 commands, no turn, no cost), then `system/commands_changed`, then each root `init` (§1.2.3). Codex: `skills/list` with the session folder after the thread opens, and again after `skills/changed`. Codex has no API for its TUI commands or custom prompts, and the app-server does not run them, so the board does not offer them.
3. **Does `@` open a menu on the bare `@`?** It opens today with one hint row and no file (§1.1.7). The design adds the files that this session changed (the session diff that the board loads already), filtered by a 1-letter query; the station search still starts at 2 letters.

## 3. Design

### 3.1 Wire: no new shape

`session.slash_commands { commands: Vec<String>, skills: Vec<String> }` (`crates/fleetdeck-proto/src/event.rs:299-305`) and `SessionSummary.slash_commands` / `.skills` (`session.rs:133-141`) stay as they are. No proto change, no feature flag, no migration. What changes is when the stations send the event and what Codex puts in it:

- Claude: `commands` = the command names; `skills` = the skill names (a subset of `commands` on 2.1.308).
- Codex: `commands` = `[]`; `skills` = the enabled skill names.

Both lists are sorted (byte order of the names), without duplicates, at most `MAX_COMMANDS = 500` names, each name 1 to `MAX_COMMAND_NAME = 128` bytes, with no whitespace and no leading `/` (a name that breaks a rule is dropped). Worst case about 64 KiB per event; the recorded Claude event is about 3 KiB.

**Emission rule (both providers):** the core keeps the last emitted pair. It emits a new event only when the new sorted pair differs from the last emitted pair. Before the first emission there is no pair, so the first list is emitted also when it is empty (an empty list now means "the provider has no commands"). A source that does not carry the list (a missing key, a value that is not an array) changes nothing.

### 3.2 Claude core (`crates/fleetdeck-providers/src/claude/commands.rs`, new)

```rust
//! B11: the command list of one Claude CLI process (`session.slash_commands`).

/// The most names in one list.
pub const MAX_COMMANDS: usize = 500;
/// The longest name, in bytes.
pub const MAX_COMMAND_NAME: usize = 128;

/// What the core knows about the command list of its process.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct CommandState {
    /// `terminal_slash_commands` of the last root `init`, bare names. Empty before it.
    pub terminal: Vec<String>,
    /// `skills` of the last root `init`, normalized. Empty before it.
    pub skills: Vec<String>,
    /// The last emitted `(commands, skills)`; `None` before the first event.
    pub emitted: Option<(Vec<String>, Vec<String>)>,
}

/// Bare, valid, not terminal-only, sorted, unique, at most MAX_COMMANDS.
pub(super) fn normalize(names: impl IntoIterator<Item = String>, terminal: &[String]) -> Vec<String>;

/// `commands` of an `initialize` answer or a `system/commands_changed` frame:
/// the `name` of each object. Not an array: no output.
pub(super) fn from_objects(core: &mut ClaudeCore, commands: Option<&Value>) -> Vec<Output>;

/// A root `init`: stores `terminal_slash_commands` and `skills`, then emits
/// `slash_commands` (minus terminal) with the skills. Neither `slash_commands`
/// nor `skills` is an array: stores what it can, no output.
pub(super) fn from_init(core: &mut ClaudeCore, body: &Map<String, Value>) -> Vec<Output>;

/// Emits `Event::SlashCommands` when the pair differs from `state.emitted`.
fn emit_if_changed(core: &mut ClaudeCore, commands: Vec<String>, skills: Vec<String>) -> Vec<Output>;
```

`normalize`: strip one leading `/`; drop an empty name, a name over 128 bytes and a name with a whitespace character; drop a name in `terminal`; sort; dedup; truncate to 500. Skills use `normalize(skills, &[])`.

Changes in the existing files:

1. `ClaudeCore` (`claude/core.rs:164`) gets `#[serde(default)] pub(super) commands: CommandState` (`new` sets `CommandState::default()`). `CHECKPOINT_VERSION` stays (an additive defaulted field, as `last_total_cost`).
2. `map.rs` `on_system`, `"init"`: for a root init (no `parent_tool_use_id`), call `commands::from_init`. The first init puts its output where `SlashCommands` is today (after `session.started`, the notice and `session.models`); a later root init returns `identity` plus the output of `from_init`. A child init changes nothing. Remove `slash_commands()` (`map.rs:226-238`); its logic moves to `commands.rs`.
3. `map.rs` `on_system`: new arm `"commands_changed" => commands::from_objects(core, body.get("commands"))`.
4. `map.rs` `on_control_response`, `HostRequest::Initialize`: keep the models logic; append `commands::from_objects(core, resp.response.get("commands"))`. A control answer is released at once (DL-141), so this event can come before `session.started`.
5. `core.rs` `on_frame` (`core.rs:498-508`): before the first init, an `Output::Emit(Event::SlashCommands { .. })` is released at once, like a write, not held in `early`. Reason: `commands_changed` comes before the first init in a session without a prompt (recorded).
6. `tests/claude_core_replay.rs:134-140`: add `Event::SlashCommands { .. }` to the events allowed before `session.started`, and the comment "(DL-141, DL-410)".

Recorded sequence for one process (2.1.308), with the expected events:

| Frame | Names | Event |
| --- | --- | --- |
| `initialize` answer | 141 (with the 4 terminal ones; no skills known) | `session.slash_commands` 141 / 0 skills, before `session.started` |
| `system/commands_changed` before the first prompt | 147 | 147 / 0, released at once |
| first `init` | 141 slash minus 4 terminal = 137, 106 skills | after `session.started`: 137 / 106 |
| `commands_changed` after the MCP servers connect | 147 minus 4 terminal = 143 | 143 / 106 |
| later `init` (147) and `commands_changed` | the same set | nothing |

(Recorded order in a process that got a prompt at once: `init` 141, `commands_changed` 147, then `init` 147 for each turn, `raw-turns.jsonl`.)

### 3.3 Codex core (`crates/fleetdeck-providers/src/codex/core_skills.rs`, extended)

1. `HostRequest` (`codex/core_types.rs:76`) gets `SkillCatalog` ("`skills/list` for the board's skill list after the thread opened (B11)").
2. `CodexCore` (`codex/core.rs:35`) gets `#[serde(default)] pub(super) emitted_skills: Option<Vec<String>>` (the last emitted list).
3. In `core_skills.rs`:

```rust
/// `skills/list` with the session folder, for the board (B11).
pub(super) fn skill_catalog(&mut self) -> Output; // host_id(HostRequest::SkillCatalog), params {"cwds": [cwd]}
/// The answer: the enabled skill names of every entry of `data`; emits
/// `SlashCommands { commands: vec![], skills }` when the list changed.
pub(super) fn on_skill_catalog(&mut self, r: &Value) -> Vec<Output>;
```

   `cwd` is `self.spec.cwd.clone().unwrap_or_else(|| self.config.cwd.clone())` (the same folder as `session.started`, `codex/map.rs:231-235`). A skill counts when `enabled == true` and `name` is a string; the names pass the same rules as §3.1 (the Rust normalize of Codex is a local copy of 15 lines in `core_skills.rs`; the two crates share nothing here).
4. `codex/map.rs` `on_thread` (`map.rs:257-259`): when `self.config.request_models`, push `self.skill_catalog()` after the `model/list` request.
5. `codex/map.rs` `on_response`: `(HostRequest::SkillCatalog, Ok(r)) => self.on_skill_catalog(&r)` and `(HostRequest::SkillCatalog, Err(_)) => Vec::new()` (before the generic error arm: a failed list is no session error).
6. `skills/changed`: move from `IGNORED_NOTIFICATIONS` (`map.rs:55`) to `HANDLED_NOTIFICATIONS`; the arm sends `self.skill_catalog()` when `request_models` is true and the thread is open (`thread_id` is `Some`), else nothing.
7. `CoreConfig.request_models` doc comment (`codex/mod.rs:264-266`): "Send the catalog requests after the thread opens: `model/list` (DL-151) and `skills/list` (B11). Replays of recordings that hold neither set it to false."
8. The testkit sim (`crates/fleetdeck-testkit/src/sim_codex.rs:224`) answers `skills/list` with `{"data": [{"cwd": <cwds[0] or "">, "skills": [{"name": "sim-skill", "description": "", "enabled": true, "scope": "user", "path": "/sim/SKILL.md"}], "errors": []}]}`.

### 3.4 Board: positions and menus (`web/src/session/slash.ts`, rewritten)

```ts
// B11 (DL-412, DL-413): the command menu of the composer. Replaces matchSlash (DL-140, DL-174).
import type { EventEnvelope, SessionSummary } from '../gen';
import type { MenuRow } from './Composer';

/** The most rows of the command menu; more give a status row. */
export const MAX_COMMAND_ROWS = 200;

/** Claude: `/name` in the first word. Codex: `$name` in any word, and `/` in the first word. */
export type CommandProvider = 'claude' | 'codex';

export type CommandList = {
  readonly provider: CommandProvider;
  /** Sorted unique names: commands and skills together. `null`: no `session.slash_commands` yet. */
  readonly names: readonly string[] | null;
  /** The names that are skills (the badge "skill"; Claude only). */
  readonly skills: ReadonlySet<string>;
};

/** The list of a session: the summary, else the newest timeline event (DL-174). */
export function commandList(
  s: Pick<SessionSummary, 'provider' | 'slash_commands' | 'skills'>,
  events: readonly EventEnvelope[],
): CommandList;

/** The command word at the caret: a pick replaces text[start, end). */
export type CommandMatch = {
  readonly trigger: '/' | '$';
  readonly start: number;
  readonly end: number;
  /** The text between the trigger and the caret. */
  readonly query: string;
};

export function matchCommand(text: string, cursor: number, provider: CommandProvider): CommandMatch | null;

/** Names for a query: prefix, then a part prefix, then contains; each group sorted. */
export function rankNames(names: readonly string[], query: string): string[];

/** The rows and the status row of a match; `null`: no menu. */
export function commandMenu(
  list: CommandList,
  match: CommandMatch,
): { readonly label: 'Commands' | 'Skills'; readonly rows: MenuRow[]; readonly status: string | null } | null;

/** The text and the caret after a pick of `name`. */
export function pickCommand(
  text: string,
  match: CommandMatch,
  name: string,
  provider: CommandProvider,
): { readonly text: string; readonly caret: number };
```

Rules:

1. `commandList`: `provider` is `'codex'` when `s.provider === 'codex'`, else `'claude'` (the station sets the provider when it creates the session, `crates/fleetdeck-station/src/store/log.rs:36`, so it is known before `session.started`). `names` = sorted unique union of `s.slash_commands` and `s.skills`; when `s.slash_commands` is absent, the newest `session.slash_commands` event of `events` (`slashCommands` of `timeline.ts:137`, commands only); when both are absent, `null`. `skills` = `new Set(s.skills ?? [])`.
2. `matchCommand`, `/`: `s` = index of the first character that is not whitespace (`/\s/`). Match when `text[s] === '/'` and `s < cursor <= end`, where `end` = index of the first whitespace after `s` (else `text.length`). `query = text.slice(s + 1, cursor)`. The same for both providers. A `/` in any other word: no match (DL-412).
3. `matchCommand`, `$` (Codex only): `w` = start of the word that ends at the caret (scan back while not whitespace). Match when `text[w] === '$'` and `cursor > w`; `end` = first whitespace at or after `cursor`, else `text.length`; `query = text.slice(w + 1, cursor)`.
4. `rankNames` (case-insensitive, `q = query.toLowerCase()`): group 1 = names that start with `q`; group 2 = the rest whose part after a `:`, `-` or `_` starts with `q` (`/ab` finds `marketing-skills:ab-testing`); group 3 = the rest that contain `q`. Each group sorted by `a.toLowerCase().localeCompare(b.toLowerCase())`. Empty `q`: every name, sorted.
5. `commandMenu`:
   - Claude, `/`: label "Commands". Rows `{ key: name, text: '/' + name, badge: 'skill' if in skills }`.
   - Codex, `/` or `$`: label "Skills". Rows `{ key: name, text: '$' + name }` (no badge).
   - Rows: the first `MAX_COMMAND_ROWS` of `rankNames`.
   - Status (exact texts in §3.6). Codex `$` with `names === null`, with no names, or with no match: return `null` (a `$` word can be `$HOME` or `$5`).
6. `pickCommand`: `insert` = `'/' + name` (Claude) or `'$' + name` (Codex, also from a `/` match). `after = text.slice(match.end)`. Add one space unless `after` starts with whitespace. `caret = match.start + insert.length + 1`.

### 3.5 Board: the composer (`web/src/session/Composer.tsx`)

1. Props: remove `slashCommands` and `skills`; add `commands?: CommandList | null` and (task T4) `changesRef?: string`.
2. Replace `Composer.tsx:254-256` with: `const list = props.commands ?? null; const command = list !== null && !bangOn ? matchCommand(text, cursor, list.provider) : null; const menu = command !== null && list !== null ? commandMenu(list, command) : null;`. Everywhere `slash !== null` is read, read `menu !== null`. `hashMenu(text, cursor, comments, menu !== null)`.
3. Precedence stays: the command menu, then `#`, then `@`. Because the `/` menu now needs the caret in the first word, `@` works in the later words of a `/review` prompt.
4. Rows: `menu.rows`; the status row: `menu.status` (when not `null`), else the `@` status as today (`Composer.tsx:275-286`).
5. `choose`: for the command menu, `const picked = pickCommand(text, command, row.key, list.provider); caretTo.current = picked.caret; edit(picked.text, picked.caret);`.
6. `ComposerMenu`: a `ref` on the listbox; `useScrollEdges(ref)` (`web/src/ui/Menu.tsx:26`) for the fade marks; a `useLayoutEffect` on `current` that calls `document.getElementById(\`${props.id}-${props.current}\`)?.scrollIntoView?.({ block: 'nearest' })`, so the keys never select a hidden row.
7. Remove the note `Composer.tsx:527-531` (the status rows of §3.6 replace it).
8. Placeholder (`Composer.tsx:450-454`): Claude with at least one name: `Message ${target}… (/ for commands)` (as today); Codex with at least one name: `Message ${target}… ($ for skills)`; else `Message ${target}…`.
9. `aria-autocomplete="list"` when `list !== null || canSearch`.
10. `SessionView.tsx:637-638`: replace the two props with `commands={commandList(s, events)}` and (T4) `changesRef={changesRef}`; remove `slashCommands` from the import at `SessionView.tsx:65`. The file stays at 797 lines (limit 800).

`web/src/styles/base.css:1749-1766`: add `.fd-composer-menu` to the three fade selectors (`.fd-menu-body[data-scrolled]::before, .fd-composer-menu[data-scrolled]::before`, and the same for `::after` and `[data-more-below]`), and `display: block` to the shared rule (the composer menu is a block box; the menu body pseudo elements are flex items and stay the same). This is a new place of an existing primitive, not a new token.

### 3.6 Exact UI texts

| Case | Menu label | Status row (last row, `aria-disabled`) |
| --- | --- | --- |
| Claude `/`, `names === null` | Commands | No command list from the agent yet. The text goes to the agent as a prompt. |
| Claude `/`, `names` empty | Commands | This session has no commands. The text goes to the agent as a prompt. |
| Claude `/`, no match | Commands | No matching command. The text goes to the agent as a prompt. |
| Codex `/`, `names === null` | Skills | No skill list from Codex yet. The text goes to Codex as a prompt. |
| Codex `/`, `names` empty | Skills | Codex reports no skills for this folder. The text goes to Codex as a prompt. |
| Codex `/`, no match | Skills | No matching skill. The text goes to Codex as a prompt. |
| Codex `/`, with rows | Skills | Codex runs a skill that the prompt names with $. Codex terminal commands such as /compact do not run here. |
| Any, more than 200 matches | as above | `${countText(n)} more. Type more letters to filter.` |
| Codex `$`, no list, no names or no match | — | no menu |
| `@` bare or 1 letter | Files | Type 2 or more letters to search files (unchanged) |

Badge: "skill" (unchanged). The keys are unchanged: ↑ ↓ move and wrap, ↵ and Tab pick, Esc closes; a menu with only a status row captures no key.

### 3.7 Board: `@` on the bare `@` (task T4)

1. `Composer` calls `useDiffPaths(props.changesRef ?? '')` (`web/src/session/workspace.ts`, the changed paths of the loaded session diff and of the loaded turn diffs; `useSessionChanges` loads them when the session opens, `PropertiesPanel.tsx:389`).
2. New pure function in `web/src/session/at.ts`:

```ts
/** B11: the changed files for a query under MIN_AT_QUERY letters: paths that contain it (case-insensitive), in their order, at most MAX_AT_OPTIONS. */
export function recentPaths(paths: readonly string[], query: string): string[];
```

3. When `at !== null && atQuery === null`, the choices are `recentPaths(paths ?? [], at.query)` as rows `{ key: path, text: path }`, then the status row "Type 2 or more letters to search files". A pick inserts `@<path> ` (the existing `at` branch of `choose`, `Composer.tsx:385-390`).
4. No changed path: the menu is as today (only the status row; the e2e test `menus.spec.ts:134-151` stays green).

### 3.8 Errors and edge cases

- The `initialize` answer of an older CLI without `commands`: no event; the list comes with the first `init` (as today).
- A Codex app-server without `skills/list` or an error answer: no event, no session error; the board shows "No skill list from Codex yet." on `/`.
- Shim takeover: the restored core keeps `CommandState` / `emitted_skills` (serde default), so it sends no duplicate. It gets no second `initialize` answer (`claude/checkpoint.rs:8`); the next `init` or `commands_changed` updates the list.
- A child `init` (subagent) does not change the list.
- `/clear` and other built-in commands with side effects stay in the list, as today (out of scope, §3.10).
- A `/path/to/file` as the first word shows "No matching command. …" while the caret is in that word; after a space the menu closes. The text goes to the agent (recorded: the model answers).
- A pick with images or shell chips: the CLI still runs the command (the last text block counts, recorded).
- An unknown Claude command costs a model turn (recorded); the status row says that the text goes to the agent as a prompt.

### 3.9 Compatibility

| Deck/board | Station | Result |
| --- | --- | --- |
| new board | old station, Claude | the list comes at the first `init` (as today); before it, "No command list from the agent yet." |
| new board | old station, Codex | no event; `/` shows "No skill list from Codex yet."; `$` has no menu |
| old board | new station, Claude | the list comes earlier and sorted; the old board shows 8 rows (no harm) |
| old board | new station, Codex | `commands: []`: the old `hasCommands` is false, so no menu (the old board would insert `/name`, which Codex does not run, so this is correct) |

No feature flag: the event shape is unchanged (the same choice as DL-174 for `skills`).

### 3.10 Out of scope

- Descriptions and argument hints in the menu (they are in `initialize` and `commands_changed`, but not in the event; a description list in `SessionSummary` would go in every board snapshot and delta). Owner question 2.
- Claude aliases (`/review` → `code-review`): the contains rule already finds `code-review` for `rev`.
- Codex TUI commands (`/compact`, `/review`, `/model`): the app-server has methods for some of them (`thread/compact/start`, `review/start`); a mapping is a new command path (high risk). Owner question 3.
- Codex custom prompts (`~/.codex/prompts`): the app-server has no list and does not expand them.
- Menus in the New session prompt (`web/src/spawn/SpawnForm.tsx`, a plain textarea). Owner question 4.
- Filtering built-in commands that do not fit a board session (`/clear`, `/color`).
- Sending Codex skills as `{"type": "skill", name, path}` input items (the text mention works, §1.3.4).

## 4. Risk class

**Normal.** No authentication or authorization change, no stored data repair, no migration, no new route, no new event kind or field. It changes when an existing event is sent (Claude: before `session.started`, with the DL-141 precedent) and adds one read-only provider request (Codex `skills/list`). The names come from the provider of the session and reach only the people who already see its summary (the same as today). Review: one `codex-run review` of the final branch.

## 5. Build plan

T1 and T2 can run in parallel with T3 (no shared file). T4 follows T3. All heavy commands go through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>` (below: `HEAVY <command>`).

### T1 — Claude: the command list from `initialize`, `commands_changed` and every root `init` (backend, about 350 lines)

- **Owned files:** `crates/fleetdeck-providers/src/claude/commands.rs` (new), `crates/fleetdeck-providers/src/claude/tests_commands.rs` (new), `crates/fleetdeck-providers/src/claude/mod.rs`, `crates/fleetdeck-providers/src/claude/map.rs`, `crates/fleetdeck-providers/src/claude/core.rs`, `crates/fleetdeck-providers/src/claude/tests.rs`, `crates/fleetdeck-providers/src/claude/tests_models.rs`, `crates/fleetdeck-providers/tests/claude_core_replay.rs`, `crates/fleetdeck-providers/tests/claude_driver_replay.rs`, `crates/fleetdeck-providers/tests/claude_driver.rs`, `crates/fleetdeck-providers/tests/capability_matrix.rs`, `crates/fleetdeck-providers/tests/capability_matrix/replay.rs`, `web/src/testing/fixtures/claude-subagent.events.json`, `web/src/testing/fixtures/claude-tool_call_approve.events.json`, `web/src/testing/fixtures/claude-ask_user_question.events.json`.
- **Changes:** §3.2 items 1-6. `mod.rs`: `mod commands;` and `#[cfg(test)] mod tests_commands;`. Regenerate the three board fixtures with `FD_UPDATE_FIXTURES=1` (the recorded `initialize` answers now yield the event). Change the replay or driver tests only where the new event changes a count or an order; say each change in the report.
- **Tests (`tests_commands.rs`):**
  - `an_initialize_answer_with_commands_emits_the_sorted_names_before_the_session_start`: initialize answer `commands: [{name:"review"},{name:"/compact"},{name:"a b"},{name:""}]` before any init → one `SlashCommands { commands: ["compact","review"], skills: [] }` and no `SessionStarted`.
  - `an_initialize_answer_without_commands_emits_no_list`: `commands` missing, and `commands: "x"` → no `SlashCommands`.
  - `commands_changed_before_the_first_init_is_released_at_once`: through `ClaudeCore::on_frame`, a `system/commands_changed` frame before `init` → the event is in the output (not held).
  - `the_first_init_drops_the_terminal_commands_and_adds_the_skills`: after an initialize list with `doctor`, an init with `terminal_slash_commands: ["doctor"]` and `skills` → a second event without `doctor`, with the skills; order `SessionStarted` first.
  - `the_same_list_again_emits_nothing`: a second init and a `commands_changed` with the same set in another order → no event.
  - `a_child_init_does_not_change_the_list`: an init with `parent_tool_use_id` and other names → no event.
  - `an_empty_first_list_is_emitted_once`: init with `slash_commands: []`, `skills: []` → one event with two empty lists; again → nothing.
  - `names_over_the_limits_are_dropped`: a 129-byte name dropped; 600 names → 500.
  - `the_command_state_survives_a_checkpoint`: serialize the core after an emit, restore, feed the same list → no event; a checkpoint JSON without `commands` deserializes.
  - Update `tests.rs` `the_first_init_emits_the_slash_commands_without_the_terminal_only_ones`: skills become sorted `["deploy","review"]`.
- **depends_on:** none.
- **Acceptance:**
  - `FD_UPDATE_FIXTURES=1 HEAVY cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'binary(claude_core_replay)'` once, then without the variable.
  - `HEAVY cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/claude/) | binary(claude_core_replay) | binary(claude_driver_replay) | binary(claude_driver) | binary(capability_matrix)'`
  - `cd web && HEAVY bun run test timeline` (the fixture readers: `src/session/timeline.test.ts` and the tests that use `src/testing/timeline.ts`).
  - `HEAVY cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T2 — Codex: the skill list (backend, about 300 lines)

- **Owned files:** `crates/fleetdeck-providers/src/codex/core_skills.rs`, `crates/fleetdeck-providers/src/codex/core_types.rs`, `crates/fleetdeck-providers/src/codex/core.rs`, `crates/fleetdeck-providers/src/codex/map.rs`, `crates/fleetdeck-providers/src/codex/mod.rs`, `crates/fleetdeck-providers/src/codex/tests_skills.rs` (new), `crates/fleetdeck-testkit/src/sim_codex.rs`, `crates/fleetdeck-providers/tests/codex_driver_sim.rs`.
- **Changes:** §3.3 items 1-8. Keep `codex/map.rs` and `codex/core.rs` under 800 lines (put the logic in `core_skills.rs`).
- **Tests (`tests_skills.rs`, a core with `request_models: true`):**
  - `the_thread_open_sends_skills_list_with_the_session_folder`: after the `thread/start` answer, a `skills/list` write with `{"cwds": [<cwd>]}` after the `model/list` write.
  - `a_skills_answer_emits_the_enabled_sorted_names_as_skills`: two entries, a disabled skill, a duplicate, a name with a space → `SlashCommands { commands: [], skills: [sorted enabled names] }`.
  - `an_empty_skills_answer_emits_an_empty_list_once`.
  - `the_same_skills_again_emit_nothing`.
  - `a_failed_skills_list_emits_no_event_and_no_error`.
  - `skills_changed_sends_skills_list_again_after_the_open` and `skills_changed_before_the_open_sends_nothing`.
  - `without_request_models_no_skills_list_is_sent`.
  - `codex_driver_sim.rs`: `a_sim_session_reports_its_skills`: the event arrives after `session.started` with `skills: ["sim-skill"]`.
  - `tests/codex_schema.rs` (not owned, read-only) must still pass with `skills/changed` in the handled list.
- **depends_on:** none.
- **Acceptance:**
  - `HEAVY cargo nextest run -p fleetdeck-providers --no-fail-fast -E 'test(/codex/) | binary(codex_core_replay) | binary(codex_driver_replay) | binary(codex_driver_sim) | binary(codex_schema) | binary(capability_matrix)'`
  - `HEAVY cargo nextest run -p fleetdeck-testkit --no-fail-fast`
  - `HEAVY cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T3 — Board: command positions, the Codex `$` menu, ranking and scroll (ux, about 550 lines)

- **Owned files:** `web/src/session/slash.ts`, `web/src/session/Composer.tsx`, `web/src/session/SessionView.tsx`, `web/src/session/composerMenus.test.ts` (new), `web/src/session/controls.test.tsx`, `web/src/session/session.test.tsx`, `web/src/styles/base.css`, `web/e2e/menus.spec.ts`.
- **Changes:** §3.4, §3.5, §3.6. Remove `matchSlash` and `MAX_SLASH_OPTIONS`; move their tests in `controls.test.tsx` to the new functions (`controls.test.tsx` is 703 lines: put new tests in `composerMenus.test.ts`). Follow `docs/v2/DESIGN-SYSTEM.md` (skill `fleet-design-system`).
- **Tests (`composerMenus.test.ts`):**
  - `matchCommand` Claude: `'/'`@1 → query `''`; `'  /rev'`@6 → start 2; `'/review fix'`@4 → query `'rev'`, end 7; `'fix /review'`@11 → `null`; `'a\n/x'`@4 → `null`; `'/review fix'`@11 → `null` (caret not in the first word).
  - `matchCommand` Codex: `'use $sk'`@7 → `$`, start 4; `'$'`@1 → query `''`; `'/'`@1 → `/`; `'mail me$x'`@9 → `null`.
  - `rankNames`: `['marketing-skills:ab-testing','abc','xab','compact']` with `'ab'` → `['abc','marketing-skills:ab-testing','xab']`; empty query → all sorted.
  - `commandMenu`: Claude 300 names → 200 rows and the status "100 more. Type more letters to filter."; the skill badge; every status text of §3.6; Codex `$` with no match → `null`; Codex `/` rows are `$name` with the Codex status row.
  - `pickCommand`: `'/rev fix'` with a match in the first word and `review` → `'/review fix'`, caret 8; Codex `'/sk'` → `'$skill-a '`; `'x $s'` → `'x $skill-a '`.
  - `commandList`: provider `codex` → `'codex'`; union and sort of commands and skills; summary without a list and no event → `names: null`.
  - Static render (`controls.test.tsx` or the new file): a Claude composer with text `/` and 20 names renders 20 options; a Codex composer with `$` renders the listbox "Skills".
  - `session.test.tsx`: the `(/ for commands)` and `($ for skills)` placeholders from a summary.
  - `web/e2e/menus.spec.ts`: the `/` menu opens on `  /co` and on `/co fix` with the caret after `co`; ArrowDown past row 8 scrolls the selected row into view (the option is in the viewport); a pick in `/co fix` gives `/compact fix`.
- **depends_on:** none (the event shape is unchanged).
- **Acceptance:**
  - `cd web && HEAVY bun run test session` and `cd web && bun run typecheck && bun run lint`
  - `sh tools/design-check.sh`
  - `cd web && HEAVY bun run build`, then `FD_E2E_PORT=<free port> HEAVY ./node_modules/.bin/playwright test e2e/menus.spec.ts`
  - Gallery shots of the session composer with the open menu, dark and light, desktop and phone (skill §3.3).
  - `HEAVY cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T4 — Board: changed files on the bare `@` (ux, about 150 lines)

- **Owned files:** `web/src/session/at.ts`, `web/src/session/Composer.tsx`, `web/src/session/SessionView.tsx`, `web/src/session/composerMenus.test.ts`, `web/e2e/menus.spec.ts`.
- **Changes:** §3.7. `SessionView.tsx` gets only `changesRef={changesRef}` (one line; the file must stay under 800 lines).
- **Tests:** `recentPaths(['src/a.ts','docs/b.md'], '')` → both; `'a'` → `['src/a.ts']` (case-insensitive, `'A'` too); 30 paths → 20. Static render: `@` with two changed paths shows two options and then the status row. e2e: with a fake session diff of `src/main.rs`, `@` shows `src/main.rs`; ↵ gives `@src/main.rs `.
- **depends_on:** T3.
- **Acceptance:** the same commands as T3.

## 6. Decisions made in this draft

1. **Claude list from three sources** (`initialize` answer, `system/commands_changed`, every root `init`), not only the first `init`. Rejected: a cached list from the last session of the same provider instance (a guess: the folder changes the project skills and commands).
2. **The event can come before `session.started`** (the `initialize` answer and a held `commands_changed` are released at once), with DL-141 as the precedent. Rejected: hold it as `pending_models` (DL-151) — then a session without a prompt has no list until its first turn, which is the reported fault.
3. **Sorted, deduplicated, bounded lists, emitted only on change**; the first list is emitted also when empty. Rejected: the frame order (it put 100 user skills before `/compact`), and "no event for an empty list" (the board could not say "no commands").
4. **Terminal-only filter from the last `init` only**; before it, `doctor`, `color`, `focus` and `reload-plugins` can show. Rejected: a fixed list in the code (it drifts; `/color` runs in `-p` mode anyway).
5. **Codex: `skills/list` after the thread opens and after `skills/changed`, gated by `request_models`.** Rejected: a new `CoreConfig` field (about 12 struct literals in 4 crates change for no gain); reusing the handshake `skills/list` answer (it is sent only with the fleet skill and without the session folder).
6. **Codex in the existing event: `commands: []`, `skills: names`.** Rejected: a new event kind or a `syntax` field (a proto change and a flag; the board knows the provider from the summary). An old board then shows no menu for Codex, which is correct.
7. **Claude `/` menu only in the first word**, also after leading whitespace (the board trims). Rejected: a menu at any word (a built-in command never runs there; a skill runs only if the model chooses to; and every path `/home/...` in a prompt would open a menu). Owner question 1.
8. **Codex `$` menu at any word start, and the `/` menu in the first word shows the Codex skills and inserts `$name`.** Rejected: no `/` menu for Codex (the owner types `/` and gets no hint of the `$` syntax).
9. **No menu for a Codex `$` word without a match**; a `/` first word always shows a status row. Reason: `$HOME`, `$5` are common text; a leading `/` is a clear intent.
10. **Ranking: prefix, part prefix (after `:`, `-`, `_`), contains; each group sorted.** Rejected: fuzzy matching (hard to predict, more code).
11. **Up to 200 rows in the existing 240px scroll box**, the selected row scrolled into view, the menu fade of `ui/Menu.tsx`. Rejected: keep 8 rows plus a "more" row (the owner asked to see all commands).
12. **The note under the box is replaced by status rows in the menu.** Rejected: keep both (two places for one fact).
13. **Names only, no descriptions, in this track.** Rejected: descriptions in `SessionSummary` (every board snapshot and delta would carry about 40 KiB per Claude session). Owner question 2.
14. **The bare `@` shows the files that this session changed.** Rejected: `files.list` of the root (one more station read per `@`, and its top-level folders cannot be searched further by the name search); a lower `MIN_AT_QUERY` (the station refuses 1 letter, and the walk would be slow).
15. **`CHECKPOINT_VERSION` stays**: the new core fields are additive with `serde(default)`, as `last_total_cost`.
16. **`MenuRow` stays in `Composer.tsx`**; `slash.ts` imports it as a type (as `review/hashMenu.ts` does).

## 7. Owner questions

1. **A `/` menu in the middle of the text?** Claude runs a command only at the start; a skill named in the middle loads only when the model chooses to. Recommended default: no menu in the middle (only the first word).
2. **Descriptions in the `/` menu?** They need a small station read for the rich list (not the summary). Recommended default: names only now; a later item if you want descriptions.
3. **Codex terminal commands (`/compact`, `/review`) through the app-server methods?** That is a new command path with its own review. Recommended default: not now; the Codex `/` menu says that they do not run here.
4. **The same menus in the New session prompt?** Recommended default: not in this track.

## Decision log entries (to append)

## DL-410 — Claude command list from `initialize`, `commands_changed` and every root `init` (B11)

- Context: owner report 2026-10-22 (B11): the `/` menu misses commands. The Claude core sent `session.slash_commands` only from the first `init`, which the CLI sends only with the first prompt; the `initialize` answer (141 commands, recorded on 2.1.308) and `system/commands_changed` (147) were ignored.
- Choice: `claude/commands.rs` emits `session.slash_commands` from the `commands` of the `initialize` answer, from each `system/commands_changed`, and from each root `init` (`slash_commands` minus the `terminal_slash_commands` of the last root init; `skills` of the last root init). Names: one leading `/` removed, 1-128 bytes, no whitespace, sorted, unique, at most 500. The core emits only when the sorted pair changed; the first list also when empty; a source without an array changes nothing. The event can come before `session.started` (as the control answers of DL-141); the replay invariant allows it. `ClaudeCore.commands: CommandState` is `serde(default)`; `CHECKPOINT_VERSION` stays.
- Rejected: holding the event as `pending_models` (DL-151); a cached list from another session.

## DL-411 — Codex skills in `session.slash_commands` (B11)

- Context: Codex sessions had no `/` menu. The app-server 0.163.0 has `skills/list` and `skills/changed`, but no list of its TUI commands or custom prompts, and it sends `/compact` to the model as text (recorded). It loads a skill that the text names with `$name` anywhere (recorded: +1,367 input tokens for the 5,472-byte skill file).
- Choice: after the thread opens, and after each `skills/changed`, the Codex core sends `skills/list {cwds: [<session folder>]}` (`HostRequest::SkillCatalog`) when `CoreConfig.request_models` is true, and emits `session.slash_commands { commands: [], skills: <enabled names> }` with the rules of DL-410. A failed list emits nothing and no error. The testkit sim answers with one skill.
- Rejected: a new event kind or field; a new `CoreConfig` field; offering the Codex TUI commands.

## DL-412 — Composer: where the command menus open (B11)

- Context: Claude runs a slash command only when the prompt starts with it (recorded); the board trims the text before it sends it.
- Choice: `/` opens the command menu while the caret is in the first word of the text (leading whitespace allowed), for Claude and Codex. A `/` in another word opens nothing. In a Codex session `$` opens the skill menu at any word start, with no menu when nothing matches; a pick inserts `$name ` (also from the `/` menu). A Claude pick inserts `/name ` and keeps the rest of the text. The command menu wins over `#` and `@` only while the caret is in its word.
- Rejected: a `/` menu at every word start.

## DL-413 — Composer: the command menu lists every command (B11)

- Context: the menu showed at most 8 rows in the frame order, so built-in commands never showed for `/`.
- Choice: `matchSlash` and `MAX_SLASH_OPTIONS` are replaced by `matchCommand`, `rankNames`, `commandMenu` and `pickCommand` (`web/src/session/slash.ts`). Ranking: prefix, part prefix after `:`, `-` or `_`, contains; each group sorted case-insensitively. Up to 200 rows in the 240px scroll box, the selected row scrolled into view, the menu fade (`.fd-composer-menu` in the `base.css` fade rules). The list is commands and skills together. Status rows say when there is no list yet, when the provider has none, and when nothing matches (texts in `composer-menus.md` §3.6); they replace the note under the box. Placeholder `($ for skills)` for Codex.
- Rejected: 8 rows plus a "more" row; fuzzy matching; descriptions in the summary.

## DL-414 — Composer: the bare `@` shows the changed files of the session (B11)

- Context: the bare `@` showed only "Type 2 or more letters to search files"; the station search needs 2 letters.
- Choice: under 2 letters the `@` menu lists the changed paths of the session that the board already loaded (`useDiffPaths`), filtered by the 1-letter query (case-insensitive), at most 20, then the same status row. No change of the station.
- Rejected: a `files.list` of the root; a 1-letter station search.
