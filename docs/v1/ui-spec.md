# Fleet Deck v1.0 — board UI specification

*Companion to the [design-spec](./design-spec.md) capstone. Part of [Fleet Deck v1.0](./README.md). The design-spec says what the deck *is* at 1.0; this doc specifies every **surface, control, state, and interaction** on the board so an implementer can build without guessing. Grounded in the actual board source (v0.22.4, `board/src/**` — anchors cited); every element is marked **(unchanged)**, **(changed)**, **(moved)**, or **(new)**. Where a control's behavior differs by state (empty / degraded / unauthorized / provider-limited), the state is enumerated — a case not listed here is a case not designed.*

**Conventions used throughout:**
- *operator* / *worker* refer to the [P5](./p5-programmable-fleet.md) token classes. "Operator-gated" = the control renders but disabled with an explaining tooltip unless the board session holds an operator token.
- Every new modal joins the existing Esc/overlay discipline (§12) and the hotkey-suppression list (`useBoardHotkeys.js:110-112` — a new overlay MUST be threaded into `blockingOverlayOpen`, and the `?`-overlay HOTKEYS list in `helpText.js` updates in the same commit, per the rule in `useBoardHotkeys.js:4-6`).
- All new payload shapes come from `contracts/` — no new comment-contracts.

---

## 1. Global chrome — the header at 1.0

Today's header (`Header.jsx:26-146`): wordmark · WS pill · stale banner · legacy-upgrade chip · NEEDS YOU chip · fleet line · clock · daemon version · then buttons. The 1.0 inventory, left to right:

| Element | v0.22.4 | v1.0 | Notes |
|---|---|---|---|
| Wordmark, WS pill, stale banner | ✓ | **(unchanged)** | |
| Legacy-upgrade chip | ✓ | **(unchanged)** | |
| NEEDS YOU chip | ✓ | **(changed)** | count now spans both providers; clicking still jumps the inbox rail |
| Fleet line | `N sessions · N conflicts` | **(changed)** | adds per-provider split when Codex present: `7 sessions (5 claude · 2 codex) · 1 conflict` |
| Clock, daemon version | ✓ | **(unchanged)** | |
| **Usage chip** | — | **(new)** | fleet-total burn vs the tightest reset window, e.g. `▮▮▮▯ 62% · 5h`; turns warning at 80%; renders `usage: unknown` when no window is known ([P4](./p4-usage-accounts.md) — never a fabricated number). Click → opens the ops strip (§10) |
| ✉ Compose (`c`) | ✓ | **(unchanged)** | |
| ▦ Terminals (count) | ✓ | **(unchanged)** | §6 |
| + Spawn (live count) | single button → form | **(changed)** | becomes a **split button** — §3.1 |
| ⌸ Files | ✓ | **(unchanged)** | read-only FileViewer, home-rooted |
| **▤ Stream** | — | **(new)** | toggles the Slack-style stream view (§8); badge = total unread across channels (read cursors) |
| ⌫ Clear | ✓ | **(unchanged)** | offline sessions only |
| ⑂ Worktrees (badge/hazard) | ✓ | **(unchanged)** | |
| ⇄ Share (LAN dot) | ✓ | **(unchanged)** | LanPanel as-is; explicitly NOT merged into Settings — it must stay reachable when you're trying to turn LAN on (`LanPanel.jsx` rationale) |
| **⚙ Settings** | — | **(new)** | opens the Settings modal (§4). Visible always; write actions inside are token-gated per tab |
| Density / theme / ? Help | ✓ | **(unchanged)** | |

**Header degraded states:** WS `reconnecting`/`offline` pill as today; when the daemon predates a feature the button hides on capability-absence exactly like `wtSupported` does today (`Header.jsx:110`) — **every new header button ships with a capability probe** (`/state` advertises `capabilities: {stream, usage, settings_v2, providers: [...]}` in the snapshot; absence hides the control rather than rendering a dead button).

---

## 2. Navigation & view model

