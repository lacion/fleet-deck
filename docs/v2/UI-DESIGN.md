# Board visual design (DL-141)

*Written 2026-10-12 on `v2` @ `3a3c616`. The structure of DL-140 (UI-PLAN.md) stays: sidebar, chat-first session, terminal dock, phone stack navigation. This file replaces the visual layer. Every worker brief copies from it. When a brief and this file disagree, the brief wins.*

## 0. The problem

The DL-140 board works, but it does not look or feel like Fleet Deck:

| Defect | Cause |
| --- | --- |
| A fallback system font everywhere | `index.html` loads no font; IBM Plex is named but not present. |
| Huge, clumsy buttons on the desktop | 44 px `--tap-min` applies to every control, not only to touch screens. |
| Fleet cards are data dumps | "Input unknown · Output unknown · Cost unknown", "No current tool", "Workspace:", an "Open" button in each card. |
| 8 lane boxes, most of them "No sessions" | Every status is its own boxed lane. |
| "Needs you" 3 times on the home, 2 times in a session | Sidebar group + banner rail + lane; chat card + dock. |
| No visual system | 9 CSS files by different workers, no shared button/badge/chip rules, no icons. |
| Diffs without colors, Copy button over the code on a phone | No diff renderer; the Copy button is an overlay. |
| The terminal dock takes a row when it has no tabs | The "Open shell in workspace" bar always renders. |

## 1. Principles (from the v1 "1a Console" design)

1. **Amber (`--act`) means "yours to act".** Only needs-you items, the primary action of a decision, the focus ring and the selected tab use amber. A model or provider badge is never amber.
2. **Mono for data, sans for prose.** Titles, prose, buttons: IBM Plex Sans. Callsigns, paths, branches, models, times, counts, commands: IBM Plex Mono. Labels in sentence case (§8).
3. **Dense on a desktop, roomy on touch.** Controls are 30 px high (24 px small) with a fine pointer; 44 px only with `(pointer: coarse)`.
4. **Show what is known, hide what is unknown.** No "unknown" or "not reported" text on cards and rows. A missing value is an absent element.
5. **One place for each thing.** A needs-you request has one control surface per screen.
6. **Quiet chrome, loud state.** Borders are `--border-soft`/`--border`; color comes from state (pulse dots, badges, the amber edge).

## 2. Foundation (wave 0)

### 2.1 Fonts

Bundled, so the board works offline on a LAN: `@fontsource/ibm-plex-sans@5.3.0` (latin 400, 500, 600, 700) and `@fontsource/ibm-plex-mono@5.3.0` (latin 400, 500, 600), imported in `main.tsx` before the style sheets.

### 2.2 Icons

`lucide-react@1.47.0`, through one wrapper, `web/src/ui/icons.tsx`: `<Icon of={Plus} />` renders 16 px, stroke 1.75, `aria-hidden`, class `fd-icon`. Components import icons only from `ui/icons.tsx` (it re-exports the set below). An icon-only button always has an `aria-label`.

| Use | Icon |
| --- | --- |
| New session, new shell | `Plus` |
| Search | `Search` |
| Back, chevrons | `ChevronLeft`, `ChevronRight`, `ChevronDown`, `ChevronUp` |
| Overflow menu | `Ellipsis` |
| Interrupt | `Square` |
| Stop process | `Power` |
| Resume | `Play` |
| Terminal, shell | `SquareTerminal` |
| TUI tab | `MessageSquare` |
| Return to board control | `Undo2` |
| Rename | `Pencil` |
| Close, detach, dismiss | `X` |
| Copy / copied | `Copy` / `Check` |
| Add image | `Paperclip` |
| Send | `ArrowUp` |
| Jump to latest | `ArrowDown` |
| Branch | `GitBranch` |
| Diff | `GitCompare` |
| Folder, workspace | `Folder` |
| Brand mark | `Zap` |
| Fleet | `LayoutGrid` |
| Plans | `FileText` |
| Stations | `Server` |
| Sessions (phone bar) | `MessagesSquare` |
| Notifications on / off | `Bell` / `BellOff` |
| Warning, conflict | `TriangleAlert` |
| Subagent | `Workflow` |
| Background tasks | `Layers` |
| Tool: shell, edit, read, search, web, mcp, subagent, other | `SquareTerminal`, `Pencil`, `FileText`, `Search`, `Globe`, `Plug`, `Workflow`, `Wrench` |
| Tool status: ok, failed, denied | `Check`, `X`, `Ban` |
| Log out | `LogOut` |
| Sidebar drawer (tablet) | `PanelLeft` |
| Keyboard help | `Keyboard` |

### 2.3 Format helpers

`web/src/ui/format.ts`: `modelFamily(model)` and `shortModel(model)` (exact code in the wave-0 brief). `claude-sonnet-4-5` → `Sonnet 4.5`, family `sonnet`; `gpt-5-codex` → `gpt-5-codex`, family `null`; `null` → `default`.

