# Direction B: "Graphite" (folder `b/`)

Read `BRIEF.md` first. This file is the visual system of direction B. Follow it exactly; where it is silent, choose the quieter option.

## Idea

Quiet precision. The board steps back and the agents' work is the content. B is the closest to the modern tools that developers already use every day (an issue tracker, a command launcher): a neutral graphite UI, one signal color, list rows with **status glyphs**, a command bar, a properties panel, a "New session" modal. It is the low-risk modern option. Its distinctive device is the **status glyph set**: every status has its own shape *and* color, so the state reads without color too.

## Color

Primary mode: **dark**. Alt mode: light.

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--canvas` | `#121214` | `#FBFBFC` | page canvas (main) |
| `--side` | `#18181B` | `#F4F4F6` | sidebar, side panels |
| `--raised` | `#1E1E22` | `#FFFFFF` | rows on hover, inputs, cards, menus |
| `--line` | `#2A2A30` | `#E4E4E8` | hairlines |
| `--ink` | `#EDEDF0` | `#18181B` | primary text |
| `--ink-2` | `#A0A0AB` | `#5B5B66` | secondary text |
| `--ink-3` | `#7D7D88` | `#6E6E78` | tertiary text (still 4.5:1) |
| `--signal` | `#FFB224` | `#FFB224` fill / `#9A5B00` text | **needs you** and the decision primary (Allow). Text on it: `#1A1200` |
| `--ok` | `#4CC38A` | `#1A7F4B` | working, ok, additions |
| `--bad` | `#F2555A` | `#D1343B` | hazard, deny, deletions, offline, bypass |
| `--focus` | `#9AA5FF` | `#4353D9` | focus ring, links, selected nav item marker |

Rules:

- Only `--signal` may draw the eye. Everything else is graphite.
- General primary buttons (New session, Start session): `--ink` fill with `--canvas` text. Decision primary (Allow): `--signal` fill.
- Separation by hairlines and by the `--side`/`--canvas` step. Panels that float (modal, menus, approval card, composer): 1 px `--line` border + `box-shadow: 0 1px 0 rgba(255,255,255,.05) inset, 0 24px 64px rgba(0,0,0,.5)` (light: `0 16px 48px rgba(24,24,27,.14)`).
- Model and provider names: `--ink-2` text, never colored.

## Status glyphs (the signature)

14 px SVG glyphs, drawn inline (not Lucide):

| Status | Glyph | Color |
| --- | --- | --- |
| working | a ring with a 3/4 arc filled (a progress pie) | `--ok` |
| verifying | a ring with a 1/2 arc | `--focus` |
| needs you | a filled circle with a dark "!" | `--signal` |
| queued | a dashed ring | `--ink-2` |
| idle | an empty ring | `--ink-3` |
| ended | a filled circle with a dark check | `--ink-3` |
| unreachable, lost | a ring with a diagonal slash | `--bad` |

## Type

- **Geist** for the interface and **Geist Mono** for callsigns, paths, branches, commands, code and diffs. Link: `https://fonts.googleapis.com/css2?family=Geist:wght@300..700&family=Geist+Mono:wght@400;500;600&display=swap`.
- Scale (desktop): 12 px meta · 13 px UI and rows · 14 px chat prose (line-height 1.6) · 16 px section headings · 20 px page title (weight 600, letter-spacing −0.015em). Phone: 15 px body; the phone home title is a 28 px large title.
- Weights 400 / 500 / 600 only. Tabular numbers for counts and times.
- Sentence case. No all-caps.

## Shape and space

- 4 px grid. Radius: 6 px controls and rows, 10 px panels and cards, 14 px modal.
- Desktop control height 28 px; list rows 36 px; phone 44 px.

## Layout

### Fleet home (desktop)

```
┌ sidebar 244 ───────────────┬ main ─────────────────────────────────────────────┬ panel 300 ───────┐
│ [◆] Fleet Deck     ● Live  │ Fleet   [List | Board]          [Filter] [Display]│ Usage            │
│ [+ New session]  [⌕ ⌘K]    │───────────────────────────────────────────────────│ Claude 5 h  62 % │
│ ◉ Inbox                2   │ Needs you  2                            [ ] a  d  │ ▔▔▔▔▔▔▔▔▔▔▔      │
│ ▦ Fleet                    │ ● Fix flaky shim test  Bash cargo nextest…  [Allow][Deny]│ Claude week 31 % │
│ ◎ Watch                    │ ● Stripe webhooks  Use the test key or…  [Test][Live]    │ Codex 5 h  18 %  │
│ ≡ Activity                 │                                                   │ Today: 4.2 M in, │
│ ▤ Stations                 │ Lighting   Linux  3 of 12                       ⌄ │ 610 k out, $38.40│
│ ⌄ More                     │ ◔ falcon-vf9n  Fleet Deck UI   ✎ Write BRIEF.md  ui-refresh  +412  14m │
│                            │ ○ heron-mc2q  Omarchy docs    The docs build…  docs/omar…  +260 2h     │
│ Live                       │ dev1   Omarchy  2 of 12                           │ Conflict radar   │
│ ◔ Fleet Deck UI            │ ◔ lynx-4kq2  Rannoch checkout  ✎ Edit cart.ts  #128 2 subagents +142 −37│
│ ● Fix flaky shim test      │ mac-m4  macOS  2 of 12                            │ tokens.css       │
│ ● Stripe webhooks          │ ◔ wren-77ta  Journzy iOS build  ▸ xcodebuild…    │ falcon, heron    │
│ ◔ Rannoch checkout   …     │ ✓ 1 ended                                          │ Mail & tasks     │
│ ─────────                  │                                                   │ Activity  Assign │
│ ⚙ Settings  ◐  (LM)        │                                                   │                  │
└────────────────────────────┴───────────────────────────────────────────────────┴──────────────────┘
```

