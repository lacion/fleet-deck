# Full session history in the chat (DL-189)

- **Status:** Accepted for build, revision 5 (2026-10-15). The extra review of revision 4 confirmed the round-3 fixes and found one blocker (read integrity, §9); the owner chose "apply the fix and build": revision 5 answers it with a station-owned snapshot, and the adversary PRE-PR review of the code checks it. Earlier, revision 4: round 3 got FAIL (§8). Revision 3: revision 2 got DESIGN REVIEW round 2 **PASS** for the owner-only scope (open majors M6, M7, M9, M13 answered here, §6); the owner's per-share `history` option (§3.8) is new in revision 3. Needs a round-3 PASS before code. Revision 1 got DESIGN REVIEW FAIL (4 blockers, 9 major; §5); §6 maps them. Needs a PASS before code.
- **Owner request (2026-10-15):** an imported session shows an empty chat: "i think we should work on propperly importing a session and showing all in the chat".
- **Goal:** the chat of a session shows its whole conversation: the part from the terminal before an import, and every older turn that the deck no longer keeps. Nothing is copied at import; the history is read on demand.

## 1. Facts today

- `session.import` (DL-308) resumes the native conversation (`claude --resume <id>`, Codex `thread/resume`). The agent has the whole context, but the timeline starts at the import: an imported session's timeline holds only `session.named`, `session.base`, `session.started`, `session.models`, `session.slash_commands`, `usage.updated`.
- The deck keeps at most `TIMELINE_CAP = 500` timeline rows per session (`crates/fleetdeck-deck/src/store/read_model.rs:31`, DL-049). A long live session also loses its older chat on the board. The station event log keeps acknowledged events 7 days.
- Both providers persist the conversation of the runs that Fleet Deck drives: Claude by default (no `--no-session-persistence`), Codex because the station sends `ephemeral: false` (`crates/fleetdeck-providers/src/codex/core.rs:308`).
- Transcript files (provider-owned, not a stable public format):
  - Claude: `<config home>/projects/<encoded cwd>/<session id>.jsonl`; `<config home>` is the instance `config_home` (`CLAUDE_CONFIG_DIR`) or `~/.claude`. One JSON object per line. Conversation records: `type` `user` / `assistant` with `message.role` and `message.content` (a string, or blocks `text`, `thinking`, `tool_use`, `tool_result`, `image`), `timestamp`, `uuid`, `isSidechain`, `isMeta`. Many other record types (`attachment`, `mode`, `ai-title`, `system`, `file-history-*`, `queue-operation`, `pr-link`, …).
  - Codex: `<config home>/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl` (`CODEX_HOME` or `~/.codex`). Records: `response_item` with `payload.type` `message` (`role` `user` / `assistant` / `developer`), `reasoning`, `function_call` / `custom_tool_call` and their `*_output`; `event_msg`; `session_meta`; `turn_context`; token records.

## 2. Scope of revision 2

- **Pre-import history only.** The history of a session is the part of its native conversation that happened **before the session's first execution** on this station: for an imported session, the terminal part. Everything from the first execution on is the session's own timeline (unchanged). The station resolves this boundary itself (§3.3); the board never de-duplicates.
- **Owner only by default; sharing is the owner's decision (owner, 2026-10-15: "it should be my decission to share an imported session").** The owner's board login always reads history. A member reads it only through a share on which the owner turned on the share option `history` (§3.8); then the member sees prompts and answers, and tool lines with the tool name only. A bearer token (any scope) gets `403 owner_only`.
- **Not in this revision:** older live turns that the deck no longer keeps (`TIMELINE_CAP`). The board shows a gap line there (§3.7); a later step can page the station event log or raise the cap.

## 3. Design

### 3.1 Link method `session.history` (station)