- **Views (mutually exclusive, full-page):** **Board** (lanes of cards — default) · **Stream** (§8) · **Terminal grid** (full-screen overlay, as today).
- **Overlays over any view:** Drawer (per-card), Compose, SpawnForm, Settings, FileViewer, WorktreesModal, LanPanel, PlanLibrary, Help, DirPicker, and the confirm dialogs (§11).
- View state persists per tab (like `threads` today, `App.jsx:58`); deep links: `#stream/<channel-id>` and `#session/<sid>` open the respective view/drawer on load.

---

## 3. The spawn surface

### 3.1 The button **(changed)**

`+ Spawn` becomes a **split button**: the main face does exactly what it does today (opens the quick-spawn form); the `▾` opens a menu:

| Menu item | Opens | Gate |
|---|---|---|
| **Quick spawn** | §3.2 form | as today (`spawnAvailable`) |
| **From issues…** | issue browser (§9.1) | disabled + tooltip when no forge CLI detected (§4.1) |
| **Review a PR…** | PR-review flow (§9.2) | disabled + tooltip when no forge CLI detected |

The live-count badge is unchanged. Hotkey: none today, none added (Compose owns `c`; see §13).

### 3.2 The spawn form at 1.0 — full field spec

Basis: `SpawnForm.jsx` (1,204 lines). Fields listed in render order; unlabeled rows keep today's `frow`/`fl` layout.

| # | Field | Kind | v1.0 status | Behavior, validation, cases |
|---|---|---|---|---|
| 1 | Spawn target | radiogroup `this machine (cwd)` / `a repo` | **(unchanged)** | as today (`SpawnForm.jsx:634-648`) |
| 2 | cwd path + 🗀 | text + DirPicker | **(unchanged)** | |
| 3 | Repo shorthand + default org + host (github/gitlab) + transport (https/ssh) + 🗀 | inputs + radiogroups | **(unchanged)** | dual-forge rules as today |
| 4 | Branch | text | **(unchanged)** | existing-or-new |
| 5 | repos_dir + 🗀 | text | **(unchanged)** | placeholder from settings |
| 6 | **Provider** | radiogroup `claude` / `codex` | **(new)** | default `claude`. `codex` disabled with tooltip *"enable the Codex provider in ⚙ Settings → Providers"* until consent is granted (§4.3). Selecting `codex` (a) relabels the model input with Codex model ids, (b) shows the reduced-telemetry note *"Codex cards are turn-level + shell telemetry — see what this means"* (links P1 docs), (c) maps the permission ladder onto Codex's approval×sandbox grid (§3.3), (d) hides Claude-only controls (remote control — Tier C, `/rc` is Claude-gated today). A `claude` session runs **driven by default** (`claudeSdk`; answerable approvals, interrupt, steer — [P7](./p7-drive-and-observe.md)) and falls back to the observed floor automatically when the runner or login is unavailable — this field picks the **provider**, never the posture |
| 7 | **Account** | select (default home + configured accounts) | **(new, stretch)** | rendered only when ≥2 accounts configured (§4.4) AND the daemon advertises the `accounts` capability. Default = default home. Tooltip shows the account's tightest window ("resets in 15m"). If the pinning stretch is cut, this select never renders — the form must not show a dead control |
| 8 | Prompt | textarea, `3x` batch prefix | **(changed)** | batch parsing stays board-side for quick spawn but POSTs the **server-side batch endpoint** when N>1 ([P3](./p3-issue-pr-spawning.md)); placeholder behavior as today. In **issue mode** (§9.1) the prompt is pre-filled with the fenced issue block and the fence is **read-only** (visually distinct, non-editable region) — the operator may add text *around* it, never edit inside it |
| 9 | Model | text | **(unchanged)** | placeholder switches per provider |
| 10 | **Permission ladder** | segmented control, 4 modes | **(changed — consolidation)** | replaces BOTH today's `permission-mode` select (`SpawnForm.jsx:945-948`) and the separate unsupervised checkbox+arm flow. §3.3 |
| 11 | Worktree | checkbox | **(unchanged)** | forced-on when batching, as today |
| 12 | Remote control | checkbox | **(changed)** | Claude-only (hidden for codex); still mutually exclusive with gateway routing (`SpawnForm.jsx:363-369` — the losing checkbox disables, never blocks submit) |
| 13 | **Gateway routing** | single toggle `route via LLM gateway` | **(moved)** | today the FULL gateway profile (URL/token/style/discovery/default) is edited inside this form (`SpawnForm.jsx:112-122,332-346`). At 1.0 the profile moves to ⚙ Settings → Gateway (§4.2); the form keeps only this per-spawn toggle, seeded from `gateway.default`, disabled with tooltip *"configure the gateway in ⚙ Settings"* when `!gateway.ready` |
| 14 | Plan-mode hint, batch hints, failure strip with per-arm retry | — | **(unchanged)** | retry-failed-arms semantics as today (`SpawnForm.jsx:552`) |