### 2.4 Tokens added to `tokens.css`

```
--ctl-h: 30px;  --ctl-h-sm: 24px;  --header-h: 52px;
--sidebar-w: 264px;  --rail-w: 320px;  --chat-w: 760px;
--fs-ui: 13px;  --fs-chat: 14px;
--free-bg: #0e2a1e (light #dff2e7);
--shadow-pop: 0 12px 32px rgba(0,0,0,.45) (light 0 12px 32px rgba(20,30,50,.16));
--add-bg: rgba(87,185,138,.10) (light rgba(30,122,78,.10));
--del-bg: rgba(255,107,84,.10) (light rgba(179,48,28,.08));
```

### 2.5 Primitives (`base.css`)

Every component uses these. A worker does not restyle a primitive in an area file.

| Class | Look |
| --- | --- |
| bare `button` (`:where(button)`) | 30 px, padding 0 12px, `--raised` fill, 1 px `--border`, radius `--r-ctl`, Plex Sans 500 12.5 px, inline-flex, gap 6 px. Hover: border `--dim`. Focus-visible: 2 px `--act` outline, offset 2 px. Disabled: opacity .45. |
| `.fd-primary` | `--act` fill, `--act-ink` text, no border, 600. Disabled: `--raised` fill, `--faint` text, `--border` border, opacity 1 (never a dim amber). |
| `.fd-ghost` | Transparent, no border, `--dim` text; hover `--raised` fill, `--text`. |
| `.fd-danger` | `--hazard` text; hover `--hazard-bg` fill and `--hazard` border. |
| `.fd-sm` | 24 px, padding 0 8px, 11.5 px. |
| `.fd-iconbtn` | Square (30 or 24 with `.fd-sm`), ghost look, icon centered. |
| inputs, selects (`:where(...)`) | 30 px, `--code` fill, 1 px `--border`, radius `--r-ctl`, 13 px; focus border `--act`, no outline; placeholder `--faint`. Textarea: auto height, padding 8px 10px. |
| `.fd-field` | A wrapper with a leading icon inside the input (`.fd-field > .fd-icon` absolute left 9 px; input padding-left 30 px) and an optional trailing `.fd-kbd`. |
| `.fd-chip` | Filter toggle (v1): mono 11 px, padding 3px 11px, pill, transparent; `[aria-pressed="true"]`: `--border` border, `--border-soft` fill, `--text`. |
| `.fd-badge` | Mono 10.5 px, padding 1px 7px, radius `--r-chip`, `--border-soft` fill, `--dim` text, nowrap. `data-tone`: `act`, `hazard`, `ok`, `info` (comet), `plan` (quill), `muted`. `data-family`: `opus`, `sonnet`, `haiku`, `fable`, `quill`, `comet` (v1 model hues). `data-provider`: `claude`, `codex`, `unknown`. `.fd-caps`: capitals, 10 px, letter-spacing 1 px. |
| `.fd-dot` | 8 px status dot (v1 `.fd-pulse`). `data-status`: `working` (`--ok`, pulse 2.4 s), `verifying` (`--m-comet`, pulse 3.2 s), `needs_you` (`--act`, pulse 1.6 s), `queued` (`--dim`), `idle` (`--faint`), `ended` and `lost` (hollow `--faint`), `unreachable` (hollow `--hazard`). |
| `.fd-label` | Mono 10.5 px capitals, letter-spacing 1.8 px, `--faint`, 500. |
| `.fd-kbd` | v1 key hint: mono 10 px, `--faint`, 1 px `--border`, radius 3 px, padding 0 4px. |
| `.fd-note`, `.fd-empty`, `.fd-count`, `.fd-vh` | Dim small text; faint text; mono faint count; visually hidden. |
| `.fd-menu` | A `<details>` popover. The summary looks like a button (no marker). `.fd-menu-body`: absolute, right 0, 4 px under the summary, min-width 220 px, `--surface`, 1 px `--border`, radius `--r-panel`, `--shadow-pop`, padding 4 px, column. Its buttons are full-width ghost rows, left-aligned. `.fd-menu-sep`: a 1 px `--border-soft` line. `.fd-menu-text`: 12 px `--dim` text, padding 6px 10px. |
| `.fd-panel` | `--surface`, 1 px `--border`, radius `--r-panel`. |
| `.fd-code` | Mono 11.5 px, `--code` fill, 1 px `--border-soft`, radius 6 px, padding 8px 10px, `white-space: pre`, overflow auto. |
| `.fd-diff` + `.fd-dl[data-kind]` | Diff lines: `file` (`--surface` fill, `--text`, 600), `meta` (`--faint`), `hunk` (`--m-comet`), `add` (`--ok` on `--add-bg`), `del` (`--hazard` on `--del-bg`), `ctx` (`--dim`). |
| `@media (pointer: coarse)` | Buttons, summaries, inputs, selects: min-height `--tap-min` (44 px); `.fd-iconbtn`: min-width 44 px. |

