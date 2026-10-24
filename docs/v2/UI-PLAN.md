# Board UI rebuild and TUI handover (DL-140)

*Written 2026-10-12 against `v2` @ `dd29735`. Status: APPROVED by the owner (build all of it now, on `v2`). This file is the single record of the design decisions. Worker briefs copy from it.*

## 1. The problem (before)

| Area | What the code did | Effect |
| --- | --- | --- |
| Fleet screen (`App.tsx`) | One long page: filters, assign bar, needs-you, mail and tasks, lanes, usage, conflicts, feed | No overview. |
| Session screen (`SessionView.tsx`) | A form: rename, controls, composer at the top, revert, turns, files, timeline at the bottom | It is not a conversation. |
| Messages | Plain `<p>`, no markdown | Agent output is hard to read. |
| Timeline | No auto-scroll; raw tool output | You search for the latest output. |
| Lifecycle | No Stop, no Resume | An ended session is a dead end. |
| Shell | Only in Stations, typed path, one terminal, lost on navigation | Hard to find and use. |
| Phone | Same page, narrow | Not usable as a chat client. |

## 2. Decisions

1. **The chat view is the primary way to drive an agent.** It renders the canonical events (D3 drive-only).
2. **TUI handover is a separate, explicit owner action.** The station stops the driven process, then runs the provider TUI (`claude --resume`, `codex resume`) in a station PTY that the board shows in the terminal dock. While the TUI holds the native session, the station refuses every resume, auto-continue, import and handover of it. "Return to board control" ends the TUI and resumes the driven session. Nothing resumes automatically.
3. **Three layouts:** desktop ≥ 1024 px (pinned sidebar), tablet 768–1023 px (sidebar as a drawer), phone < 768 px (stack navigation, chat first, like T3 Code mobile).
4. **Hash routes** so that browser back works on a phone: `#/` fleet home, `#/s/<station>/<session>` (optional `?tab=changes|files`), `#/new`, `#/plans`, `#/stations`.
5. **Markdown:** `react-markdown` + `remark-gfm`. No raw HTML (no `rehype-raw`). Links open in a new tab with `rel="noopener noreferrer"`. Images are not loaded; they render as links.
6. **One session controller** owns the session actions, the pending command and the last outcome. The header, the composer, the approval dock and the terminal dock use it. No component orchestrates stop/resume/handover by itself.
7. **Terminal dock:** shells and TUI tabs live in a board-level store, not in a screen. Navigation and a hidden dock do not close a shell. Three different actions: *Detach* (remove the tab, the PTY runs on), *Close shell* (end the PTY, with a confirm), *Return to board control* (TUI tabs only).
8. **Global keys** never fire inside an input, a textarea, a select, a contenteditable element, a terminal, or during IME composition. Esc only closes the top overlay.
9. **Slash commands:** the Claude `init` frame lists them. The event `session.slash_commands { commands }` carries them (minus the TUI-only `terminal_slash_commands`). It is not folded; the composer palette reads the newest one from the timeline. Without a list, the owner can still type a command. A slash command is sent as a normal prompt.

## 3. TUI handover (station-authoritative)

### 3.1 Protocol

- `ShellChange::Handover { shell_id, session_id, cols, rows }` → link method `shell.handover`.
- `ShellChange::Release { shell_id }` → link method `shell.release`: the owner clears a hold whose process the station cannot see (after a station restart, §3.4).
- Both need the link feature `shell.handover.v1`. A peer without it refuses with `feature_not_negotiated`; the board shows that text.
- `ShellInfo.session_id: Option<SessionId>`: set for a TUI shell.
- New event `session.terminal { shell_id: Option<String>, pid: Option<u32>, reason: Option<String> }`. `Some(shell_id)` = a TUI holds the session. `None` = the hold is released.
- `SessionSummary.terminal: Option<TerminalHold { shell_id, pid, since }>`, folded from `session.terminal` in `fleetdeck-proto/src/fold.rs`. This is the single source of truth for the board.
- New event `session.slash_commands { commands: Vec<String> }`, emitted after the first Claude `init`. It is not folded into the summary; the board reads the newest one from the timeline.
- All new fields are optional with serde defaults (L16: old stored events and summaries still decode).

### 3.2 Admission (one store transaction)

`shell.handover` is admitted only when every fact holds, else the ordinary rejection receipt:
`allow_shell`, `max_shells`, the session exists, it has ended (`exited` or `lost`), no live process, no unended execution, `capabilities.resume`, a native session id, the provider instance is configured, the cwd is inside the allowed roots, `summary.terminal` is `None`, and no other session with the same native id (same provider instance) has a hold.

