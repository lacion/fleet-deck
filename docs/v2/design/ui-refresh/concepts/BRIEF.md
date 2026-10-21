# UI refresh: concept brief (shared by every direction)

*Branch `ui-refresh`, 2026-10-22. The owner says the board "feels a bit old school" and wants "a new improved, very beautiful UI. All cards on the table."*

This brief is the same for every concept. Each direction has its own file (`A-flight-deck.md`, `B-graphite.md`, `C-signal.md`) with the visual system. A concept is a **static, high-fidelity mock-up** of the real board. It is not product code. It must look like a finished product screen, not a wireframe.

## 1. The product

Fleet Deck is a self-hosted board that supervises many AI coding agents (Claude Code and Codex sessions). The agents run on several of the owner's own machines, called **stations**. The **deck** is the server that links the stations; the **board** is the web UI. One owner uses it on a desktop (keyboard first) and on a phone (thumb first).

The primary job of the board, in order:

1. Show at a glance which sessions **need you** (a permission request, a question, a plan to approve).
2. Let the owner act at once: Allow / Deny, answer, steer.
3. Show what each agent does now (its current tool, its focus line, its changes).
4. Start new sessions, review changes, and move between stations.

Vocabulary that the UI uses (keep these words): fleet, station, session, callsign (a short agent name, for example `falcon-vf9n`), needs you, working, idle, ended, steer, queue follow-up, interrupt, changes, artifacts, mail, tasks, land (merge a branch), conflict radar (two sessions changed the same file), usage.

Session statuses: `working`, `verifying`, `needs_you`, `queued`, `idle` (waiting for a prompt), `ended`, `lost`, `unreachable` (its station is offline).

## 2. Today's board (what to improve)

Screenshots of the current board: `docs/v2/design/ui-refresh/concepts/baseline/*.png`. Look at all of them before you start. The problems the owner sees:

- Flat boxes everywhere: 1 px borders around everything, boxes inside boxes (the New session form has 4 bordered panels).
- No hierarchy: almost all text is 11–13 px; titles are barely bigger than metadata.
- A terminal look: IBM Plex Sans + Plex Mono; model names in green mono text; mono everywhere for times and counts.
- Native browser controls: grey `<select>` boxes, native checkboxes, native `<details>` triangles.
- The fleet home is a sparse spreadsheet: four columns of text and 60 % empty space.
- A muddy light theme (brown-amber primary #8a5300 on blue-grey).
- The bottom panel (Activity · Mail & tasks · Assign · Usage) looks like a footer of links.
- No depth, no motion, no brand identity beyond a small icon.

## 3. Artboards (every concept delivers all 6)

| File | Viewport | What it shows |
| --- | --- | --- |
| `fleet.html` | 1440 × 900 | The fleet home (desktop), primary color mode. |
| `fleet.html?theme=alt` | 1440 × 900 | The same page in the other color mode (dark ↔ light). |
| `session.html` | 1440 × 900 | One session: Chat tab with tool calls, a code block, a table, an open approval and the composer. |
| `new.html` | 1440 × 900 | The New session form. |
| `phone-home.html` | 390 × 844 | The phone home: needs-you first, then the sessions by station, bottom tab bar. |
| `phone-session.html` | 390 × 844 | The phone session with the open approval. |

Every page supports `?theme=alt` (set `data-theme` on `<html>` from the query string with a 3-line inline script). The primary mode is the default. Only `fleet.html?theme=alt` is screenshotted in the alt mode, but the CSS of every page must work in both modes.

## 4. The scenario (use exactly this content on every artboard)

Now is 2026-10-22 00:30. All times are relative to now.

**Stations**

| Station | Platform | State | Sessions | Notes |
| --- | --- | --- | --- | --- |
| Lighting | Linux · x86_64 | online | 3 of 12 | Claude 2.1.306 signed in, Codex 0.163.0 signed in |
| dev1 | Omarchy · x86_64 | online | 2 of 12 | has a desktop (Omarchy) |
| mac-m4 | macOS · arm64 | online | 2 of 12 (1 ended) | Claude 2.1.305, Codex 0.162.1 |

**Sessions**

| # | Callsign | Title | Station | Branch / folder | Model · provider | Status | Detail |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `otter-n1rr` | Fix flaky shim test | Lighting | `fleet-sync` | Sonnet 5.5 · Claude | **needs you** (permission, 2 min) | Permission: **Bash** `cargo nextest run -p fleetdeck-station -E 'test(shim)'` |
| 2 | `kite-x81d` | Stripe webhooks | dev1 | `rannoch/webhooks` | GPT-6.1 Codex · Codex | **needs you** (question, 6 min) | Question: "Use the test key or the live key for the webhook secret?" Options: *Test key* / *Live key* |
| 3 | `falcon-vf9n` | Fleet Deck UI | Lighting | `ui-refresh` | Opus 5.5 · Claude | working, 14 min | Focus line: "Designing the new board: concept briefs". Current tool: Write `concepts/BRIEF.md`. Changes +412 −0 in 3 files |
| 4 | `lynx-4kq2` | Rannoch checkout flow | dev1 | `rannoch/checkout` | Opus 5.5 · Claude | working, 38 min | Current tool: Edit `src/checkout/cart.ts`. Changes +142 −37 in 6 files. 2 subagents running. PR #128 open |
| 5 | `wren-77ta` | Journzy iOS build | mac-m4 | `journzy/ios-ci` | Sonnet 5.5 · Claude | working, 9 min | Current tool: Bash `xcodebuild -scheme Journzy test`. Changes +18 −4 in 2 files |
| 6 | `heron-mc2q` | Omarchy integration docs | Lighting | `docs/omarchy-integration` | GPT-6.1 Codex · Codex | idle (waiting for a prompt, 2 h) | Last message: "The docs build passes. Next?" Changes +260 −31 in 4 files |
| 7 | `swift-0p3e` | Release notes 2.0 | mac-m4 | `release-notes` | Haiku 5.5 · Claude | ended, 1 d | Landed into `v2` |

**Conflict radar**: `falcon-vf9n` and `heron-mc2q` both changed `web/src/styles/tokens.css` in the last hour (same repository `fleet-deck`).

**Usage (rate limits)**: Claude · 5-hour window 62 % used, resets in 1 h 48 min. Claude · weekly 31 % used, resets Mon. Codex · 5-hour window 18 % used. Fleet totals today: 4.2 M input tokens, 610 k output tokens, $38.40.

**Mail & tasks**: 1 mail waiting for delivery to `heron-mc2q`. Task "Add webhook retry tests" assigned to `kite-x81d` (open).

**Session artboard (`falcon-vf9n` is NOT the one shown; show `otter-n1rr` "Fix flaky shim test")** — chat transcript, top to bottom:

1. User message: "The shim test fails about 1 in 20 runs on Lighting. Find out why and fix it."
2. Reasoning item, collapsed: "Thinking · 12 s".
3. Tool: **Read** `crates/fleetdeck-station/src/shim/reaper.rs` — ok.
4. Tool: **Search** `waitpid` in `crates/fleetdeck-station` — ok, 7 matches.
5. Assistant text: "The test races the reaper: the child can exit before `install_reaper()` returns, so the `SIGCHLD` is lost. Here is the fix:"
6. Code block (rust):
   ```rust
   pub fn install_reaper() -> io::Result<Reaper> {
       let mask = block_sigchld()?;       // block first
       let reaper = Reaper::new(mask)?;   // then install
       reaper.drain_exited();             // reap what exited before
       Ok(reaper)
   }
   ```
7. Assistant text with a table:

   | File | Problem | Fix |
   | --- | --- | --- |
   | `shim/reaper.rs` | SIGCHLD lost before install | block, install, drain |
   | `shim/tests.rs` | test waits 50 ms | wait for the exit event |

8. Tool: **Edit** `crates/fleetdeck-station/src/shim/reaper.rs` — ok, +9 −3.
9. Tool: **Bash** `cargo nextest run -p fleetdeck-station -E 'test(shim)'` — **waiting for your answer**.
10. The open approval card: kind "Permission", tool "Bash", the command in a code line, the reason "Run the shim tests 20 times to confirm the fix", and the options **Allow** (key `a`), **Allow for this session**, **Deny** (key `d`).
11. The composer: placeholder "Message otter-n1rr…", an attach-image button, and two send actions: **Steer now** (interrupts the current turn with the message) and **Queue follow-up** (the default, sent after the turn ends).

Session header: title "Fix flaky shim test", callsign `otter-n1rr`, status Needs you, station Lighting, branch `fleet-sync`, folder `~/projects/obsurvive/fleet-deck`, model Sonnet 5.5, provider Claude, mode "Accept edits". Actions: **Interrupt**, **Terminal**, **Open folder**, overflow menu (⋯). Tabs: **Chat**, **Changes** (badge 2), **Files**, **Artifacts**.

**New session form** fields (the real form; you may regroup and restyle them, never drop one):

- Task: "What should the agent do?" (a large text area; this is the main field). Placeholder example: "Add a dark mode toggle to the settings page".
- Title (optional).
- Batch: "One session in its own worktree for each line" (a toggle).
- Station: Lighting / dev1 / mac-m4 (show each station's online state and free slots).
- Workspace: "Work in this directory" / "New worktree from a branch" / "Existing worktree".
- Directory: a path field with **Browse…** (value `~/projects/obsurvive/fleet-deck`).
- Provider: Claude 2.1.306 / Codex 0.163.0.
- Model (optional): default per provider; show "Opus 5.5" selected.
- Permission mode: Station default / Ask / Accept edits / Plan / Bypass (bypass is risky: show a hazard hint when selected).
- More options (collapsed): environment, project, attach plan.
- Primary action: **Start session** (key ⌘↵). The current form says at the bottom "To start: choose a directory…": in the concept the form is valid and the button is enabled.

**Navigation destinations** (desktop sidebar or rail; phone "More"): Fleet (home), Watch, Activity, Stations, Plans, Automations, Artifacts, Projects, Files, Shares, Terminals, Settings. Also: New session (key N), Search sessions (key /), the connection state (Live), the theme switch, the account menu. Phone bottom bar: Sessions, Fleet, New, More.

**Fleet home content** (what the desktop home must show; the layout is the direction's choice):

- A clear "Needs you" area with the 2 open requests, each with its inline actions (Allow / Deny for the permission; the two options for the question) and the keyboard hints (`a`, `d`, `[` `]` to move between requests).
- Every live session, grouped by station, with: status, callsign, title, model, branch, what it does now (current tool or focus line), changes (+/−), age; PR chip for #128; "2 subagents" for `lynx-4kq2`.
- The ended session collapsed ("1 ended").
- Station health in the station group header: online, sessions used of max.
- Usage meters (rate limits) and the conflict radar entry.
- Activity, Mail & tasks, Assign: these are secondary; show them as a quiet affordance, not a footer of links.

## 5. Rules for every concept

1. **Static HTML + CSS only.** One shared `styles.css` per concept plus one HTML file per artboard. A tiny inline script is allowed only for `?theme=alt` and for the icon library. No frameworks, no build step.
2. **Icons**: Lucide (the real board uses `lucide-react`): `<script src="https://unpkg.com/lucide@0.469.0/dist/umd/lucide.min.js"></script>`, `<i data-lucide="git-branch"></i>`, then `lucide.createIcons({ attrs: { 'stroke-width': 1.75 } })`. Size the icons with CSS.
3. **Fonts**: from Google Fonts with `<link>` tags, exactly the families that the direction file names. (The product will bundle them later.)
4. **Real content only.** Use the scenario above. No lorem ipsum, no invented features beyond what this brief lists.
5. **Quality floor**: text contrast at least 4.5:1 (3:1 for large text and icons); a visible focus style exists in the CSS (`:focus-visible`); controls at least 28 px high on desktop and 44 px on the phone; nothing overflows or overlaps at the given viewport; the page fills the viewport exactly (no scrollbar on the desktop artboards; the chat column can scroll inside).
6. **One color means one thing.** Follow the semantic color table of the direction file. Never use the "needs you" color for decoration.
7. Copy is in sentence case, plain and short. No all-caps labels unless the direction file says so.
8. Do not edit files outside your concept folder. Do not run `git commit` unless your task says so.

## 6. How to render

```sh
node docs/v2/design/ui-refresh/concepts/shoot.mjs <folder>   # for example: a
```

The script writes `<folder>/shots/*.png` (it uses Playwright from `web/node_modules`; run `bun install` in `web/` first if that folder is missing). Look at every PNG yourself, fix what is wrong, and render again. Do at least 2 rounds of self-critique: compare against the direction file and against the rules above.

## 7. Deliverable

- `docs/v2/design/ui-refresh/concepts/<folder>/` with `styles.css`, the 5 HTML files, and `shots/` with the 6 PNG files.
- A `NOTES.md` in the folder (max 30 lines): what you built, any place where you could not follow the direction file and why, and the 3 weakest spots that you see in your own concept.