## 3. App frame and sidebar (wave 1, worker A1)

### 3.1 Desktop (≥ 1024 px)

```
┌ sidebar 264 ─────────────────┬ route ──────────────────────────────────────────┐
│ ⚡ FLEET DECK        ● LIVE   │                                                 │
│ [+ New session          N ]  │                                                 │
│ [🔍 Search sessions     / ]  │                                                 │
│ [▲ 1 needs you]              │                                                 │
│ ─────────────────────────────│                                                 │
│ ALPHA ●                    2 │                                                 │
│ ● Review the repo        2m  │                                                 │
│   Sonnet 4.5 · ⎇ main        │                                                 │
│ ● Write the docs         5m  │                                                 │
│ BETA ●                     1 │                                                 │
│ ○ Fix the build          1h  │                                                 │
│ ▸ Ended · 3                  │                                                 │
│ ─────────────────────────────│                                                 │
│ ▦ Fleet                      ├─────────────────────────────────────────────────┤
│ ≡ Plans                      │ terminal dock (only with tabs)                  │
│ ▤ Stations               2   │                                                 │
│ >_ Terminals             1   │                                                 │
│ ⎋ Log out                    │                                                 │
└──────────────────────────────┴─────────────────────────────────────────────────┘
```

- Sidebar: `--surface`, right border `--border-soft`, width `--sidebar-w`.
- Brand row (48 px): `Zap` in `--act` + `FLEET DECK` wordmark; the connection pill at the right (v1 `.fd-wspill`): `live` → `--ok` "LIVE" with a pulsing dot; `stale` → `--act` "STALE"; no chrome → `--hazard` "OFFLINE". The pill's `title` is `chrome.label`.
- "New session": a full-width button with `Plus`, the text "New session" and a `.fd-kbd` "N" at the right.
- Search: `.fd-field` with `Search`, placeholder "Search sessions", `.fd-kbd` "/".
- Needs-you chip (only when at least one session needs you): v1 `.fd-needschip` look (amber text, `--act-bg` fill, 1 px `--act` border, mono 11 px 600) with `TriangleAlert` and "1 needs you" / "N need you"; it is a link to the first needs-you session.
- No separate "Needs you" group in the desktop and tablet sidebar. It stays on the phone list.
- Station group head: `.fd-label` station name, a 6 px reach dot (`--ok` reachable, `--hazard` not), the count at the right.
- Row (`a.fd-srow`): padding 6px 10px, radius 7 px, grid `dot | text | age`. Line 1: `.fd-dot` + title (13 px 500, ellipsis) + age (mono 10.5 px `--faint`). Line 2 (under the title): mono 11 px `--faint`: `shortModel` · `⎇ branch` (or the workspace basename when there is no branch), plus an "In terminal" `.fd-badge[data-tone=act]` when held. Hover: `--raised` fill. Current (`aria-current="page"`): `--raised` fill and inset 1 px `--border` ring.
- Footer: nav rows (30 px, icon + label, count `.fd-count` at the right): Fleet, Plans, Stations, Terminals (only with tabs). Then "Log out" as `.fd-ghost .fd-sm` with `LogOut`.

### 3.2 Tablet (768–1023 px)

Top bar (48 px, `--surface`, bottom border): `PanelLeft` icon button "Sessions" (opens the drawer), the brand, the pill. The drawer is the sidebar, 300 px, sliding from the left (v1 drawer motion, 220 ms), over a `rgba(0,0,0,.45)` backdrop.

### 3.3 Phone (< 768 px)

- Home = the session list: brand row + `Plus` icon button "New session"; search; the "Needs you" group first; station groups.
- Bottom bar: 56 px + safe-area inset, `--surface`, top border; four items, icon over an 11 px label: Sessions (`MessagesSquare`), Fleet (`LayoutGrid`), New (`Plus`), More (`Ellipsis`). Current item `--act`, others `--faint`.
- Terminals screen top bar: `ChevronLeft` back + "Terminals".

### 3.4 Notices, palette, help, screens

- Notices: a full-width strip under the frame top, 12.5 px. `info`: `--raised` fill, `--dim` text. `error`: `--hazard-bg` fill, `--hazard` text, bottom border `--hazard`.
- Palette and help: the v1 compose modal shell: top 12vh, width 560 px (max 92vw), `--surface`, radius 12 px, `--shadow-pop`, backdrop `rgba(0,0,0,.5)`. Palette input: 40 px, no border, 15 px; result rows 34 px (title + mono faint meta); the active row `--raised`. Help: key rows with `.fd-kbd`.
- Screens (New session, Plans, Stations, More): content max-width 880 px, centered, padding 24px 24px 48px; screen title 18 px 600; each section a `.fd-panel` with padding 16 px.