- Read method `session.history`, scope `read`, not mutating, link feature `history.v1`. Params `SessionHistoryQuery { session_id, before: Option<String>, limit: Option<u32> }`, `limit` 1–100, default 50.
- Answer `SessionHistory { items: Vec<HistoryItem>, before: Option<String>, state: HistoryState }`:
  - `items`: oldest first, all older than `before` (or the newest pre-import items when `before` is `None`).
  - `before`: the cursor of the next older page; `None` at the start of the conversation.
  - `state`: `complete` (no record skipped), `partial { skipped }` (records of a known file that the parser skipped: unknown shapes, bad JSON, oversize lines), `empty` (the session has no pre-import part: it was not imported, or the import had no earlier turns), `unavailable { reason }`: `no_native_id`, `not_found`, `ambiguous`, `not_trusted` (owner, link, hard link or private-path checks failed), `too_large`, `unsupported_format` (a non-empty file in which no record had a known shape), `busy`.
- `HistoryItem { id: String, at: i64, kind: user | assistant | tool, text: String, tool: Option<HistoryTool> }`; `id` is the provider's record id (Claude `uuid`, Codex the item id, else `line:<n>`). `HistoryTool { name, summary, status: ok | failed | unknown }`.
- **Wire budget:** the station adds items newest-to-oldest until the next item would make the serialized answer exceed **512 KiB** (the control frame is 1 MiB, `link.rs:16`, so escaping and the envelope stay under it). Each `text` is cut to 8 KiB, `name` to 64 characters, `summary` to 200 characters, `id` to 128 bytes, at character boundaries. At least one item is always returned when one exists.

### 3.2 Finding and trusting the transcript

