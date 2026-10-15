# Full session history in the chat (DL-189)

- **Status:** Design, revision 1 (2026-10-15). Needs an adversary DESIGN REVIEW PASS before code.
- **Owner request (2026-10-15):** an imported session shows an empty chat: "i think we should work on propperly importing a session and showing all in the chat".
- **Goal:** the chat of a session shows its whole conversation: the part from the terminal before an import, and every older turn that the deck no longer keeps. Nothing is copied at import; the history is read on demand.

## 1. Facts today

- `session.import` (DL-308) resumes the native conversation (`claude --resume <id>`, Codex `thread/resume`). The agent has the whole context, but the timeline starts at the import: an imported session's timeline holds only `session.named`, `session.base`, `session.started`, `session.models`, `session.slash_commands`, `usage.updated`.
- The deck keeps at most `TIMELINE_CAP = 500` timeline rows per session (`crates/fleetdeck-deck/src/store/read_model.rs:31`, DL-049). A long live session also loses its older chat on the board. The station event log keeps acknowledged events 7 days.
- Both providers persist the conversation of the runs that Fleet Deck drives: Claude by default (no `--no-session-persistence`), Codex because the station sends `ephemeral: false` (`crates/fleetdeck-providers/src/codex/core.rs:308`).
- Transcript files (provider-owned, not a stable public format):
  - Claude: `<config home>/projects/<encoded cwd>/<session id>.jsonl`; `<config home>` is the instance `config_home` (`CLAUDE_CONFIG_DIR`) or `~/.claude`. One JSON object per line. Conversation records: `type` `user` / `assistant` with `message.role` and `message.content` (a string, or blocks `text`, `thinking`, `tool_use`, `tool_result`, `image`), `timestamp`, `uuid`, `isSidechain`, `isMeta`. Many other record types (`attachment`, `mode`, `ai-title`, `system`, `file-history-*`, `queue-operation`, `pr-link`, …).
  - Codex: `<config home>/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl` (`CODEX_HOME` or `~/.codex`). Records: `response_item` with `payload.type` `message` (`role` `user` / `assistant` / `developer`), `reasoning`, `function_call` / `custom_tool_call` and their `*_output`; `event_msg`; `session_meta`; `turn_context`; token records.

## 2. Design

### 2.1 Link method `session.history` (station)

- New read method `session.history`, scope `read`, not mutating, behind a new link feature `history.v1` (a peer without it never gets the method: `feature_not_negotiated`). Params `SessionHistoryQuery { session_id, before: Option<String>, limit: Option<u32> }`; `limit` 1–200, default 100.
- Answer `SessionHistory { items: Vec<HistoryItem>, before: Option<String>, complete: bool, source: HistorySource }`:
  - `items` oldest first, at most `limit`, all strictly older than the `before` cursor (or the newest ones when `before` is `None`).
  - `before`: the cursor for the next older page; `None` when `complete`.
  - `source`: `transcript` or `unavailable { reason }` (`no_native_id`, `not_found`, `too_large`, `unreadable`, `unsupported_provider`).
- `HistoryItem { at: i64 (Unix ms), kind: user | assistant | tool, text: String, tool: Option<HistoryTool> }`; `HistoryTool { name: String, summary: String, status: ok | failed | unknown }`.
- Cursor: an opaque text `"<file id>:<item index>"`; `<file id>` is a hash of the transcript's device, inode and the native id, so a cursor of another file is refused (`invalid_params`). The item index counts conversation items from the start of the file.

### 2.2 Finding the transcript

- The station uses only its own session row: provider, provider instance (its `config_home`), and the stored native session id (`check_native_session_id` must pass). No path comes from the caller.
- Claude: look for `<config home>/projects/*/<native id>.jsonl` (one directory level, file name equal to `<native id>.jsonl`). Codex: `<config home>/sessions/*/*/*/rollout-*-<native id>.jsonl`, then `archived_sessions/`. More than one match: the newest by modification time.
- Open without following a link (`O_NOFOLLOW` on the file, the existing descriptor walk of `workspace/files.rs` for the directories); the file must be a regular file owned by the station user. The private-path set is checked (a file of the private set is refused).