### 3.5 Refused controls, and the Esc and overlay order (R7.3, R7.4; DL-163)

**Refused controls.** A control has one of three states:

- The provider does not support the action: hide the control. Example: the model picker of a provider that reports no models.
- A permission or a state blocks the action: show the control, disable it, and show the reason in text near the control (not only in a tooltip). Example: "Start a session" without the `drive` scope.
- The action is allowed: show it enabled.

Never leave a control that looks active and then fails with a silent error.

**Overlays.** An overlay is a dialog (`role="dialog"`, `aria-modal="true"`), a menu or the sessions drawer. From top to bottom the order is: dialogs, menus, the palette, the help, the drawer (the z-index order; the global `Esc` handler closes them in this order).

- `Esc` closes the topmost overlay only. A dialog, a menu and the drawer handle `Esc` themselves when the focus is inside; the global handler closes the next one in the order when the focus is outside.
- Every overlay component calls `useOverlay()` (`web/src/app/overlay.ts`). While one is mounted, the board hotkeys (`j k / n f c t ?`) are off. `Mod+K` and `Esc` still work.
- A test (`web/src/app/feedback.test.tsx`) reads the sources: each file with an `aria-modal="true"` dialog must call `useOverlay()`.

### 3.6 Secondary pages and forms (DL-171)

Every page that is not the fleet home, a session or the spawn form uses `Page` (`web/src/ui/page.tsx`). Worker briefs copy from this section.

**Page.** `<section class="fd-page">`, centered, `max-width` 1040 px (1280 px for `wide`: tables, the wall, Activity), padding 24px 32px (16px on a phone). The header row holds the title (`h1`, Plex Sans 600, 20 px), an optional `.fd-count`, and the page actions at the right (gap 8 px). The description is 13 px `--dim`, at most 80ch, 4 px under the header and 20 px above the content.

```
┌ Projects  3                                   [+ New project] [↻ Refresh] ┐
│ A project is a repository on a station with its forge and its trackers.   │
│ ┌ table ──────────────────────────────────────────────────────────────┐   │
│ │ NAME     STATION   FOLDER          FORGE        TRACKERS        …   │   │
```

**Section and empty state.** `Section`: a `.fd-label` row, then the content in a `.fd-panel` (padding 16 px). `EmptyState`: a `.fd-panel`, centered, padding 40px 24px: a 24 px icon in `--faint`, the title (14 px 600), a sentence (13 px `--dim`, at most 60ch) and one action. Every empty list uses it.

**Primary action.** The main action of a page is `.fd-primary` in the page header ("New project", "New automation", "New share", "New space", "Add station"). While its form is open the button is hidden. It is never dimmed. "Refresh" is `.fd-ghost` with the `RefreshCw` icon.

**Form.** The form sits in a `.fd-panel` (padding 16 px). `FormGrid` has 2 columns of 1fr from 900 px, gap 16px 20px, else 1 column. `Field`: the label is 12 px 500 `--dim`, 6 px above the control; the hint is 12 px `--faint` under the control; the error is 12 px `--hazard` and replaces the hint. A control keeps the width of its cell; a field that needs the row has `span={2}`. Blocks of one form (What, Where, When, Safety; Forge; Issue trackers) are a `.fd-formsec`: a `.fd-label` and a top border `--border-soft`. A rare field goes into a collapsed "Advanced" `<details>`. `FormActions`: a top border `--border-soft`, padding-top 12 px, aligned right: Cancel (`.fd-ghost`), then the primary submit (`.fd-primary`). A refusal of the deck shows at the left of that bar. The submit button is not disabled for an invalid draft: the submit try shows the errors.

**Errors.** A field shows its error only after it was touched (the focus left it) or after a submit try. A fresh form shows none.

**Tables.** A list of records is a table in a `.fd-panel` (`.fd-table`): head cells are `.fd-label` style, rows are 10px 12px with a `--border-soft` line, hover `--raised`; paths are mono 12 px `--dim`; the action cell is right-aligned with `.fd-sm` buttons. A destructive action asks inline ("Delete this project?" Delete, Keep). A dim row (revoked, expired) has opacity .6.

**Links.** `a` is `--m-comet`, no underline, underline on hover and focus-visible. A visited link and a link in `.fd-note` keep the color. The rule has zero specificity: a class of a card, a row or a nav item sets its own look.

**Folder picker.** `FolderPicker` is a text input and "Browse…" (and a pin button when there is a path). The panel under the input has the breadcrumbs (Roots, then each part of the path), the folders (a git icon and the text "git repository" for a repository), pinned and recent folders, "Use this folder" and "Close". Typing a path works. The spawn, Projects and Automations forms use it.