In the same transaction the station appends `session.terminal { shell_id: Some, pid: None, reason: "handover requested" }`. **The hold starts at admission**, so a resume or an import that commits later sees it.

### 3.3 Exclusion

- `admit_resume` (resume and auto-continue) refuses when `summary.terminal` is `Some`: "a terminal holds this session (shell `<id>`); return it to board control first".
- `start` with an import refuses when any session with that native id has a hold.
- A second handover refuses (§3.2).

### 3.4 Release (only after the process is gone)

- PTY spawn fails → release with the reason.
- The TUI process exits (the PTY reader reaches EOF **and** `child.wait()` returns) → release with "the terminal process exited (code N)".
- `shell.close` of a TUI shell: SIGHUP to the process group, SIGKILL after 3 s, then the release happens through the exit path above. The close receipt does not release by itself.
- Station start: for each held session whose shell is not in this process (always, after a restart): if the stored `pid` does not run → release ("the station restarted; the terminal process is gone"). If it runs → keep the hold and append `session.terminal` again with the reason "the station restarted; process N may still use this session". The owner clears it with `shell.release` after a check.
- Station shutdown closes every TUI shell like `shell.close`.
- The `pid` is stored with a second `session.terminal` after spawn.

### 3.5 Program

- Claude: `<binary> --resume <native id> --permission-mode <mode>` (+ `--model <m>` when the stored model is set and not `default`). `<mode>` is the stored mode when it is a native Claude mode, else the safest mode (L8: never more permissive).
- Codex: `<binary> resume -a <policy> -s <sandbox> [-m <model>] <thread>`. `<p>/<s>` come from the stored mode via `codex::Mode::parse`, else `Mode::safest()`.
- Env: `ChildEnv::for_instance(ChildEnv::from_process(), instance)` (the instance config home), plus `TERM=xterm-256color`. cwd: the session cwd, checked against the roots.
- The board warns before handover: "In the terminal, the board does not see approvals or prompts. The station holds this session until you return it."

### 3.6 Board flow

- Running session → "Hand over to terminal": confirm → `session.stop` (graceful) → wait until the summary shows `exited`/`lost` and no current run (max 20 s; on timeout show "The session is still stopping. Try again when it has stopped.") → `shell.handover` → open a TUI tab in the dock.
- Ended session → "Open in terminal": `shell.handover` directly.
- "Return to board control": `shell.close` → wait until `summary.terminal` is `None` (max 15 s, else show the state) → `session.resume`.
- A held session shows "In terminal" in the sidebar and the header. The composer is off with the text "A terminal holds this session. Return it to board control to send prompts."

## 4. Layout

### Desktop (≥ 1024 px)

```
┌──────────────┬───────────────────────────────────────────────────────┐
│ FLEET DECK ● │ title · station · claude · opus · acceptEdits  [⏹][⋯] │
│ [+ New]  🔍  │ Chat | Changes | Files                                │
│ NEEDS YOU 2  │                                                       │
│  ▲ fix login │  you: fix the login bug                               │
│ station-a  ⌘ │  assistant: markdown …                                │
│  ● fix login │  ▸ Bash  npm test  ✓                                  │
│  ○ refactor  │  ▸ Edit  src/login.ts  ✓                              │
│ station-b  ⌘ │                                                       │
│  ● docs      │ ┌ approval: Bash rm -rf dist   [Allow] [Deny] ──────┐ │
│              │ └───────────────────────────────────────────────────┘ │
│ Fleet  Plans │ ┌ Message… (/ for commands)                         ┐ │
│ Stations     │ └ [mode ▾] [model] [📎]               [Interrupt][↑] ┘ │
│              ├───────────────────────────────────────────────────────┤
│              │ ▣ shell station-a ~/repo │ ▣ TUI fix login │  [_][×]  │
└──────────────┴───────────────────────────────────────────────────────┘
```

### Phone (< 768 px)

Session list (needs-you first) → tap → full-screen chat with a back button, the title, a status dot and an overflow menu (Changes, Files, Terminal, Stop, Resume, Hand over, Mode, Model, Rename, Auto-continue). Sticky composer with the safe-area inset. A bottom bar on the list screens: Sessions · Fleet · New · More. A terminal is a full-screen screen with a key bar (Esc, Tab, Ctrl, ↑ ↓ ← →).

## 5. Work split

- Wave 0: the protocol contract (§3.1) and the generated TS types.
- Wave 1 (parallel): A Rust handover + slash commands; B chat timeline + markdown; C session controller + header + composer + approval dock; D terminal dock + shell client.
- Wave 2: layout, routes, sidebar, phone, fleet home, SessionView composition, keyboard, e2e.
- Reviews: one `code-reviewer` review of the final branch; `adversary` PRE-PR review of the handover lock.