### 2.3 Parsing (tolerant)

- Stream the file line by line with the bounded reader of NV-20 (`read_line_bounded`, 16 MiB per line). A line that is not JSON or not a known shape is skipped. A file over 256 MiB: `unavailable { too_large }`.
- Claude: keep `user` and `assistant` records with `isSidechain != true` and `isMeta != true`. User text: string content, or `text` blocks; a text that is only a command or hook wrapper (`<command-name>`, `<local-command-stdout>`, `<system-reminder>` …) is skipped. Assistant text: `text` blocks (never `thinking`). `tool_use` gives a `tool` item (`name`, `summary` from §2.4); its `tool_result` (matched by `tool_use_id`) sets `status` (`is_error` → `failed`). `image` blocks become the text `[image]`.
- Codex: `response_item` `message` with role `user` (its `input_text`) and `assistant` (its `output_text`); never `developer`, `reasoning`, `event_msg`. `function_call` / `custom_tool_call` give a `tool` item; the matching `*_output` sets `status` from an exit code or failure marker when present, else `unknown`.
- Each text is cut to 16 KiB at a character boundary with the marker `… [cut]`. Tool output is never returned.
- A parse result is cached per session by (device, inode, size, mtime), at most 8 entries per station, so paging does not re-read the file.

### 2.4 Tool summaries

- `summary` is at most 200 characters: the command of a shell tool, the file path of an edit, read or write, the pattern of a search, else the tool name. For a member principal, a summary that names a path that the member deny list hides is replaced by the tool name (the deny list of the station policy, the same rule as the file browser).

### 2.5 Deck route and access

- `GET /api/sessions/{station}/{session}/history?before=&limit=` (scope `read`, member-capable). Members go through `access::session_of` with `MemberAction::Read` (share coverage and role), as the timeline does. The deck forwards to the station, adds no cache, and never stores the items. Errors: `station_unreachable`, `timeout`, and the station's codes.
- The deck sends `member: true` for a member principal, so the station applies §2.4.

### 2.6 Board

- The Chat tab, when the loaded timeline does not reach the start of the conversation (an imported session, or a session whose timeline was capped): a "Load earlier messages" button at the top. A click loads one page and puts it above the timeline, with a divider "Earlier history (from the agent's transcript)". Scrolling to the top loads the next page; the scroll position stays.
- The first page uses `before = None` and drops items that are not older than the oldest timeline event (by `at`), so the live part is not shown twice.
- `tool` items render as compact single lines (icon, summary, status); user and assistant items render like chat messages, in a muted style. `unavailable` shows one line with the reason ("The agent's transcript is not on this station").

## 3. Security notes

- The history shows the same kind of text as the live timeline (prompts and answers) to the same principals (read scope, covered members). Tool outputs are never returned; tool summaries for members follow the deny list.
- The station reads only the transcript of its own session row; the caller gives no path and no native id.
- Transcripts are provider files; the parser is bounded (line, file, item, cache) and skips unknown shapes.

## 4. Tests (to be detailed in the worker briefs)

- Parser fixtures (anonymized samples of both formats): meta, side-chain, thinking, hook-wrapper and developer records are skipped; tool items get status; texts are cut; bad lines skipped.
- Locator: config home of the instance, a link in place of the file refused, a private-path file refused, no native id → `unavailable`.
- Paging: three pages cover the file once; a cursor of another file is refused; a changed file starts a new cache entry.
- Deck: member coverage, `history.v1` not negotiated → 409 with a clear text, deny-list summaries for members.
- Board: the button, the divider, the de-duplication against the timeline, `unavailable` text.