**Help.** The help dialog opens only on `?`, from the palette or from the Fleet home. A first visit shows the notice strip "Press ? for the keys and a short guide." with a dismiss button (key `fd-help-seen`).

**Badges.** Only the mode badge of a session uses `data-tone="hazard"`, and only for `dontAsk`, `bypassPermissions` and `danger-full-access`; `plan` has the `plan` tone, other modes have no tone. A model badge uses `data-family`, a provider badge `data-provider`. Amber stays for "yours to act".

## 4. Fleet home (wave 1, worker A2)

### 4.1 Desktop and tablet

```
┌ header 52 ────────────────────────────────────────────────────────────┬ rail 320 ───────────┐
│ FLEET   3 sessions · 1 needs you · 2 stations        [🔍 Filter…    ] │ NEEDS YOU (1)    🔔 │
├ ▲ CONFLICT src/parse.ts — Review the repo × Write the docs ───────────┤ ┌─────────────────┐ │
├ STATION (all) Alpha Beta    PROVIDER (all) Claude Codex               │ │Review the repo  │ │
│ QUEUED 0    WORKING 2    VERIFYING 0    IDLE 1    OFFLINE 0           │ │PERMISSION Alpha │ │
│ Alpha · 2 sessions ───────────────────────────────────────────────── │ │Bash             │ │
│             ┌card────────┐                                            │ │┌rm -rf build──┐ │ │
│             │● Review…  S│                                            │ │└──────────────┘ │ │
│             │⎇ main      │                                            │ │[ Allow ][ Deny ]│ │
│             │▲ Bash      │                                            │ └─────────────────┘ │
│             └────────────┘                                            │                     │
│ Beta · 1 session ─────────────────────────────────────────────────── │                     │
├ [Activity] [Mail & tasks] [Assign] [Usage]                        [⌄] │                     │
│ 22:56:53 ask   Review the repo  waits for Bash                        │                     │
└───────────────────────────────────────────────────────────────────────┴─────────────────────┘
```

- Header (`--header-h`, `--surface`, bottom border): `FLEET` wordmark, the fleet line (mono 12 px `--dim`), a `.fd-field` search (220 px, placeholder "Filter sessions").
- Conflict strip (v1 `.fd-confstrip`), only when the radar has conflicts: `--hazard-bg`, bottom border `--hazard`, mono 12 px: `TriangleAlert` "CONFLICT", the file, the session names as link buttons joined by "×". More conflicts: one strip line each, at most 3, then "+N more".
- Filter row: `.fd-label` "STATION" + `.fd-chip`s (All + one per station); `.fd-label` "PROVIDER" + chips (All, Claude, Codex). When a filter is on: "N of M" mono faint at the right.
- Columns (v1): QUEUED (`queued`), WORKING (`working` and `needs_you`; needs-you first), VERIFYING (`verifying`), IDLE (`idle`), OFFLINE (`ended`, `unreachable`, `lost`). Sticky column heads (`.fd-label` + count). Five columns, `minmax(170px, 1fr)`, gap 10 px, min-width 900 px (horizontal scroll below). An empty cell is empty space.
- Swimlane per station: head = station name (mono 11.5 px 600 `--dim`) + "2 sessions" `.fd-count` + a `--border-soft` rule.
- Card (`a.fd-card`, the whole card is the link to the session): v1 card: `--surface`, 1 px `--border`, left border 3 px transparent, radius `--r-card`, padding 10px 12px, gap 6 px. Hover border `--dim`. `needs_you`: left border `--act`. OFFLINE: opacity .5.
  - Line 1: `.fd-dot` + title (13 px 600, ellipsis) + the model `.fd-badge[data-family]` (`shortModel`) at the right.
  - Line 2: mono 11 px `--faint`: `GitBranch` + branch, or `Folder` + workspace basename.
  - Line 3 (12.5 px, at most 2 lines): an open request → `--act` "▲ " + its title; else a running tool → mono `title: input_summary`; else `idle` → `--faint` "Waiting for a prompt"; `ended` → "Ended"; `unreachable` → `--hazard` "Station unreachable"; `lost` → `--hazard` "Lost".
  - Line 4 (only when something to show), chips + the age at the right (mono 10.5 px `--faint`): subagents `Workflow` N, background `Layers` N, "In terminal" act badge, a cost badge when `cost_usd` is known (`$0.12`), "Stale" muted badge when the board is stale.
