# Direction A: "Flight Deck" (folder `a/`)

Read `BRIEF.md` first. This file is the visual system of direction A. Follow it exactly; where it is silent, choose the quieter option.

## Idea

The product already speaks the language of carrier air operations: a deck, stations, callsigns, strips, radar, "land". Direction A takes that language seriously and modernizes it. Two real artifacts drive the design:

1. **Flight progress strips** (air traffic control): a narrow strip, divided into fields by thin rules, with a colored holder at its left end. Controllers read a whole bay of them at a glance. Here: **every session is a strip**, and each station is a **bay** of strips.
2. **Flight-deck jersey colors** (aircraft carrier crews): yellow shirts direct the aircraft, green shirts launch and recover, red handle ordnance and crashes, purple ("grapes") handle fuel, blue handle the aircraft on deck. Here they become the semantic colors: one color, one job.

The board is calm and dark, like a deck at night. Color appears only where a state lives. The one memorable thing is the strip bay, with the yellow "needs you" strips that light up in it. Everything else is quiet.

## Color

Primary mode: **dark** ("night deck"). Alt mode: light ("day deck").

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--deck` | `#111821` | `#EEF2F6` | page canvas |
| `--bay` | `#161E29` | `#F6F8FB` | sidebar, bay background, panels |
| `--strip` | `#1C2532` | `#FFFFFF` | strips, inputs, cards |
| `--strip-hi` | `#232E3D` | `#EEF4FB` | hover, selected strip |
| `--rule` | `#2A3545` | `#D8E0E9` | strip field dividers, hairlines |
| `--ink` | `#E9EEF4` | `#111821` | primary text |
| `--ink-2` | `#A5B0BF` | `#4A5666` | secondary text |
| `--ink-3` | `#7C8899` | `#5F6B7B` | tertiary text (still 4.5:1) |
| `--yellow` | `#FFD43B` | `#FFD43B` (fill) / `#8A6400` (text) | **needs you** and the primary button of a decision (Allow). Text on yellow: `#1B1600` |
| `--green` | `#3DDC97` | `#0F8F5C` | working, ok, additions |
| `--red` | `#FF6257` | `#D83A30` | hazard, deny, deletions, offline, conflicts, bypass mode |
| `--purple` | `#B79CFF` | `#6E4FE0` | fuel: usage, tokens, cost, rate limits |
| `--blue` | `#66B8FF` | `#1D6FD8` | handling: focus ring, selection, links, queued |

Rules:

- Yellow is never decoration, never a logo color on a calm board, never the New session button.
- Model and provider names are never colored. They are `--ink-2` text.
- A general primary button (New session, Start session) is a **solid ink button**: `--ink` fill with `--deck` text. A decision primary (Allow) is yellow.
- Elevation comes from the fill steps (`--deck` → `--bay` → `--strip` → `--strip-hi`), not from borders. Use `--rule` only for strip field dividers and section hairlines.
- Floating layers (menus, the approval card, the composer): `box-shadow: 0 1px 0 rgba(255,255,255,.04) inset, 0 16px 40px rgba(0,0,0,.45)` (light: `0 12px 32px rgba(17,24,33,.12)`).

## Type

- **Mona Sans** (Google Fonts, variable: width 75–125, weight 200–900) for all interface text. Link: `https://fonts.googleapis.com/css2?family=Mona+Sans:ital,wdth,wght@0,75..125,200..900&family=JetBrains+Mono:wght@400;500;600&display=swap`.
- **JetBrains Mono** only for code-like values: paths, branches, commands, code blocks, diffs. Never for times, counts, model names or labels.
- The width axis is the device: **callsigns** are condensed (`font-stretch: 78%`, weight 680); **station names and page titles** are wide (`font-stretch: 115%`, weight 620); body is normal width.
- Numbers use `font-variant-numeric: tabular-nums`.
- Scale (desktop): 11.5 px kbd hints · 12.5 px meta · 13.5 px UI and strip title · 14.5 px chat prose (line-height 1.6) · 15 px callsign · 17 px bay header · 24 px page title (line-height 1.15, letter-spacing −0.01em). Phone: body 15 px, titles 22 px.
- Sentence case everywhere. No all-caps labels.

## Shape and space

- 4 px grid. Radius: 6 px controls, 8 px strips, 12 px panels, 16 px sheets and the composer.
- Desktop control height 32 px (small 26 px). Phone 44 px.
- Main strip height 60 px; compact strip in the sidebar bay 46 px.

## The strip (the signature component)

```
┌─┬────────────┬──────────────────────────────────────┬───────────────┬──────────┬──────┐
│▌│ otter-n1rr │ Fix flaky shim test                  │ ⎇ fleet-sync  │ +9 −3    │ 2 min│
│▌│ Sonnet 5.5 │ ▲ Bash  cargo nextest run -p fleet…  │ Lighting      │ 1 file   │      │
└─┴────────────┴──────────────────────────────────────┴───────────────┴──────────┴──────┘
 holder  callsign cell     title + "now" line            where cell      changes   age
```