- Input: only the station's own session row: provider, provider instance, cwd, stored native id (`check_native_session_id` must pass). The caller gives no path and no native id.
- **Effective home** (major 6): at the first `session.started` with a native id, the station resolves the provider home from the **same environment that launched the process** (`ChildEnv::for_instance` with the instance's `config_home` and `env`; then `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, else `HOME` + `.claude` / `.codex`) and stores it with the located transcript in a new table `session_transcripts (session_id PRIMARY KEY, home TEXT, path TEXT, dev INTEGER, ino INTEGER, located_at INTEGER)` (station schema V24; forward-only, no row rewritten). A session without a row (sessions made before V24, such as the owner's two imports) has **no trusted provenance**: the station tries, in order, the home of the current instance environment and the default home (`~/.claude` / `~/.codex`), and accepts a candidate only when the file there passes every trust check of the next bullet **and** its embedded session id is this native id. More than one passing candidate: `ambiguous`. The accepted candidate is stored with `provenance = rediscovered` (a later instance change cannot redirect it).
- **Exact location, no newest-by-mtime** (blocker 2):
  - Claude: exactly `<home>/projects/<encoded cwd>/<native id>.jsonl`, where `<encoded cwd>` replaces every character that is not ASCII alphanumeric with `-` (the rule of Claude Code; `claude --resume` itself looks there, so a resumed session's file is at that path). No glob.
  - Codex: `<home>/sessions/*/*/*/rollout-*-<native id>.jsonl` and `<home>/archived_sessions/rollout-*-<native id>.jsonl`; exactly one match, else `ambiguous` (zero: `not_found`).
- **Trust checks** on the opened descriptor (directories walked with the `O_NOFOLLOW` descriptor walk of `workspace/files.rs`, file opened `O_NOFOLLOW`): a regular file; owner = the station user; link count 1 (the hard-link rule of `PrivatePaths::is_private_opened`); not a private path (`PrivatePaths::is_private_file`); the first record that names a session must name this native id (Claude `sessionId`, Codex `session_meta.payload.id`), else `not_trusted`. A stored `(dev, ino)` that no longer matches the file at the path: the checks run again and the row is updated; a failure is `not_trusted`.

### 3.3 The boundary: a captured prefix

- **Capture at import.** The import effect, **before** it starts the resume process, locates and trusts the transcript (§3.2), and records in `session_transcripts` the **prefix**: `prefix_size` (the file size at that moment, cut back to the end of the last complete line) and `prefix_sha256` (SHA-256 of bytes `[0, prefix_size)`), with `capture = import`. The capture runs under the §3.5 budget; when it fails (no file, not trusted, too large, deadline) the import still runs, and the row records `capture = failed` with the reason.
- **Snapshot.** In the same capture step, the station copies bytes `[0, prefix_size)` of the transcript into its own file `<station data>/history/<session id>.jsonl` (directory 0700, file 0600, staged + fsync + rename + directory fsync), and checks that the copy's SHA-256 equals `prefix_sha256` (a mismatch: the provider wrote during the copy; the capture retries once, then `capture = failed`). From then on **every history read uses only the snapshot**; the provider's file is never read again for this session. The provider cannot change the snapshot, so a page can never mix revisions or show later text.
- **History = exactly the snapshot.** Items are the conversation records inside the snapshot. No time is used to decide what is history. The snapshot is deleted with the session's other station data (a purge); its size counts against the 256 MiB file limit (§3.5).
- **Not imported** (`session.start`): no capture, `state: empty`.
- **Imported before V24** (the owner's two sessions): on the owner's first read, the station captures lazily: `prefix_size` = the end of the last conversation record whose time is before the session's first process start (`processes.started_at`), and stores `capture = lazy`. A `lazy` capture makes a snapshot the same way; it may use inferred times, so it is **owner-only forever**: a member read of a `lazy` (or `failed`) session gets `unavailable { reason: not_captured }`.

### 3.4 Parsing

- Claude: records `type` `user` / `assistant` with `isSidechain != true` and `isMeta != true`. User text: a string content or `text` blocks; a text made only of command or hook wrappers (`<command-name>`, `<command-message>`, `<command-args>`, `<local-command-stdout>`, `<local-command-caveat>`, `<system-reminder>`) is skipped. Assistant text: `text` blocks (never `thinking`). A `tool_use` block gives a `tool` item; its `tool_result` (same `tool_use_id`) sets `status` (`is_error: true` → `failed`, else `ok`). An `image` block becomes `[image]`.
- Codex: `response_item` records: `message` with role `user` (`input_text`) or `assistant` (`output_text`); never `developer`, `reasoning`. `function_call` / `custom_tool_call` give a `tool` item; the matching `*_output` sets `status` from an exit code when present, else `unknown`.
- Tool `summary`: the command of a shell tool, the path of an edit/read/write, the pattern of a search, else empty. Owner only (§2), so details are shown; tool **output** is never returned.
- **Oversize lines** (major 8): a new helper `skip_line_bounded` consumes the rest of a `TooLong` line up to its newline (the bytes count against the read budget); the record counts as skipped; parsing restarts at the next line, never inside a record.
- **Fixtures** (unverified finding): sanitized, version-labelled samples (Claude Code 2.1.300, codex-cli 0.162.1) in `crates/fleetdeck-station/tests/fixtures/transcripts/`, made from real local transcripts by a script that keeps every record's structure and replaces the text content with placeholders; each fixture has its expected item list (incl. side chains, meta records, wrappers, a resumed session, a rewound or branched conversation if the provider writes one, and records without a time); a new provider version that changes the shapes shows `partial` or `unsupported_format`, never a silent empty history.

### 3.5 Index, cache and budgets (major 7, 9)

- The station builds an **index** of a transcript in one scan: per kept item its byte offset, length and `at` (no text). Pages read only their records again with positioned reads. Limits: file at most 256 MiB (`too_large`), at most 200,000 indexed items (older items beyond the cap: `partial`), one scan per request at most 10 s (`busy` on the deadline, nothing cached).
- **Immutable history, simple cursor:** the history is the snapshot (§3.3), which never changes. The index key is the session id and `prefix_sha256`; an evicted index or a restart rebuilds the same list from the same snapshot. The cursor is `"<first 16 hex of prefix_sha256>:<item index>"`; a cursor whose hash part does not equal the session's captured hash is `stale_cursor`.
- **Cache:** at most 4 indexes per station (LRU), at most 32 MiB of index memory in all; single-flight per key (a second request for the same key waits for the first scan).
- **One budget for the whole request:** one worker permit per station (a semaphore of 1) covers discovery, trust checks, indexing and the page reads of a request, with one deadline of 10 s. Discovery (the Codex glob) visits at most 5,000 directory entries (`busy` beyond). The scan and the page reads use a **captured extent**: the file size at open; bytes written after it wait for the next request. A page reads at most 1 MiB of record bytes. The blocking work runs on the blocking pool; on the deadline the request answers `busy`, but the permit is released only when the worker actually returns (as `manager/files.rs` slots), so timed-out work can never pile up.

### 3.6 Deck route

- `GET /api/sessions/{station}/{session}/history?before=&limit=`: the owner login, or a member through `access::session_of` (share coverage and role, `MemberAction::Read`) **and** the member's **own** share (`grant.share_id`) with `history = true` (§3.8); a member without such a share gets `403 history_not_shared`; a bearer token gets `403 owner_only`. The route is in the member set. Audited on failure and on every member read (actor, share id, session). The deck checks first that the station's link negotiated `history.v1` (the stored link features); without it: `409 history_unsupported` "This station runs an older Fleet Deck. Update it to see the earlier history." No forward then (major 12).
- Mapping of station errors: `unknown_session` → 404; `invalid_params`, `stale_cursor` → 400 / 409 with the code; `busy` → 503; a timeout → 504; no link → 409 `station_unreachable`; any other → 502 `station_error`. `unavailable`, `empty` and `partial` are a 200 answer with the state.
- The deck sends `member: true` for a member read. The station then empties every tool `summary` (the tool `name` and `status` stay) **before** the answer leaves the station, on every response, after any cache (§3.5 caches only indexes, never answers).
- The deck stores nothing and caches nothing.

### 3.7 Board

- Chat tab of an owner login: on the first open of the tab, the board asks for the first page once. (The station answers `empty` quickly for a session that was not imported: the first transcript record is already at or after the boundary.) With `items`, a block "Earlier history (from the terminal)" sits above the timeline, oldest at the top, with "Load earlier messages" at its top while `before` is not `None`; scrolling to the top loads the next page; the scroll position is kept. `partial` adds one muted line "Some lines of the transcript could not be read." `unavailable` shows one line with the reason in plain words. `empty` shows nothing.
- When the deck no longer holds the oldest timeline rows of the session (the timeline starts after `session.started`), a muted line "Older messages of this session are not kept on the deck." sits between the history block and the timeline.
- Items render like chat messages in a muted style; `tool` items are single compact lines (icon by name, summary, status). No item is editable or answerable.

### 3.8 Sharing the history (owner decision)

- New share field `history: bool` (deck `shares.history INTEGER NOT NULL DEFAULT 0`, deck schema migration; every existing share reads `false`). `NewShare.history: Option<bool>` (absent = `false`); `ShareInfo.history: bool`.
- Only the owner sets it: in the create-share form, and later with the owner-only, audited route `POST /api/shares/{share_id}/history { "enabled": bool }` (CSRF; turning it on is a danger change that needs the owner's elevation, as the other disclosure settings). Turning it off takes effect at the next request (the deck checks the share on every read).
- Which share counts: **only the share of the member's own login** (`ShareGrant.share_id` of the authenticated member session). That share must be live (not revoked, not expired), cover the session now (session, space or board scope), and have `history = true`. Another share that covers the same session never counts, whatever its setting. The session's capture must be `import` (§3.3).
- Board, share form and share row: a checkbox "Members can read the earlier history of imported sessions" with the text "Members see the full earlier conversation of the shared sessions, including anything that was pasted into it. Tool commands and file names stay hidden." The share row shows "History shared" when on, with a toggle.
- The session boundary of §3.3 holds for members too: a member never reads a conversation part from after the session's first execution, so a conversation that continued in another (unshared) session is never shown.

## 4. Tests

- Parser fixtures: kept and skipped shapes of both providers (meta, side chain, thinking, wrappers, developer, reasoning), tool status, cuts, an oversize line skipped without a mid-record parse, `unsupported_format`.
- Location and trust: the instance home and an `env` override of the home; a session made before V24 whose instance home changed after the import (rediscovery finds the file in the default home, or `ambiguous` when both pass); a symbolic link (file and directory) refused; a hard link refused; another owner refused; a private-path file refused; Codex zero/one/two matches; a first record naming another session → `not_trusted`.
- Boundary: an import captures the prefix before the resume starts; history is exactly the records inside it; records appended later, with or without times (incl. records without a time that a later private session writes), are never returned; after the snapshot is made, an in-place rewrite of the provider's file (also one between two page reads, with a cached index) changes nothing in the returned pages; a write during the copy → one retry, then `capture = failed`; the snapshot file mode and location; a started session → `empty`; a failed capture → owner and member `unavailable`; a `lazy` capture → owner page, member `not_captured`.
- Paging and cursors: pages cover the pre-import items once in order; an append keeps the cursor valid; a rewrite before the old end that keeps the last 4 KiB and then appends → `stale_cursor` (never a skipped or repeated item); after a cache eviction and after a station restart, an old cursor either continues exactly or is `stale_cursor`; the wire budget (a page of items with 8 KiB texts full of characters that JSON escapes stays under 1 MiB serialized); a scan over the deadline → `busy` and the permit stays taken until the worker ends (a second request waits); single-flight; a Codex home with more than 5,000 entries → `busy`; bytes appended during a scan are not read; a page never reads more than 1 MiB.
- Deck: two shares cover one session, A with `history = false` (member Alice) and B with `history = true` (member Bob): Alice gets 403 `history_not_shared`, Bob gets the page; owner gets the page; a token gets 403 `owner_only`; a member without a `history` share gets 403 `history_not_shared`; a member with one gets the page with empty tool summaries (checked on the wire answer); a revoked or expired share, or `history` turned off, stops the next read; only the owner can set `history` (member, token → 403), turning it on needs elevation; a station without `history.v1` → 409 without a forward; error mapping; the member-read audit row.
- Board: the block, the pager, the gap line, the `partial`/`unavailable` lines, no block for `empty`.

## 5. Review round 1 (2026-10-15, Codex adversary): FAIL — to answer in revision 2

Blockers:
1. **A native conversation is not a deck-session boundary.** End shared session A, import its conversation as private session B, keep chatting: A's members would read B's conversation. Bind authorized transcript ranges to deck sessions and executions; freeze A's upper boundary when custody moves; enforce it on the station.
2. **File name, ownership and newest mtime do not prove provenance.** Duplicates, divergent copies or hard links can return another conversation. Record and verify the transcript identity at launch/resume, check the embedded session metadata, refuse ambiguity, use descriptor checks and the existing hard-link exclusion (`PrivatePaths::is_private_opened`).
3. **The raw transcript shows members more than the live timeline** (the timeline keeps only a first-line summary of a prompt, `claude/core.rs:625`; pasted secrets and quoted private files would pass). Default imported/raw history to owner access unless the owner grants full-transcript access to a share; do not claim equivalence with the timeline.
4. **Deny-list matching cannot make commands and search patterns safe** (a secret in a `curl` header or a search pattern names no path). For members return tool categories/names only; show details only with explicit transcript authorization; apply private-path and member rules after the cache, on every response.

Major:
5. A page must fit the 1 MiB control frame (`link.rs:16`): budget the whole serialized answer and stop early with a cursor.
6. The effective provider home comes from the launch environment (`ChildEnv::for_instance` then `instance.env` can override `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `HOME`); persist non-secret transcript provenance per session.
7. Resource bounds: aggregate cache bytes and items, scan/read budgets, deadlines, bounded workers, single-flight per key, a captured bounded extent, append handling.
8. `read_line_bounded` returns `TooLong` without consuming the line: define incomplete/unavailable or drain to the newline under the budget; never parse mid-record.
9. Cursors must name a parse generation and a stable record boundary; a rewritten file gives an explicit stale-cursor result.
10. Timestamps cannot de-duplicate transcript and timeline (station `at` vs provider time): keep native message/input/tool ids and correlate.
11. The overlap can span many pages: start history at a server-resolved overlap boundary, or reconcile every page and skip empty pages with bounded work.
12. Older peers: `unknown_method` is checked before feature negotiation (`methods.rs:628`), and the file route maps unknown errors to 502; specify feature prechecks, HTTP mappings and a board capability signal.
13. Tests must cover execution reuse, ambiguous transcripts, hard links, home overrides, owner/member cache order, inline secrets, rewrites, multi-page overlap, wire size.

Unverified: the persisted transcript formats are provider-internal; add version-labelled sanitized fixtures and distinguish empty, partial, corrupt and unsupported history.

Architect note for revision 2: a simpler first step that avoids blockers 1, 3 and 4 is **owner-only history** (members and tokens get none), bound to the session's own executions, with native-id correlation; member access can follow as a separate, owner-granted share option.

## 6. Review map (revision 1 → revision 2)

| Finding | Answer |
| --- | --- |
| B1 native conversation is not a session boundary | §2 owner only; §3.3 the boundary is the session's own first execution, so a reused conversation never extends a session's history |
| B2 provenance | §3.2 exact Claude path, one Codex match or `ambiguous`, descriptor checks, link count 1, private-path check, embedded session id |
| B3 raw transcript exposure | §2 owner only by default; member access only by the owner's per-share `history` option with an explicit warning (§3.8) |
| B4 commands and patterns | members never get tool summaries (emptied on the station, every response); tool output never returned |
| M5 frame size | §3.1 512 KiB answer budget and field cuts |
| M6 effective home | §3.2 resolved from the launch environment, stored in `session_transcripts` |
| M7 resource bounds | §3.5 file, index, memory, deadline, semaphore, single-flight |
| M8 oversize line | §3.4 `skip_line_bounded` |
| M9 cursor generation | §3.5 generation, append vs rewrite, `stale_cursor` |
| M10, M11 de-duplication and overlap | §2, §3.3 a server boundary; the board never de-duplicates |
| M12 older peers | §3.6 feature precheck and the error mapping |
| M13 tests | §4 |
| Unverified formats | §3.4 fixtures, `partial`, `unsupported_format` |

## 7. Review round 2 (2026-10-15): PASS — map of the open items (revision 2 → revision 3)

| Open item | Answer |
| --- | --- |
| M6 legacy provenance from current config | §3.2 rediscovery: only a candidate that passes every trust check and the embedded id; `ambiguous` when two pass; stored as `rediscovered` |
| M7 discovery and page reads outside the budget | §3.5 one permit and one deadline over discovery, checks, scan and reads; 5,000 entries; captured extent; 1 MiB per page; permit kept until the worker returns |
| M9 tail hash does not prove continuity | §3.5 cursor by item identity, checked after every rebuild; no append shortcut |
| M13 tests | §4 added cases |
| U formats | §3.4 fixtures from real transcripts with expected items (open until they exist) |

## 8. Review round 3 (2026-10-15): FAIL — owner decision: fix + one extra review (revision 4)

| Finding | Answer in revision 4 |
| --- | --- |
| BLOCKER: a member borrows another share's `history` permission | §3.8, §3.6: only the share of the member's own login (`grant.share_id`) counts; test with two shares and opposite settings |
| BLOCKER: inferred times let later private records into the history (B1 reopened) | §3.3: history is the byte prefix captured at import, hash-checked; no time decides; `lazy` captures (legacy) are owner-only forever |
| MAJOR M9: one cursor item does not prove continuity | §3.5: the history is immutable (verified prefix); the cursor carries the prefix hash |
| UNVERIFIED U: fixtures | still open until the build supplies them (§3.4) |

## 9. Extra review of revision 4 (2026-10-15): FAIL (1 blocker) — owner decision: apply the fix and build

| Finding | Answer in revision 5 |
| --- | --- |
| Round-3 blockers (share borrowing, inferred times) and M9 | confirmed fixed |
| BLOCKER: page bytes reread from the provider's file are not bound to the verified prefix (in-place rewrite, check/read race) | §3.3 a station-owned snapshot copied and hash-checked at capture; every read uses only the snapshot |
| UNVERIFIED U: fixtures | supplied by the build (§3.4); the PRE-PR review checks them |