- Sidebar (`--side`): the logo (an 18 px rounded diamond mark in `--ink`) + "Fleet Deck" 13 px weight 600 + the Live state; a row with **New session** (solid ink, small) and a search button showing ⌘K; nav items 30 px tall with Lucide icons (`inbox` Inbox with the amber count, `layout-grid` Fleet, `eye` Watch, `activity` Activity, `server` Stations, then a "More" disclosure that lists Plans, Automations, Artifacts, Projects, Files, Shares, Terminals). A "Live" section lists the 6 live sessions as one-line rows (status glyph + title, 13 px). Footer: Settings, theme, the avatar "LM".
- Main: a 48 px header bar (`--line` bottom hairline) with "Fleet", a List/Board segmented control, Filter and Display ghost buttons. Then the Needs-you block: a header row ("Needs you" + count + key hints in kbd style at the right) and one 44 px row per request with the glyph, the session title, the request text, and the inline buttons. Then one group per station: a 32 px group header (station name weight 600, platform `--ink-2`, "3 of 12", an online dot, a collapse chevron) and 36 px rows: glyph · callsign (Geist Mono, `--ink-3`, 96 px column) · title (`--ink`) · now (tool icon + text, `--ink-2`, truncated) · chips (branch in mono, PR #128, "2 subagents") · changes (`+142` `−37`) · model (`--ink-2`) · age. Hover: `--raised` row fill.
- Right panel (300 px, `--side`): Usage (thin 4 px bars in `--ink-2` on `--line`; the bar turns `--signal` above 80 %), Conflict radar, Mail & tasks, and two ghost buttons Activity and Assign.

### Session (desktop)

Sidebar (the session row selected: `--raised` fill and a 2 px `--focus` marker at the left) + main + properties panel (280 px, `--side`).

- Header bar 48 px: breadcrumb "Lighting / otter-n1rr" (`--ink-3`, mono for the callsign), then the title "Fix flaky shim test" (15 px weight 600) and the status glyph; actions at the right: Interrupt, Terminal (ghost with icons), `folder-open`, `ellipsis`.
- Tabs as a segmented underline row: Chat, Changes 2, Files, Artifacts.
- Chat column max 680 px, centered. The user message: a `--raised` card with the avatar "LM" at the left. Agent prose: plain. Tool calls: single 30 px rows, Lucide tool icon, tool name weight 500, target in Geist Mono `--ink-2`, the result and a duration at the right in `--ink-3`, a chevron on hover. Thinking row collapsed.
- Code block: `--raised` fill, 10 px radius, 1 px `--line`, filename-like header "rust" + copy button.
- Table: hairline rows, header in `--ink-2` 12 px weight 500.
- Approval card: 1 px `--signal` border, `--raised` fill, the floating shadow; "Permission" + "Bash" + "2 min ago"; the command line in mono; the reason; buttons Allow (signal, kbd a), Allow for this session, Deny (kbd d).
- Composer: a bordered 10 px-radius box, the textarea, and a toolbar: `paperclip`, a model chip, at the right **Steer now** (ghost) and **Queue follow-up** (solid ink, ↵).
- Properties panel: a list of label/value rows (Status, Station, Branch, Folder, Model, Provider, Mode, Started, Changes +9 −3, Tokens 182 k / 9 k, Cost $1.12), then a "Files changed" list.

### New session (desktop)

The fleet home stays visible behind a dim overlay (`rgba(0,0,0,.55)`, light `rgba(24,24,27,.25)`). A **modal** 680 px wide, 14 px radius, `--raised`, centered at 120 px from the top:

- Top row: a breadcrumb chip "Lighting" ▸ "New session", a close `x` button.
- A borderless title input ("Session title (optional)", 18 px weight 600) and under it a borderless task textarea (15 px, 6 lines, placeholder "Add a dark mode toggle to the settings page").
- A row of **property pills** (28 px, `--canvas` fill, 1 px `--line`, Lucide icon + value + chevron): `server` Lighting · `folder` This directory · `folder-open` ~/projects/obsurvive/fleet-deck (mono) · `bot` Claude 2.1.306 · `sparkles` Opus 5.5 · `shield` Accept edits. One pill is open as a menu to show the style: the Permission mode menu (Station default, Ask, Accept edits ✓, Plan, Bypass with a `--bad` hint "Runs every tool without asking").
- Bottom bar (top hairline): a "Batch: one session per line" switch, "More options" ghost button, and at the right **Start session** (solid ink, kbd ⌘↵).

### Phone home and phone session

- Phone home: a large title "Fleet" (28 px weight 600) with the Live dot and the avatar; a search field; the "Needs you" rows as cards (`--raised`, 10 px radius) with two 44 px buttons each; then grouped list rows (station group headers, rows 56 px: glyph, title, now line under it, age). Bottom tab bar with a translucent `--side` fill and a top hairline: Sessions, Fleet, New, More (Lucide icons, labels 11 px, active in `--ink`, inactive `--ink-3`).
- Phone session: a header (back chevron, title over "Lighting / otter-n1rr", the glyph), a tab row, the end of the transcript, the approval card docked above the composer with 44 px stacked buttons.

## Do not

- No gradients, no glass blur except the phone tab bar, no colored model badges, no glows.
- No second accent color: `--focus` is for focus, links and the selection marker only.
- Do not use cards for list rows on the desktop: rows are flat, separated by spacing and hover fill.