**Explicitly not in the form:** `setup_cmd`. It stays API-only, and at 1.0 requires an **operator token** on the request ([P5](./p5-programmable-fleet.md)). The form never exposes it — decided, not omitted.

### 3.3 The permission ladder **(the consolidation, specified)**

One segmented control, four modes, one hazard flow:

| Ladder mode | Claude mapping (`--permission-mode`) | Codex mapping (approval × sandbox) | Hazard UI |
|---|---|---|---|
| **Supervised** | `default` | `untrusted` × `read-only` | none |
| **Auto-accept edits** | `acceptEdits` | `on-request` × `workspace-write` | none |
| **Auto** | `plan` → then auto (plan hint row as today) | `on-failure` × `workspace-write` | none |
| **Full access ⚠** | `bypassPermissions` | `never` × `danger-full-access` | reveals the **arm row** — the same two-step, server-side single-use 60 s arm-token flow that exists today (review #15; the form never quietly downgrades, `SpawnForm.jsx:74-77`) |

Cases: selecting away from Full-access disarms (as today, `SpawnForm.jsx:940`); **issue-mode locks the ladder to Supervised** — control disabled with the note *"spawns from issue text are always supervised"* ([P3](./p3-issue-pr-spawning.md) injection boundary); batch spawns apply one ladder mode to every arm; the Codex mapping renders the resolved pair as fine print under the control so the operator sees exactly what will be passed. When a session is **driven** ([P7](./p7-drive-and-observe.md)) the same rung sets the runtime's live posture — Claude's SDK `permissionMode`, Codex's app-server approval policy — and each approval it would otherwise gate becomes an **answerable card** (allow · deny · steer) on the board and phone, not a pane-only prompt.

---

## 4. The ⚙ Settings modal **(new)**

One modal, left tab rail, right pane. Opens over any view. Read is open; **every write is token-gated** (the pattern `POST /api/settings` already enforces — `http.mjs:1054-1070`); tabs that need operator show a lock chip when the board session lacks it. Esc closes (slots into the §12 chain). Each tab below lists every control and its degraded states.

### 4.1 Integrations (forges)

| Control | Spec |
|---|---|
| GitHub row | detection status of `gh`: **found** (version, `gh auth status` identity, e.g. `lacion`) / **not found** (install hint text — never an install button: [[git-auth-qol-roadmap]] "never auto-install CLIs") / **found, unauthenticated** (`gh auth login` hint). Read-only — Fleet Deck never stores forge tokens |
| GitLab row | same, for `glab` |
| Default forge | radiogroup github/gitlab — seeds the spawn form's host radiogroup default |
| PR-write allowlist display | the three verbs (create PR · post review · comment), each with an enable checkbox (operator). Default all-on when a CLI is authenticated. Fine print: *"every write asks for confirmation and leaves an audit line — never batched, never automatic"* |
| Trackers (Jira/Linear) | a single disabled row: *"post-1.0"* — present so the cut scope is visible, not discoverable-by-absence |

**Degraded:** neither CLI found → the tab still renders (with hints), and §3.1's issue/PR menu items disable with *"no forge CLI detected — see ⚙ Settings → Integrations"*.

### 4.2 Gateway & proxies **(moved here from the spawn form)**

Exactly today's profile, relocated: `gateway_base_url` (URL, validated per `settings.mjs:340-356` — no embedded credentials, no query, http/https only) · `gateway_token` (write-only; display is masked and `token_set: true` is the whole truth shown, `settings.mjs:386-397`) · auth style bearer/api-key · model discovery toggle · `gateway_default` ("route new spawns through the gateway unless told otherwise") · a **ready** pill (`base_url && token_set`).
**(new)** CPA row: *"CLIProxyAPI usage queue"* toggle + queue URL — the optional richer usage source for gateway-routed sessions ([P4](./p4-usage-accounts.md)); off by default; status pill `consuming / idle / error(reason)`.
**Degraded:** URL set but no token → ready pill red, spawn-form toggle disabled (as the board gates on `ready` today).

### 4.3 Providers

| Control | Spec |
|---|---|
| Claude row | always-on; shows hook bundle version + sessions observed + **drive status** ([P7](./p7-drive-and-observe.md)): *SDK runner detected* / *driving N* / *floor-only* (runner or login unavailable → sessions run as the observed floor) |
| Codex row | tri-state: **not installed** (a single `Enable Codex observation…` button → the consent dialog, §11) / **installed** (shows what was written: `~/.codex/hooks.json` + the `[features].codex_hooks` flag; an **Uninstall** button that reverts both — the uninstall story [P1](./p1-codex-provider.md) requires) / **degraded** (hooks flag off or engine unavailable → *"observing via notify + pane liveness — cards are coarser and labeled"*) |
| Tier note | static text: what a Codex card shows (turn-level, shell telemetry) and doesn't (file chips, conflict radar) — the honesty is part of the UI. Adds: *"drive — answerable approvals, interrupt, steer — lands for Codex after Claude, once its hooks stabilize; until then Codex sessions are floor-only"* ([P7](./p7-drive-and-observe.md)) |

### 4.4 Accounts **(stretch — whole tab cut if pinning is cut)**

Rows = configured config-homes (`CLAUDE_CONFIG_DIR` / `CODEX_HOME` paths): add (path picker + provider), remove, set-default. Per-row: masked identity, per-account limit bars (tightest window first), *"resets in Xm"*. Fine print: credentials never leave the machine, never on `/state` ([[fleetdeck-security-standing]] discipline). **Degraded:** a home whose transcripts aren't readable renders *"unreadable — pinned spawns to this home lose revive/adopt"* (the P4 refactor's honest failure mode).

