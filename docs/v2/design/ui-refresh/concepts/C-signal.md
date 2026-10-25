# Direction C: "Signal" (folder `c/`)

Read `BRIEF.md` first. This file is the visual system of direction C. Follow it exactly; where it is silent, choose the quieter option.

## Idea

Daylight operations. Bright, legible outdoors on a phone, friendly but exact. The device of C is **station identity color**: every station owns one hue, so the owner sees *where* an agent runs before reading a word (Lighting is cobalt, dev1 is teal, mac-m4 is violet). The layout is also different from the other directions: a **top bar** instead of a sidebar, a **hero** that states what needs you in one big line, and the stations as **side-by-side lanes** of session tickets. Display type with character (Bricolage Grotesque) gives the board a voice.

## Color

Primary mode: **light**. Alt mode: dark.

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `--canvas` | `#F3F5F9` | `#0D1220` | page canvas |
| `--surface` | `#FFFFFF` | `#141B2D` | tickets, panels, top bar |
| `--raised` | `#F8F9FC` | `#1A2338` | inputs, hover, chips |
| `--line` | `#E3E7EF` | `#263049` | hairlines |
| `--ink` | `#0F1729` | `#EEF2FA` | primary text |
| `--ink-2` | `#4B5568` | `#A8B3C7` | secondary text |
| `--ink-3` | `#687387` | `#8090A8` | tertiary text (still 4.5:1) |
| `--sun` | `#FFB000` fill / `#8F5A00` text | `#FFC233` | **needs you** and the decision primary (Allow). Text on it: `#231800` |
| `--go` | `#12A150` | `#3DD68C` | working, ok, additions |
| `--stop` | `#E5383B` | `#FF6B6E` | hazard, deny, deletions, offline, bypass |
| `--st-lighting` | `#2F5BFF` | `#6E8BFF` | station Lighting |
| `--st-dev1` | `#0E9F8E` | `#2CC9B5` | station dev1 |
| `--st-mac` | `#8B5CF6` | `#B197FC` | station mac-m4 |

Each station hue also has a tint for fills: 10 % of the hue over `--surface` (light) or 16 % (dark), for example `color-mix(in srgb, var(--st-lighting) 10%, var(--surface))`.

Rules:

- A station hue marks only the station: its lane header, the ticket's station tab, the station chip. Never use it for status.
- `--sun` is only for needs you and the decision primary. General primary buttons (New session, Start session): `--ink` fill with `--surface` text, fully rounded (pill).
- Focus ring: 2 px `--ink` outline with a 2 px offset (neutral, so it never looks like a station).
- Shadows only on things that float or that you can pick up: tickets get `0 1px 2px rgba(15,23,41,.06), 0 4px 12px rgba(15,23,41,.05)` (dark: none, use `--line` borders). Menus, the approval card and the composer get `0 16px 40px rgba(15,23,41,.14)` (dark: `0 16px 40px rgba(0,0,0,.5)`).
- No gradient washes. Flat color only.

## Type

- **Bricolage Grotesque** for display: page titles, the hero line, lane headers (station names), callsigns, big numbers. Use weight 700 and the width axis at 90 for titles. **Instrument Sans** for all interface and prose text. **JetBrains Mono** only for code, paths, branches and commands. Link: `https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wdth,wght@12..96,75..100,400..800&family=Instrument+Sans:wght@400..700&family=JetBrains+Mono:wght@400;500&display=swap`.
- Scale (desktop): 12.5 px meta · 14 px UI · 15 px chat prose (line-height 1.6) · 18 px lane header · 24 px page title · 36 px hero line (Bricolage, line-height 1.1, letter-spacing −0.02em). Phone: body 15 px, hero 28 px.
- Sentence case. No all-caps.

## Shape and space

- 4 px grid, but airy: 24 px gutters on the desktop.
- Radius hierarchy (each level is different on purpose): 999 px for chips, pills and buttons · 12 px for inputs · 16 px for tickets · 22 px for panels and the composer.
- Desktop control height 34 px; phone 48 px.