- Rail (≥ 1280 px: right column, `--rail-w`, left border `--act-bg`): head "NEEDS YOU" (mono 11 px, letter-spacing 2 px, `--act`, 600) + count pill (`--act` fill, `--act-ink`); a `Bell`/`BellOff` icon button for notifications at the right (title = the state text). Items (v1 `.fd-q`): line 1: session title link (mono 11.5 px 600) + kind badge caps (`PERMISSION` act, `QUESTION` info) + station badge + age; the request title (13.5 px 600); the details as `.fd-code` (max-height 130 px); the answer row. Empty: the v1 all-clear (44 px ring with `Check` in `--ok`, "ALL CLEAR", "Nothing waits for you.").
- Answer from the rail: when the session's actions allow approvals and the request is not a question form, one button per option, the first `.fd-primary`, all `flex: 1`. While sending: the buttons are disabled; a failed outcome shows its text in `--hazard` under the buttons. A question form, or no approve scope: one `.fd-sm` button "Open session" with `ChevronRight`.
- Below 1280 px: the rail moves under the header and filters, full width, cards in a grid `repeat(auto-fill, minmax(280px, 1fr))`; it renders only when something waits.
- Bottom panel (v1 feed): 200 px, `--surface`, top border; tabs (`.fd-chip` look, role tablist): Activity, Mail & tasks, Assign (only with assign), Usage; a `ChevronDown`/`ChevronUp` icon button collapses it to its 36 px tab row. The open state and the tab persist in `localStorage` (`fd-home-panel`), with try/catch.
  - Activity (v1 feed lines): mono 11.5 px, line-height 1.5: time `--faint`, kind tag (52 px wide; `ask` `--act`, `error` `--hazard`, `revert` `--m-comet`, `turn` and `tool` `--dim`, `join`, `end` and `rename` `--faint`), the session name (link button, `--dim`, hover `--text`), the text (`--dim`, ellipsis). Filter chips in the panel head, small.
  - Usage: token and cost totals as a mono stat row; rate meters as 6 px bars (`--ok` < 80 %, `--act` ≥ 80 %) with the meter text.

### 4.2 Phone

The fleet screen: header (wordmark + fleet line), filters as one horizontal scroll row of chips, needs-you items (rail items, full width), then one section per non-empty column (`.fd-label` head + count) with full-width cards (the station as a badge on line 2), then the bottom panel as a normal block.

## 5. Session (wave 1, worker B)