- Holder: a 4 px bar at the left edge in the status color: yellow (needs you), green (working), blue (queued), `--ink-3` (idle), hollow outline (ended), red (unreachable, lost).
- The fields are separated by 1 px `--rule` vertical lines, like the boxes of a paper strip. The callsign cell is 120 px wide: callsign (condensed, 15 px, `--ink`) over the model (12.5 px, `--ink-2`).
- The "now" line: an icon of the current tool + the tool name + its target in JetBrains Mono `--ink-2`; or the focus line in `--ink-2` italic-free regular. A needs-you strip shows the request here, in yellow text.
- Changes: `+142` in green, `−37` in red, then "6 files" in `--ink-3`.
- Chips (PR #128, 2 subagents): small 22 px pills, `--strip-hi` fill, `--ink-2` text, with a Lucide icon (`git-pull-request`, `workflow`).
- Working strips have a 2 px green progress hairline along the bottom edge (static in the screenshot, 40 % width). Needs-you strips have a 1 px yellow outline and a soft yellow glow (`0 0 0 1px #FFD43B, 0 0 24px rgba(255,212,59,.18)`).

## Layout

### Fleet home (desktop)

```
┌ rail 60 ┬ bay 296 ─────────────────┬ main ─────────────────────────────────────┬ ops 320 ──────────┐
│ [FD]    │ ⌕ Jump to…           ⌘K  │ Fleet                    [All|Working|Idle]│ Fuel              │
│         │ [ New session        N ]  │ 7 sessions on 3 stations. 2 need you.      │ Claude 5 h  ▓▓▓░ 62%│
│ ▦ Fleet │                           │                                            │ Claude week ▓░░░ 31%│
│ ◎ Watch │ Needs you  2              │ ┌ Needs you ────────────────── [ ] a d ──┐ │ Codex 5 h   ░░░░ 18%│
│ ≡ Act.  │ ▌otter  Fix flaky shim…   │ │ ▌strip + [Allow a][For session][Deny d]│ │ 4.2 M in · $38.40 │
│ ▤ Stat. │ ▌kite   Stripe webhooks   │ │ ▌strip + [Test key][Live key]          │ │───────────────────│
│ ▣ Plans │                           │ └────────────────────────────────────────┘ │ Radar             │
│ ⟳ Auto. │ Lighting  3               │ Lighting   Linux · x86_64   ●  ▮▮▮▯▯▯▯▯▯▯▯▯│ tokens.css        │
│ ◫ Art.  │ ▌falcon  Fleet Deck UI    │ ▌strip                                     │ falcon × heron    │
│ ⌂ Proj. │ ▌heron   Omarchy docs     │ ▌strip                                     │───────────────────│
│ ▢ Files │ dev1  2                   │ dev1  …                                    │ Mail & tasks      │
│ ⇪ Share │ ▌lynx    Rannoch checkout │ mac-m4 …   1 ended ›                       │ 1 mail · 1 task   │
│ ─────── │ mac-m4  1                 │                                            │ [Activity][Assign]│
│ ⚙ ◐ (L) │ ▌wren    Journzy iOS      │                                            │                   │
└─────────┴───────────────────────────┴────────────────────────────────────────────┴───────────────────┘
```

- Rail (60 px, `--bay`): the logo mark (a 30 px rounded square, `--ink` fill, "FD" in Mona Sans width 125 weight 800 in `--deck`), then nav icons (Lucide: `layout-grid`, `eye`, `activity`, `server`, `file-text`, `repeat`, `boxes`, `folder-kanban`, `folder`, `share-2`, `square-terminal`), each 40 px with a tooltip label on hover; active = `--strip-hi` fill + 2 px blue bar at the left. Bottom: settings, theme, account avatar (initials "LM").
- Bay (296 px, `--bay`, no border; a 1 px `--rule` line only between bay and main): search field with ⌘K hint, the New session button (solid ink, full width, "N" key hint), then the compact strip groups. Group headers: station name in wide type 13 px weight 600 + count in `--ink-3`. The connection state ("Live", green dot) sits at the top right of the bay.
- Main (`--deck`): page title "Fleet" (24 px wide), one summary sentence under it, a segmented filter at the right. Then the Needs-you panel (a `--bay` panel, 12 px radius, with a 13 px "Needs you" header, the key hints at the right in kbd style), then one bay per station: a bay header (station name wide 17 px, platform in `--ink-2`, an online dot, and a **slot meter**: 12 small 6×12 px ticks, filled for used sessions) followed by its full strips with 6 px gaps.
- Ops column (320 px, `--bay`): three quiet sections separated by hairlines: **Fuel** (usage meters drawn as 6 px purple bars on `--strip`, with the percent and the reset time), **Radar** (the conflict: file name in mono, the two callsigns), **Mail & tasks** (one line each), and two small ghost buttons Activity and Assign. This column replaces the old footer of links.

### Session (desktop)

Rail + bay (the `otter-n1rr` strip is selected: `--strip-hi` fill and a blue 2 px outline) + main. Main has the chat column (max 720 px) and an inspector (280 px) at the right.

- Header: a large strip: yellow holder, callsign cell (`otter-n1rr` condensed 17 px over "Sonnet 5.5 · Claude"), title "Fix flaky shim test" in 20 px weight 640, then a row of meta chips (`server` Lighting, `git-branch` fleet-sync in mono, `folder` ~/projects/obsurvive/fleet-deck in mono, "Accept edits"). Actions at the right: Interrupt (ghost, `square` icon), Terminal (ghost, `square-terminal`), an icon button `folder-open`, an icon button `ellipsis`.
- Tabs under the header: Chat, Changes (badge 2), Files, Artifacts. The selected tab: `--ink` text and a 2 px `--ink` underline.
- Transcript: the user message in a `--strip` bubble, right-aligned, max 70 % width. Agent prose is plain text. **Tool calls sit on a flight path**: a 2 px `--rule` vertical line at the left of the column; each tool call is a 34 px row on that line with a 10 px node (green filled = ok, yellow ring = waiting), the tool icon, the tool name in weight 600, the target in mono `--ink-2`, and the result at the right ("7 matches", "+9 −3") in `--ink-3`. The reasoning item is a collapsed row "Thinking · 12 s" with a chevron.
- Code block: `--strip` fill (dark: use `#0E141C` for code), 12 px radius, a 32 px header row with the language "rust" and a copy icon button. Mono 13 px, line-height 1.55, simple token colors (keywords `--blue`, comments `--ink-3`, functions `--ink`).
- Table: no outer border, header row in 12.5 px `--ink-2` weight 600, rows divided by `--rule` hairlines, code cells in mono.
- Approval card: floating above the composer, `--strip` fill, 16 px radius, the yellow holder at the left and the yellow glow. Header "Permission request" (13 px weight 600) + "Bash" + "2 min" at the right. The command in a mono code line. The reason in `--ink-2`. Buttons: **Allow** (yellow, kbd `a`), **Allow for this session** (neutral `--strip-hi`), **Deny** (ghost, red text, kbd `d`).
- Composer: a 16 px-radius floating dock, `--strip` fill, the floating shadow. A textarea (2 lines visible), and a toolbar row: `paperclip` icon button, a model chip "Sonnet 5.5", then at the right the split send: **Queue follow-up** (solid ink, "↵") and **Steer now** (ghost, "⌘↵").
- Inspector (280 px, `--bay`): sections "Status" (Needs you, since 2 min), "Changes" (+9 −3, the file `shim/reaper.rs`), "Fuel" (this session: 182 k in, 9 k out, $1.12 as purple text), "Station" (Lighting, online, slot meter).

### New session (desktop)

Rail + bay + main. Main shows one centered sheet (max 760 px) on `--deck`, no outer border, with sections separated by space, not boxes:

- Page title "New session" (24 px wide).
- The task text area as the hero: 160 px tall, 17 px text, `--strip` fill, 12 px radius, placeholder "Add a dark mode toggle to the settings page".
- **Station**: three selectable station tiles in a row (`--strip`, 12 px radius, 72 px tall): station name (wide 15 px), platform (`--ink-2`), the slot meter, "online". The selected tile (Lighting) has a 2 px blue outline and a `check` icon.
- **Workspace**: a segmented control (This directory / New worktree / Existing worktree), then the directory field (mono) with a Browse… button (`folder` icon).
- **Agent**: a segmented control for the provider (Claude 2.1.306 / Codex 0.163.0), a custom select for the model ("Opus 5.5", chevron-down icon), a segmented control for the permission mode (Station default / Ask / Accept edits / Plan / Bypass, with Bypass in red text).
- Title field and the Batch switch (a custom 36×20 px switch) on one row.
- "More options" as a quiet disclosure row with a `chevron-right` icon.
- A sticky footer bar at the bottom of main (`--bay`, top hairline): left "Lighting · ~/projects/obsurvive/fleet-deck" summary in `--ink-2`; right: Cancel (ghost) and **Start session** (solid ink, kbd ⌘↵).

### Phone home and phone session

- Phone home: a 56 px top bar (logo mark, "Fleet" in wide 20 px, the Live dot, the avatar). A search pill. "Needs you" with the 2 requests as strips with full-width 44 px buttons under each (Allow / Deny; Test key / Live key). Then the bays with compact strips (46 px → 56 px tall on the phone). The bottom tab bar (64 px + safe area, `--bay`, top hairline): Sessions, Fleet, New (a 44 px solid ink circle with `plus`), More.
- Phone session: a top bar (back chevron, the callsign over the title, the status as a yellow 8 px dot and "Needs you"), a scrollable tab row, the end of the transcript (from the code block on), and the approval as a **bottom sheet** (16 px top radius, the yellow holder as a 4 px top bar, three 44 px buttons stacked: Allow, Allow for this session, Deny), with the composer under it.

## Do not

- No gradients, no glassmorphism, no grain, no glow except the needs-you glow.
- No middle-dot meta strings inside strips: the strip fields replace them. (A middle dot is allowed in a one-line summary such as "4.2 M in · $38.40".)
- No colored model badges, no all-caps labels, no mono for times or counts.
