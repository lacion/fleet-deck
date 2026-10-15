# Subagent work and repository branches in a session (DL-191)

- **Status:** Accepted, revision 3 (2026-10-15): DESIGN REVIEW round 3 **PASS** (no blocker or major left). Revision 1: FAIL (3 blockers, 7 majors; §5). Revision 2: FAIL (2 not fixed, 1 new blocker, 2 new majors; §6).
- **Owner request:** backlog B1, "lets get this done": a session that gives work to subagents in other worktrees must show that work. The first part of B1 (an import's base is the `HEAD` at import) is built.
- **Goal:** (A) a subagent card in the chat shows the subagent's own messages and tool calls; (B) the Changes tab lists the branches of the session's repository with their committed work, so the owner sees work done outside the session folder (by subagents in other worktrees, or merged later).

## 1. Facts

- Claude keeps one transcript per subagent: `<claude home>/projects/<encoded cwd>/<native session id>/subagents/agent-<agent id>.jsonl` (0600). Its records have the history shapes with `isSidechain: true`, `sessionId` = the parent native id, `agentId` = the agent id.
- The tool result of an `Agent`/`Task` call in the parent stream carries the structured `tool_use_result` (transcript key `toolUseResult`) with `agentId` (observed: `{"isAsync": true, "status": "async_launched", "agentId": "afef5265450389d2c", …}`), for background and foreground agents.
- The station's Claude mapping (`fleetdeck-providers/src/claude/map.rs`) emits `tool.started` with `provider_refs.tool_use_id`; the board's subagent item drops `provider_refs`.
- History (DL-189): `history/locate.rs` (`resolve_home`, `encode_cwd`, `descend` without following links, `check_file`: owner, one hard link, not private, size), `history/lines.rs` (`Lines` over a captured extent), `history/parse.rs` (record parser; it drops every `isSidechain` record), the budget (one permit per station, 10 s).
- Git 2.56 on the owner's machine supports `for-each-ref --format=%(ahead-behind:HEAD)`. The station's `Git` runner (`workspace/git.rs`) disables hooks, fsmonitor and the generic external diff; `session_diff.rs` passes `--no-ext-diff --no-textconv`; `worktrees.rs` passes `--no-show-signature`; only `Git::bounded` caps stdout.

## 2. Design

### 2.1 The agent id comes from the station's own stream (A)

- **Frame parser** (`fleetdeck-providers/src/claude/frames.rs`): a user frame keeps its top-level structured tool result (`tool_use_result`, or `toolUseResult`) as an optional `serde_json::Value` on `MessageFrame` (capped: only the `agentId` string is read; the rest is dropped at parse time).
- **Mapping** (`claude/map.rs`): for the tool result that answers a known tool node whose tool name is `Agent` or `Task`, read `agentId` from that structured value only (never from result text); pattern `[a-z0-9]{1,64}`, else ignored.
- **Event schema** (`fleetdeck-proto/src/event.rs`): `ToolCompleted` gets `provider_refs: BTreeMap<String, Value>` with `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]` (an older reader ignores it; an older event reads as empty). The mapping puts `agent_id` there. The TS types are regenerated.
- **Binding row** (§2.2): the station records the binding when the event is ingested. `session.subagent` reads only that row; the request never names an agent id or a file.

### 2.2 The binding is recorded per subagent, with its launch home (A)

- New station migration V25: table `subagent_logs (session_id TEXT, tool_use_id TEXT, agent_id TEXT, native_id TEXT, cwd TEXT, provider_home TEXT, created_at INTEGER, PRIMARY KEY (session_id, tool_use_id))`. When the manager ingests a `tool.completed` with `provider_refs.agent_id` from a running process, it inserts one row with the launch facts of that process: the native session id, the session cwd and the absolute provider home that `resolve_home` gave for the launch environment of that process (made absolute against the process environment's `HOME`; a relative or missing home → no row). `INSERT OR IGNORE`: the first binding of a tool use stays.
- A later resume in another home writes new rows only for its own new tool uses; older cards keep their own home. No session-level home.
- A card without a row (a tool use from before this build, or no agent id) → `unavailable { not_found }`.

### 2.3 Reading one subagent log (A)

- **Link method** `session.subagent` (read, scope `read`, feature `subagent.v1`). Params `SubagentQuery { session_id, tool_use_id }` (`tool_use_id`: 1–128 of `[A-Za-z0-9_-]`). Answer `SubagentLog { items: Vec<HistoryItem>, state: HistoryState }`.
- **Path:** from the §2.2 row: `<provider_home>/projects/<encode_cwd(cwd)>/<native_id>/subagents/agent-<agent_id>.jsonl`, opened segment by segment with `descend` (no link followed). Then only the metadata checks of `check_file` (owner, one hard link, not private, ≤ 64 MiB), split from its transcript scan; the identity check is the per-record check below. Claude sessions only; a Codex session → `unavailable { unsupported_format }`.
- **One read, one extent:** the file is read once from the retained descriptor up to the size captured at open (`Lines` with that end); bytes appended later are not read. A last line without a terminating newline is not a record (the reader checks the newline explicitly; `Lines` alone returns it). When the read ends before the captured end (the file shrank), the answer is `partial`. No index and no second read.
- **Subagent parsing mode:** `parse_line_subagent(line, native, agent_id)` accepts records with `isSidechain: true` whose `sessionId` equals `native` and `agentId` equals `agent_id`; every other rule of the history parser applies (meta, thinking, wrappers dropped; tool calls and results paired). A record with another `sessionId` or `agentId` stops the read with `unavailable { not_trusted }`.
- **Bounded answer:** keep the newest items in a ring of at most 300; build the answer, serialize it, and drop the oldest items until the serialized answer is ≤ 512 KiB (each text cut as in history); `partial { skipped }` counts dropped records. The request runs under the history budget (one permit, 10 s).

### 2.4 Branches of the repository (B)

- **Link method** `session.branches` (read, scope `read`, feature `session_branches.v1`). Params `{ session_id }`. Answer `RepoBranches { head: Option<String>, branch: Option<String>, branches: Vec<RepoBranch>, truncated: bool }`, `RepoBranch { name, head, ahead: u32, behind: u32, at: i64, subject }`.
- **Git version:** the station reads `git version` once per process; below 2.45 (no `GIT_NO_LAZY_FETCH` guarantee, no `ahead-behind`) the method answers the error `git_too_old` and the board says "Update Git on this station (2.45 or newer) to see the branches." No fallback command.
- **Commands** in the session's `cwd`, all through the hardened runner of §2.5, under one 10 s deadline:
  1. `git rev-parse --verify -q HEAD` → `head` (full hex oid), or `None` for an unborn `HEAD`.
  2. `git symbolic-ref -q --short HEAD` → `branch`, or `None` when detached.
  3. `git for-each-ref --count=201 --sort=-committerdate --format=%(refname)%00%(objectname)%00%(committerdate:unix)%00%(contents:subject)%00%(ahead-behind:HEAD) refs/heads` (stdout cap 256 KiB). With an unborn `HEAD` the `ahead-behind` atom cannot run: skip this command and answer an empty list.
- **Grammar** of command 3 (no `-z` exists): each record is `refname NUL oid NUL unix NUL subject NUL ahead SP behind LF`. It is safe to split on LF: Git forbids control characters in ref names, `%(contents:subject)` is one line, and the other fields are digits and hex. The parser is strict: exactly five fields; `refname` starts with `refs/heads/`, at most 256 bytes; `oid` full hex (40 or 64); `unix`, `ahead`, `behind` decimal; the subject cut to 200 characters; any malformed record ends the parse and sets `truncated`. 201 records → keep 200 and set `truncated`.
- The list excludes the current branch by name (`branch`), never by tip. A detached `HEAD` excludes nothing.
- No `git worktree list` (it reads worktree metadata files and prints their contents).

### 2.5 The diff of one branch (B)

- **Link method** `files.branch_diff` (read, feature `session_branches.v1`). Params `{ session_id, head }` (`head`: full hex oid). The station runs the §2.4 commands again (same grammar) and accepts `head` only if it equals the `oid` of a listed branch other than the current one; else `invalid_params`.
- Commands (`cwd` only): `git merge-base HEAD <head>` (bounded), then `git diff --no-ext-diff --no-textconv --no-color --submodule=short --ignore-submodules=dirty --no-renames <merge-base> <head>` through `Git::bounded` (the existing diff byte limit).
- **Hardened runner:** a new `Git` method `bounded_with_env(args, envs, max_bytes)` (or an `envs` option on `GitOptions`) sets, for every Git call of §2.4–2.5: `GIT_NO_LAZY_FETCH=1`, `GIT_TERMINAL_PROMPT=0`, `GIT_PAGER=cat`, plus `-c core.pager=cat -c diff.external= -c core.fsmonitor=false` before the subcommand, `--no-show-signature` where a command takes it, and the runner's existing protections. Git below 2.45 is refused (§2.4), so the lazy-fetch guard always holds. An outer `tokio::time::timeout` gives one 10 s deadline for the whole request (all commands), and a per-session mutex allows one branches or branch-diff request per session at a time; a timed-out child is killed by the runner's existing cleanup.
- **Answer budget:** the answer is a `SessionDiff`-shaped value (`scope = committed`); serialize it and cut the patch at a character boundary until the serialized answer is ≤ 512 KiB (`truncated = true`).

### 2.6 Deck and board

- **Deck routes** (owner only: a member or a token gets 403 `owner_only`; feature precheck → 409 `subagent_unsupported` / `branches_unsupported` without a forward; fixed texts; the history error mapping):
  - `GET /api/sessions/{station}/{session}/subagents/{tool_use_id}` → `SubagentLog`.
  - `GET /api/sessions/{station}/{session}/branches` → `RepoBranches`.
  - `GET /api/sessions/{station}/{session}/branches/{head}/diff` → the diff.
- **Board, subagent card:** the timeline keeps `tool_use_id` on a `subagent` item. For the owner the card gets "Show its work" (then "Refresh"); the items render with the history shapes inside the card; `partial` / `unavailable` lines as in history.
- **Board, Changes tab:** a "Branches" section under "Since the session started": one row per branch other than the session's own (name, `n ahead`, `merged` when `ahead == 0`, subject, age), newest first; a click shows its committed diff with the existing diff view. Owner only.

## 3. Security notes

- Owner only for both parts.
- Subagent: no directory scan, no meta file, no request-chosen name: the agent id comes from the station's own event log; the path uses the stored provider home; every segment is opened without following links; the file passes the history trust checks; every record must name the parent session and the agent; one bounded read of a captured extent.
- Branches: one `for-each-ref` with NUL-separated output and a strict parser; no `worktree list`; no command in another folder; `diff` without textconv, external diff, submodule recursion, lazy fetch or pager; `head` must be a current branch head; one deadline; serialized answers within 512 KiB.

## 4. Tests

- Mapping: the tool result with `tool_use_result.agentId` gives `provider_refs.agent_id` on `tool.completed`; a bad id is ignored.
- Subagent: the event lookup; no agent id → `not_found`; a link on the path or the file, a hard link, another owner, a private path → `not_trusted`; a record of another session or agent → `not_trusted`; appended bytes after open are not read; a partial last line is ignored; the 300-item ring and the 512 KiB serialized cap with escaping-heavy text; a Codex session → `unsupported_format`; after a resume in another home, old and new cards each read their own home (one binding row per tool use); a tool use without a binding row → `not_found`.
- Branches: a repository with a merged, an ahead and a behind branch; a branch name with a newline is impossible (Git refuses it) but a malformed record in a fake runner output stops the parse; a hostile `diff.textconv`, `diff.external` and `diff.submodule=diff` in the repository config change nothing; a `head` that is not a listed branch head → `invalid_params`; the serialized-size cut with an escaping-heavy patch; Git below 2.45 → the error `git_too_old`; shared tips, a detached and an unborn `HEAD`, and more than 200 branches (`truncated`).
- Deck: owner only, token and member 403, feature precheck 409, error mapping. Board: "Show its work" for the owner only; the Branches rows and the diff view.

## 5. Review map (revision 1 → revision 2)

| Finding | Answer |
| --- | --- |
| BLOCKER Git can run repository-controlled programs | §2.5 the flag and environment list for every Git call |
| BLOCKER `worktree list` reads symlinked metadata | §2.4 no `worktree list`; `for-each-ref` prints names from paths and only parsed oids |
| BLOCKER diff opens submodules | §2.5 `--submodule=short --ignore-submodules=dirty` |
| MAJOR wire budget | §2.3, §2.5 serialized-answer caps; §2.4 field caps |
| MAJOR parser drops sidechain records | §2.3 subagent parsing mode |
| MAJOR meta identity | §2.1 agent id from the station's own stream; §2.3 every record checked |
| MAJOR live-file consistency | §2.3 one read of a captured extent, no index |
| MAJOR provider home provenance | §2.2 stored per session |
| MAJOR unbounded Git work | §2.4 one bounded command; §2.5 one deadline, one request per session |
| MAJOR newline-forged records | §2.4 `-z` / `%00` strict parser; §2.5 head re-check against a fresh listing |

## 6. Review map (revision 2 → revision 3)

| Finding | Answer |
| --- | --- |
| BLOCKER `for-each-ref -z` does not exist | §2.4 the NUL-field, LF-record grammar and why it is safe |
| Round-1 #1 not fixed: `bounded` has no environment; old Git ignores `GIT_NO_LAZY_FETCH` | §2.5 `bounded_with_env`; §2.4 Git below 2.45 refused, no fallback |
| MAJOR agent id dropped by the frame parser; `ToolCompleted` has no refs | §2.1 frame parser keeps the structured result; `ToolCompleted.provider_refs` (backward compatible) |
| MAJOR a resume in another home loses older cards | §2.2 one binding row per tool use with the launch home; no session-level home |
| MAJOR HEAD and the current branch not established | §2.4 commands 1 and 2; unborn and detached cases; 201-record truncation; exclusion by name |
| Notes: split `check_file`; newline completion; absolute home; outer deadline | §2.2, §2.3, §2.5 |