### 5.1 Header

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ● Review the repo ✎  [needs you] [Sonnet 4.5] [Claude]   [■ Interrupt][▣][⋯] │
│   Alpha · ⎇ main · /work/repo                                                │
├ Chat   Changes 1   Files ─────────────────────────────────────────────────────┤
```

- Row: flex, no wrap; padding 10px 16px; bottom border `--border-soft`. Left block (flex 1, min-width 0): line 1 `.fd-dot` + title (15 px 600, ellipsis) + `Pencil` `.fd-iconbtn .fd-sm` "Rename" + status badge (`needs you` act, `working` ok, `verifying` info, others muted) + model badge (family) + provider badge; line 2 the place, mono 11 px `--faint`.
- Actions (flex none): Interrupt (`Square` + "Interrupt") while it can interrupt; Resume (`.fd-primary`, `Play` + "Resume") while it can resume; when held: "In terminal" act badge + "Return to board control" (`Undo2`); else "Open in terminal": a `.fd-menu` whose summary is `SquareTerminal` + "Open in terminal" (icon only below 1280 px, `aria-label` kept) and whose body is the confirm text with `.fd-primary` "Open in terminal" and "Cancel".
- One overflow `.fd-menu` (`Ellipsis`, `aria-label` "Session menu"): Rename…, Stop, Force stop… (`.fd-danger`; confirm inside the menu), separator, mode ladder, model picker, "Continue after a station restart" checkbox, separator, "Open shell in workspace" (when given). The old "Stop" and "⚙" menus go away.
- The outcome strip under the header: 12.5 px, padding 6px 16px; ok `--dim` with `Check`, failure `--hazard-bg`/`--hazard`; an `X` icon button "Dismiss".
- "Controls are off…" and the hold notice: one strip each under the header (`--act-bg` fill, bottom border `--act`, 12.5 px, `.fd-sm` buttons).
- Tabs row: 36 px, padding 0 16px, bottom border; tabs are text buttons 13 px `--dim`; selected `--text` with a 2 px `--act` underline; counts `.fd-count`.
- Phone: one row (48 px): `ChevronLeft` back, `.fd-dot`, title (ellipsis), then Interrupt as an icon button (when it can), Resume as an icon button (when it can), and the overflow menu. On the phone, "Open in terminal" / "Return to board control" are items of the overflow menu, and the badges and the place show as the first `.fd-menu-text` of the menu.

### 5.2 Chat

- Column: max-width `--chat-w`, centered, padding 24px 24px 16px (phone 16px 12px), gap 14 px.
- User prompt: right-aligned bubble, `--raised` fill, 1 px `--border`, radius 14px 14px 4px 14px, padding 8px 12px, 14 px, max-width 85 %. The run label shows only when it is not "You" (mono 10.5 px `--faint` above the bubble).
- Assistant message: Markdown, 14 px, line-height 1.6. Headings 17/15/14 px 600. Inline code: mono 12.5 px, `--code` fill, 1 px `--border-soft`, radius 4 px, padding 1px 5px. Links `--m-comet`. Blockquote: 3 px `--border` left border, `--dim`. Tables: wrapper with horizontal scroll; 13 px; head cells `--surface`; cell padding 6px 10px; `--border-soft` lines.
- Code block: a `.fd-panel`-like box with `--code` fill and radius 8 px; a 28 px head row: the language (from `language-xxx`, mono 10.5 px `--faint`) at the left, a `Copy`/`Check` `.fd-iconbtn .fd-sm` ("Copy code" / "Copied") at the right; the `pre` under it, padding 10px 12px, mono 12.5 px, horizontal scroll. The head is never over the code.
- Thinking: a `<details>`: summary mono 11.5 px `--faint` with a chevron ("Thinking", streaming "Thinking…" with a pulse); body 13 px `--dim`, `pre-wrap`, 2 px `--border-soft` left border, padding-left 12 px.
- Tool row (v1 timeline density): a `<details>`; summary 28 px, radius 6 px, hover `--surface`: status mark (running `.fd-dot[data-status=working]`, ok `Check` `--ok`, failed `X` `--hazard`, denied `Ban` `--act`, ended `--faint` "–"), the kind icon (§2.2, `--faint`), the title (mono 12 px 500), the first line of the input (mono 12 px `--dim`, ellipsis, flex 1), file badges (at most 3, then "+N"), an exit badge (`hazard`) when the exit code is not 0, a `ChevronRight` that turns 90° when open. Body: indent 22 px; the input and the output as `.fd-code` (max-height 320 px).
- Tool fold (replaces the old tool group): a run of 3 or more consecutive finished tool rows is one `<details>`; summary mono 11.5 px `--dim` "12 tool calls · 7 reads, 3 commands, 2 MCP" (kinds in the order reads, edits, commands, MCP, other) with a chevron; body indented with a 1 px `--border-soft` left border. A failure adds a `hazard` badge "N failed" and the fold starts open. A running tool is never folded; it stays a row after the fold.
- Subagent: `.fd-panel`, padding 10px 12px; head `Workflow` + title (13 px 600) + status badge; the task 12.5 px `--dim` (2 lines when closed); nested items with a left border.
- Approval in the chat: open → one line, mono 12 px `--act`: `TriangleAlert` "Waiting for your answer · {title}". Resolved → one line `--faint`: `{title} · {outcome}`. The controls are only in the dock.
- Plan: `.fd-panel`, padding 12px 14px; `.fd-label` "PLAN" in `--m-quill`; Markdown.
- Turn diff line: `GitCompare` + "2 files changed" + "+3" (`--ok`) "−1" (`--hazard`), mono 11.5 px, and a `.fd-ghost .fd-sm` "Review changes".
- Working: 12 px `--faint` "Working" with three dots. Jump to latest: a floating pill (`.fd-sm`, `--raised`, `--shadow-pop`, `ArrowDown`) centered over the bottom of the scroll area.

#### Chat filters (DL-175)

- A filter bar above the scroll area (it does not scroll with the chat): one row of `.fd-chip` toggles in a `role="group"` "Chat filters", `aria-pressed`, padding 6px 24px (phone 6px 12px), bottom border `--border-soft`, the same width as the chat column. On a phone the row scrolls sideways. It shows when the chat has at least one item.
- Chips: Reads, Edits, Commands, MCP, Other tools, Thinking, Notices; a separator; "Compact tools". A hidden kind that has rows shows its count on the chip ("Reads 12", `--faint`).
- Reads: Read, Glob, Grep, LS and the file view tools (tool kind read or search). Edits: Write, Edit, MultiEdit, NotebookEdit, apply patch. Commands: Bash and the Codex shell. MCP: names that start with `mcp__` and the Codex MCP calls. Other tools: the rest. Notices: info notices; error notices always show.
- Always shown: agent and user messages, approvals and questions, errors, plans, turn diff lines, artifact chips. The filter also applies inside subagent cards.
- Default: every kind on, Compact tools off. The state is per browser (localStorage `fd-chat-filter`, the same for every session; a blocked storage gives the default).
- Compact tools also folds the agent's intermediate messages ("notes": every agent message of a turn but the last one, and any message that a tool row follows) into the fold, for runs of 2 or more items: "12 tool calls · 5 notes · 7 reads, 3 commands, 2 MCP". The last message of a turn never folds.
- The filters never change the stored timeline. A change keeps the bottom of the chat when the chat is at the bottom. When every row is hidden the chat says "Every row is hidden by the filters."

### 5.3 Approval dock and composer

- The dock and the composer share the chat column width. The foot has a 24 px top fade from transparent to `--bg`.
- Dock item (v1 `.fd-q`): 1 px `--act` border, `--surface`, radius `--r-panel`, padding 12px 14px, gap 9 px. Line 1: kind badge caps (`PERMISSION` act; `QUESTION` info for a question form) + the title (13.5 px 600). Details: `.fd-code` max-height 130 px, shown at once when they have at most 600 characters, else in a `<details>` "Show details". Options: the first `.fd-primary`, the others plain; min-width 96 px; `flex: 1` on the phone. The "Needs you · N" heading is visually hidden. Question forms (QuestionAnswer): options as v1 `.fd-opt` rows (number key badge, label 12.5 px 600, description `--dim`); the picked row `--act` border on `--act-bg`.
- Composer: one box, `--surface`, 1 px `--border`, radius 12 px; focus-within border `--dim`. The textarea has no border and no fill, padding 12px 14px 4px, 14 px. Bar (padding 6px 8px 8px): `Paperclip` icon button "Add image" (the file input stays hidden in the label), the note (11.5 px `--faint`, ellipsis, flex 1), then the send buttons: idle → `.fd-primary` `ArrowUp` + "Send"; working → "Steer now" (when it can steer) + `.fd-primary` "Queue follow-up". The slash menu: a `.fd-panel` popover above the box with `--shadow-pop`, max-height 240 px, rows 30 px mono 12.5 px; the active row `--raised` with a 2 px `--act` left edge.

### 5.4 Changes and Files

- Body: padding 20px 24px, max-width 1040 px.
- Each turn: a `.fd-panel` row: "Turn N" mono 600, the diff summary mono with colored +/−, then `.fd-sm` "Show the diff" and the revert controls. The diff renders with `.fd-diff` lines (§2.5), mono 12 px, line-height 1.5, horizontal scroll, max-height 70vh.
- The revert explanation: `.fd-note`.
- Files: rows mono 12 px with hover `--raised`; the viewer `.fd-code`.

## 6. Terminal dock (wave 1, worker C)

- No tabs → the dock renders nothing (no empty bar). "Open shell in workspace" moves to the session overflow menu.
- Bar: 34 px, `--surface`, top border `--border-soft`. Tab: 34 px, padding 0 6px 0 10px, mono 12 px `--dim`: kind icon (`SquareTerminal` shell, `MessageSquare` TUI), title, station (11 px `--faint`), an `X` `.fd-iconbtn .fd-sm` "Detach …" (visible on hover, focus and the selected tab). Selected tab: `--code` fill (it joins the terminal), `--text`, 2 px `--act` top edge.
- Tools at the right: `Plus` icon button "Open shell in workspace" (when given); for a TUI tab `.fd-sm` `Undo2` "Return to board control"; a `.fd-menu` (`Ellipsis`, "Terminal menu") with "Close shell" (`.fd-danger`, shell tabs only; the confirm is inside the menu); a `ChevronDown`/`ChevronUp` icon button "Hide terminals"/"Show terminals".
- The resize handle: 6 px hit area; a 2 px `--act` line on hover and focus.
- Terminal body: `--code` background, padding 6px 8px. xterm theme: background `#0a0d13`, foreground `#e7ecf5`, cursor `#f0a63c`, selection `rgba(240,166,60,.30)`.
- Phone (full screen): the tab bar at the top (44 px); the key bar at the bottom: keys 36 px high, mono 12 px, `--raised`, radius `--r-ctl`.