### 4.5 Access & tokens (operator only — the whole tab)

`REQUIRE_TOKEN` state (read-only display of the daemon config) · **worker token** and **operator token** rows: mint/rotate, shown **once** at mint (copy button, never displayed again — same show-once discipline as the LAN QR credential) · **blessed coordinators** list (sessions granted operator; add by callsign; revoke) · **spawn cap** numeric input (per-hour quota for agent-initiated spawns, [P5](./p5-programmable-fleet.md)) · audit note: every grant/revoke/rotate writes a feed + stream line.

### 4.6 Board

Density and theme (mirrors of the header toggles) · stream retention display (read-only, from daemon) · default view on open (Board/Stream). Deliberately thin — board prefs stay mostly in the header where they live today.

---

## 5. Session cards at 1.0 — anatomy

Zones top-to-bottom (basis `SessionCard.jsx`, 427 lines today):

1. **Identity row** — callsign, repo, branch, **(new)** provider badge (`◆ claude` / `◇ codex`), **(new)** `reduced` chip on Codex cards (tooltip explains Tier A telemetry), **(new)** `driven` chip when Fleet Deck is driving the session (tooltip *"approvals are answerable here"*; absent on the observed floor — [P7](./p7-drive-and-observe.md)), status pill (`queued → working → verifying → needs-you → idle → offline` — unchanged vocabulary).
2. **(new) Usage chip** — per-session burn vs its account's tightest window; `unknown` state renders dimmed, never fabricated.
3. **Activity zone** — file chips + conflict ripples (Claude only; absent on Codex cards — absence is honest, not broken), sparkline, last line.
4. **(new) Review strip** ([P2](./p2-harvest-surface.md)) — `Δ diff` button (opens the diff viewer, §7) · checkpoint tick-strip (one tick per passing-Stop turn; hover = turn n + time; click = diff-this-turn) · `↩ revert` on the newest tick — **idle-gated**: disabled while status is working/verifying with tooltip *"agent is mid-turn — revert is available when idle"*; click → revert confirm (§11). Cards with no recorded `base_ref` (adopted/legacy) show `Δ diff (inferred base)` — the [P2](./p2-harvest-surface.md) label, still functional.
5. **Action row** — watch tick (grid selection), open terminal, mail, kill, rename — **(unchanged)**, all provider-aware via the strategy object (a Codex card's mail button follows its **floor capability**: enabled, or hidden-with-capability-absence — never rendered-but-broken). On a **driven** session the row also carries the drive controls (§5.1).

Drawer (`Drawer.jsx`): gains the same review strip expanded (full checkpoint list), the turn-level chat thread when P6 chat ships (composer already exists, `Drawer.jsx:423-451`), and the per-card terminal open **(unchanged**, `App.jsx:458`).

### 5.1 Driven-session controls **(new — [P7](./p7-drive-and-observe.md))**

A session Fleet Deck is **driving** (`claudeSdk` now; `codexAppServer` when staged) grows a control strip the observed floor never shows — and every control degrades to *absent* (not broken) the instant the session falls back to the floor:

- **Answerable approvals.** A `needs-you` from a driven session renders as an **actionable card** (allow · deny · always-allow-this-tool · steer) on the card, the inbox, the stream, and the phone — resolved in-protocol (Claude `canUseTool`, Codex `item/{commandExecution,fileChange}/requestApproval`), not by typing in the pane. On the floor the same prompt is display-only and points at the terminal (§6).
- **⛔ Interrupt.** Stops the current turn without killing the session (Claude `Query.interrupt()`, Codex `turn/interrupt`) — armed only while working/verifying, disabled with *"idle — nothing to interrupt"* otherwise.
- **Steer.** On a driven session the card/drawer composer **injects mid-turn** (streaming-input / a mid-turn `turn/start`) instead of queuing mail — labeled *"steer (lands this turn)"* to distinguish it from mail-to-pane. On the floor the composer stays mail.
- **Inline plan / diff review.** `ExitPlanMode` and turn diffs surface as an inline approve/deny review on the card (capture-and-decide), reusing the §7 diff renderer — the plan gate becomes a board control, not a pane keystroke.
- **Resume without the pane dance.** Revive on a driven session resumes via the SDK session id / `thread/rollback` (the strategy's drive override, [architecture](./architecture.md#the-drive-override-layer-4--p7)) — no `--resume` typed into a pane.

The authoritative live surface for a driven session is the **runner-in-a-pane** (§6, path 8): Fleet Deck's own render of the driven stream, not a scraped vendor TUI. All of this is **capability-gated** (§14): a daemon without the `drive` capability renders today's floor card, never dead controls. Every drive control is **operator-token-gated** — a worker token can watch and mail, never seize the wheel ([P5](./p5-programmable-fleet.md)).

---

## 6. Terminal access — the complete matrix

Every path to a live terminal, current and new:

| # | Path | Surface | Status | Behavior |
|---|---|---|---|---|
| 1 | Header **▦ Terminals** | full-screen **TermGrid** | **(unchanged)** | ticked ("watchable") sessions, else all live (`Header.jsx:68-81`); one focused tile accepts input; Esc never reaches a terminal (`useBoardHotkeys.js:78-79`) |
| 2 | Card → open terminal | floating **TermWindow** | **(unchanged)** | non-modal, draggable, ✕/dock only — Esc never closes it (`useBoardHotkeys.js:73-77`) |
| 3 | Drawer → open terminal | floating TermWindow | **(unchanged)** | closes the drawer (`App.jsx:458`) |
| 4 | Card watch tick → grid | TermGrid selection | **(unchanged)** | |
| 5 | **Stream channel → ⌨** | floating TermWindow | **(new)** | every session-channel header carries `⌨ terminal` + `→ card`; posting in the channel is mail, the terminal is one click when mail isn't enough |
| 6 | **Needs-you (inbox/stream) → ⌨** | floating TermWindow | **(new)** | a needs-you rendered in the stream or inbox offers open-terminal beside allow/deny — the escalation path when a prompt needs eyes |
| 7 | **Chat thread → "open terminal"** | floating TermWindow | **(new, ships with P6 chat)** | the chat surface is explicitly secondary; this link is its standing reminder that the terminal is authoritative |
| 8 | **Driven session → runner-in-a-pane** | rendered **RunnerPane** | **(new — [P7](./p7-drive-and-observe.md))** | a driven session's authoritative surface is Fleet Deck's live render of the SDK / app-server stream (plans, diffs, the gate inline) in a pane — not a scraped vendor TUI; still tmux-generic and termable, and it **falls back to a plain observed pane** when the session drops to the floor |

Transport and authz: all paths ride `/ws/term`, which **remains a token-gated power route even on loopback** (`http.mjs:336-348`) — at 1.0 it requires an **operator** token (a terminal is keystroke-injection; worker tokens don't get it). Codex panes are termable like Claude panes (tmux-generic). Grid and window behaviors (focus, dock, resize) are unchanged.

---

## 7. The diff viewer **(new renderer, existing shell)**

Opens from a card/drawer `Δ diff` or a checkpoint tick. Reuses the FileViewer modal *shell* only — the review is explicit that FileViewer has no diff mode (`FileViewer.jsx:150-184`): this is a **new renderer**.

- **Scope selector:** `since spawn` (base_ref..HEAD) · `turn n` (checkpoint n-1..n) — pre-selected by what you clicked.
- **Content:** unified diff, hunk model, per-file collapse, untracked files appended from `git status --porcelain`; syntax-highlight-free at 1.0 (cap scope); large diffs cap + paginate per [P2](./p2-harvest-surface.md).
- **Notes mode:** click a line gutter → note anchor; a note rail collects them; **`Send N notes as one mail`** composes ONE line-anchored mail (never a drip) and clears the rail. The button is disabled with zero notes; a session that goes offline before send keeps the rail (this-tab state, like `threads`).
- **States:** no `base_ref` → banner `inferred base (merge-base with origin/HEAD)`; binary files → `binary — not rendered`; diff too large → first N files + `show next page`.

---

## 8. The Stream view **(new)**

Full-page view (§2), toggled by header ▤ or hotkey (§13).

- **Layout:** left channel rail + right message pane, one channel active.
- **Channels:** one per live/recent session (callsign-named) + one per repo (rollup) + `#fleet` (spawns/despawns/system). Rail rows: name, provider dot, unread count (read cursor), needs-you flame when pending.
- **Message types** (from the structured `event_stream` table, [P6](./p6-unified-views.md) — selective, never the full tool firehose): turn boundaries · selective tool actions · needs-you prompts (rendered as actionable cards inline — y/n/choice work here exactly like the inbox rail, same qbus handles) · mail in/out · checkpoint ticks · forge-write **audit lines** ([P3](./p3-issue-pr-spawning.md)) · spawn/kill/revive events.
- **Composer:** posting into a session channel **is** mail to that session (one input, send-on-Enter, Shift+Enter newline — same as Compose); posting into a repo channel or `#fleet` is disabled with *"pick a session channel to talk to an agent"*.
- **Channel header:** session status pill · `⌨ terminal` · `→ card` (jumps Board view, opens drawer) · mute toggle (silences unread badge only).
- **Data path:** per-channel fetch + cursor (NOT the 60 ms full-snapshot WS broadcast — the review is explicit the stream needs a delta/per-channel path, `http.mjs:1370-1382`).
- **States:** empty fleet → explainer panel; daemon without the `stream` capability → header button hidden (§1); a channel whose session is offline renders history read-only with a `revive` affordance when eligible.

---

## 9. Forge flows **(new)**

### 9.1 From issues… (the fan-out)

Stepper modal, 3 steps — every step lists its failure state:

1. **Pick source.** Forge (from §4.1 detection) + repo (defaults to the current repo's forge remote). *No CLI / unauthenticated → step renders the §4.1 hint and stops.*
2. **Pick issues.** List with multi-select (number, title, labels, assignee), search box, `N selected` counter. *Empty list → "no open issues match".* *API error → the `gh`/`glab` stderr, verbatim, in a copyable block.*
3. **Review the fan-out.** One row per selected issue: worktree name preview (`repo--fd-PROJ-123-<animal>` — naming as today, `spawns.mjs:1076-1088`), branch, the **fenced prompt preview** (issue title+body inside the untrusted-data fence, read-only), per-row deselect. Global controls: provider, model, worktree root — and the ladder **locked to Supervised** (§3.3). Submit = one **server-side batch** POST; results reuse the spawn-failure strip with per-arm retry (§3.2 #14).

### 9.2 Review a PR…

Single modal: PR URL input (validated against known forges) → resolve (shows title/author/branch) → options: reviewer **skill** select (detected from the user's installed skills, e.g. `security-review`, `ai-code-review`; "none — freeform review" allowed) + provider + ladder (defaults Supervised) → spawn. The daemon checks out the branch in a fresh worktree and spawns the reviewer; the card appears tagged `review: PR #N`. *Failure states: URL not a PR / CLI missing / checkout fails (stderr verbatim, worktree cleaned up).*

### 9.3 The PR-write confirm **(the P3 allowlist, as a dialog)**

Any PR-scoped write (create PR · post review · comment) — from a card action or an agent's request surfaced as needs-you — opens ONE dialog: **verb** · **target** (repo + PR/branch) · **identity preview** (*"as `lacion` via `gh`"*) · full body preview (scrollable) · `Cancel` / `Confirm`. Confirm posts, writes a feed line AND a stream audit line (§8), and closes. **Never batch** — N writes = N dialogs by design; there is no "confirm all". Operator-gated. Declining is silent (no forge call). Failure: CLI exit ≠ 0 → dialog stays open with stderr, retry allowed.

---

## 10. The ops strip **(new — P4)**

A collapsible strip under the header (opened by the header usage chip, or pinned open from ⚙ Board): per-session rows sorted **tightest-window-first** — callsign · account · provider · burn bar vs window · reset countdown — plus a fleet-total row and per-account bars when accounts exist. Every unknowable value renders **`unknown`** (dimmed, tooltip: *"this agent hasn't written usage state Fleet Deck can read"*) — never a guess, never a zero. Numbers update when agents write (the honest-caveat line renders in the strip footer).

---

## 11. Dialog & confirm inventory

Complete list at 1.0. Existing: KillConfirm · ArmMoveConfirm · RenameDialog · DirPicker · TokenGate · the spawn arm row. New:

| Dialog | Trigger | Contents | Buttons | Gate |
|---|---|---|---|---|
| **Codex consent** | §4.3 Enable | exactly what will be written (`~/.codex/hooks.json`, `[features].codex_hooks = true` in `config.toml`), the uninstall promise, the fallback if declined (notify+liveness, labeled) | Cancel / `Write Codex config` | operator |
| **Revert turn** | card/drawer ↩ | turn n summary (files touched, checkpoint time), *"restores the worktree to the end of turn n-1; the agent is idle"* | Cancel / `Revert turn n` | operator; refuses (with reason) if the session left idle since the dialog opened |
| **PR write** | §9.3 | verb, target, identity, body preview | Cancel / Confirm | operator |
| **Token mint/rotate** | §4.5 | the new token, shown once, copy button | `Done` (no cancel after mint) | operator |
| **Uninstall Codex hooks** | §4.3 | what gets reverted | Cancel / `Remove` | operator |

All five: modal, Esc = Cancel (except Token mint: Esc = Done), threaded into `blockingOverlayOpen`, focus-trapped, `aria-modal` like SpawnForm (`SpawnForm.jsx:622`).

---

## 12. Overlay & Esc order (updated)

Today's chain (`useBoardHotkeys.js:78-97`): grid swallows Esc → kill/arm/rename → help → file viewer (peels off over the drawer) → drawer/compose/spawn/lan/worktrees. At 1.0 the chain gains, in the same modal tier as kill/arm/rename: **the five §11 dialogs**; and in the panel tier: **Settings** (closes like spawn/compose) and the **diff viewer** (peels off over the drawer, exactly like FileViewer — it may be opened from one). The Stream is a *view*, not an overlay — Esc in Stream does nothing (matches Board). Floating TermWindows remain Esc-immune.

---

## 13. Keyboard map

Existing (unchanged, `useBoardHotkeys.js` + `helpText.js`): `j/k ↓/↑` inbox rail · `y/n` permission · `1-9` choice · `Enter` freeform focus · `c` Compose · `?` Help · `Esc` per §12. Modifier chords never board hotkeys (`useBoardHotkeys.js:100-104`).

New at 1.0 (added to `helpText.js` HOTKEYS in the same commit, per the standing rule):

| Key | Action | Suppression |
|---|---|---|
| `v` | toggle Board ↔ Stream view | suppressed under any overlay, and while typing |
| `u` | toggle the ops strip | same |

Deliberately minimal — the inbox answer keys (`y/n/1-9`) work identically on needs-you cards rendered in the Stream (§8), which is the real keyboard win; no new chord grammar.

---

## 14. Cross-cutting

- **Capability-gating over version-sniffing:** every new surface renders on a `capabilities` flag in the snapshot, and hides entirely when absent (the `wtSupported` pattern) — an old daemon under a new board shows today's board, not broken chrome. This is the UI half of the skew rules in [validation-and-gates](./validation-and-gates.md).
- **Types:** all new components consume `contracts/` types; new files are `.tsx` per [ts-migration](./ts-migration.md) F1b; `useFleetState` converts when the snapshot contract lands.
- **Honesty as a UI principle** (recurring, deliberate): Codex cards labeled `reduced`; inferred bases labeled `inferred`; unknown usage labeled `unknown`; degraded providers labeled with *why*. A Fleet Deck surface never renders a guess as a fact.
- **Theming/density:** all new surfaces implement both themes and both densities from day one (they're header toggles; a new view that ignores `compact` is a regression).
- **A11y:** dialogs `role="dialog" aria-modal` (existing pattern); radiogroups labeled (existing pattern, `SpawnForm.jsx:634`); the stream is a `role="log"` region; the ladder a `radiogroup` with the hazard mode `aria-describedby` its warning.

## Definition of done (UI)

Every control in this doc exists with every listed state reachable and demonstrated (including degraded/empty/unauthorized); no new surface renders on a daemon that lacks its capability flag; the gateway profile is gone from the spawn form and lives in Settings; the ladder replaced the dropdown+checkbox pair; every new overlay participates in Esc/suppression; `helpText.js` matches the shipped hotkeys; the driven-session control strip (§5.1 — answerable approvals, interrupt, steer, inline plan/diff, runner-in-a-pane) exists on a driven session and is **absent, not broken**, on the observed floor; and every label promised here verbatim (`reduced`, `inferred base`, `unknown`, `driven`) appears in the shipped UI.