## The ticket (the session card)

```
╭──────────────────────────────────────╮
│ ▀▀▀▀ (4 px station-hue tab, 48 px)   │
│ otter-n1rr                 ● Needs you│
│ Fix flaky shim test                   │
│ ▲ Bash  cargo nextest run -p fleet…   │
│ ⎇ fleet-sync   Sonnet 5.5     +9 −3   │
│ [Allow  a] [Deny  d]                  │
╰──────────────────────────────────────╯
```

- A 4 px × 48 px rounded tab in the station hue at the top left edge.
- Callsign in Bricolage 15 px weight 700; the status as a pill at the right: a dot + word ("Working" in `--go`, "Needs you" on a `--sun` fill, "Idle" in `--ink-3`).
- The title in Instrument Sans 15 px weight 600; the "now" line in `--ink-2` with the tool icon; a footer row of chips (branch in mono, model, PR #128, "2 subagents") and the changes at the right.
- A needs-you ticket shows its action buttons inside the ticket and has a 2 px `--sun` outline.

## Layout

### Fleet home (desktop)

```
┌ top bar 64 ──────────────────────────────────────────────────────────────────────────────────┐
│ (FD) Fleet Deck   Fleet  Watch  Activity  Stations  More ▾      [⌕ Search ⌘K]  [+ New session] ● (LM)│
├──────────────────────────────────────────────────────────────────────────────────────────────┤
│  2 sessions need you.                                   Claude 5 h ◔ 62 %  week ◔ 31 %  Codex ◔ 18 %│
│  ╭ otter-n1rr · Bash ───────────────╮ ╭ kite-x81d · Question ──────────╮   ⚠ tokens.css: falcon × heron│
│  │ cargo nextest run …  [Allow][Deny]│ │ Test key or live key? [Test][Live]│                          │
│  ╰───────────────────────────────────╯ ╰────────────────────────────────╯                          │
│                                                                                              │
│  ● Lighting  3 of 12          ● dev1  2 of 12              ● mac-m4  2 of 12                 │
│  ╭ ticket ╮                   ╭ ticket ╮                   ╭ ticket ╮                        │
│  ╭ ticket ╮                   ╭ ticket ╮                   ╰ 1 ended ›                       │
│  ╭ ticket ╮                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

- Top bar (64 px, `--surface`, bottom hairline): the logo (a 32 px `--ink` circle with "FD" in Bricolage 800 `--surface`), "Fleet Deck" in Bricolage 17 px; nav links as pills (the active one: `--raised` fill + `--ink` text; others `--ink-2`): Fleet, Watch, Activity, Stations, More (a menu with Plans, Automations, Artifacts, Projects, Files, Shares, Terminals, Settings). At the right: a search pill (⌘K), the **New session** pill button (solid ink, `plus` icon, "N" hint), the Live dot, the avatar "LM".
- Hero row: the line "2 sessions need you." in Bricolage 36 px (when nothing needs you it would say "All clear."), under it the two **request cards** side by side (each 1/3 of the width, `--surface`, 22 px radius, 2 px `--sun` outline, the station tab, the request, the buttons with kbd hints). At the right of the hero: the **usage gauges** as three small ring gauges (40 px rings, `--ink` arc on `--line` track, the percent in the middle, the label under) and a radar line ("`tokens.css`: falcon-vf9n and heron-mc2q", with `triangle-alert` in `--stop`).
- Lanes: three equal columns, one per station. A lane header: a 10 px dot in the station hue, the station name in Bricolage 18 px, "Linux · x86_64" in `--ink-3`, "3 of 12" with a thin bar in the station hue. Under it the tickets, 12 px apart. The ended session is a quiet row "1 ended" with `chevron-right` at the bottom of the mac-m4 lane.
- Activity, Mail & tasks, Assign: a small floating pill group at the bottom right of the page ("Activity", "Mail 1", "Assign"), `--surface`, the floating shadow.

### Session (desktop)

Top bar as on the fleet home (no nav item active; a breadcrumb "Fleet / Lighting / otter-n1rr" under the bar). Two columns under the session header:

- Session header (in the page, not a bar): the station tab (cobalt), the callsign `otter-n1rr` in Bricolage 15 px, the title "Fix flaky shim test" in Bricolage 28 px, the status pill "Needs you" on `--sun`, then chips: Lighting (with a cobalt dot), branch `fleet-sync` (mono), the folder (mono), Sonnet 5.5, Claude, "Accept edits". Actions at the right: Interrupt, Terminal, `folder-open`, `ellipsis` (pill buttons).
- Tabs: pills (Chat active with `--ink` fill and `--surface` text; Changes 2, Files, Artifacts).
- Left column (chat, 720 px): the user message in a `--raised` bubble with 22 px radius at the right; agent prose plain; tool calls as compact rows in a `--surface` "activity" card with 16 px radius (one row per tool: icon in a 24 px `--raised` circle, tool name, mono target, result at the right); the code block (`--raised`, 16 px radius, language label + copy); the table (hairline rows). The approval card (22 px radius, 2 px `--sun` outline, the floating shadow; "Permission" pill + "Bash", the command, the reason, buttons Allow (sun pill, `a`), Allow for this session, Deny (`d`)). The composer: a 22 px-radius `--surface` box with the floating shadow, the textarea, and a row: `paperclip`, a model chip, **Steer now** (ghost pill), **Queue follow-up** (ink pill, ↵).
- Right column (the rest, at least 420 px): a **live changes preview**: a `--surface` panel with 22 px radius: header "Changes" + "+9 −3 in 1 file", the file name `crates/fleetdeck-station/src/shim/reaper.rs` (mono), and the diff of the edit (about 14 lines, green and red line backgrounds as 10 % tints of `--go` and `--stop`, mono 12.5 px), then a "Files" list with 1 entry.

### New session (desktop)

Top bar + a centered column (max 820 px):

- The display question "What should the agent do?" (Bricolage 36 px) above a large task text area (`--surface`, 22 px radius, 18 px text, 160 px tall, the floating shadow).
- **Where it runs**: three station cards in a row (16 px radius, `--surface`), each with its hue tab, the name in Bricolage 18 px, platform, "3 of 12 free" with a thin bar in its hue, an online dot. The selected card (Lighting): a 2 px outline in its own hue and a `check` circle.
- Workspace as pill choices (This directory / New worktree / Existing worktree), the directory field (mono, 12 px radius) with a Browse… pill.
- **Agent**: provider as pill choices (Claude 2.1.306 / Codex 0.163.0), model as a custom select pill ("Opus 5.5"), permission mode as pill choices (Station default / Ask / Accept edits / Plan / Bypass in `--stop` text).
- Title field and the Batch switch (a 40×24 px pill switch, the knob in `--surface`, on = `--ink` track).
- "More options" row with `chevron-down`.
- Footer row (not sticky): **Start session** (a large ink pill, 44 px, kbd ⌘↵) and Cancel (ghost).

### Phone home and phone session

- Phone home: a 56 px top row (logo, the Live dot, the avatar); the hero "2 need you." in Bricolage 28 px; the two request cards stacked (full width, 48 px buttons); then a segmented pill switch for the station (All, Lighting, dev1, mac-m4, each with its hue dot) and the tickets of all stations in one column. The bottom tab bar: a floating pill bar 12 px above the bottom edge (`--surface`, the floating shadow, 999 px radius) with Sessions, Fleet, New, More; the active item is an `--ink` pill with `--surface` icon and label.
- Phone session: the header (back, the station tab, callsign over title, the status pill), pill tabs, the end of the transcript, the approval card docked above the composer with stacked 48 px pill buttons.

## Do not

- No gradient washes, no glass, no glows.
- Do not tint the whole ticket with the station hue: only the tab, the lane dot, the chip dot and the selected outline.
- No colored model badges, no all-caps, no mono for times or counts.