## 7. Out of scope

No new routes, no behavior change of the session controller, no protocol change, no new keyboard shortcuts. Tests are updated once, after the visual work (wave 2).

## 8. Design pass 2026-10 (ui-pass.md)

Wave 0 of the board design "Flight strips" (`design/ui-pass.md`). These rules replace the older rules in §1 and §2 where they differ.

- No tracked capitals and no `text-transform: uppercase`. Write labels and headings in sentence case. Capitals stay only for acronyms (MCP, URL, ID, PR).
- Mono only for data: paths, ids, callsigns in tables, models, times, costs, commands, code, diffs, `.fd-badge`, `.fd-kbd`, `.fd-code`. Headings, group and station headers, tabs, chips, empty states and sentences use Plex Sans.
- `--faint` is text-safe (at least 4.5:1 on `--bg` and `--surface`). `--faint-decor` is for decoration only (hollow dots, rules), never text.
- `--font-callsign` (IBM Plex Sans Condensed 500 and 600) is for callsigns, strip titles and the wordmark.
- The mark is `ui/Mark.tsx`: a flight strip with an amber state edge. The wordmark is "Fleet Deck" in `--font-callsign`.
- Labels (`.fd-label`): sentence case, Plex Sans 500, 12 px, `--dim`.
- Type scale tokens: `--fs-12`, `--fs-13`, `--fs-15`, `--fs-18`, `--fs-24`. `--ls-label` is 0.
