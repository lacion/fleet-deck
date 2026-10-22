# Fleet Deck design system ("Flight Deck", v3)

*Written 2026-10-22 on branch `ui-refresh`. The owner chose a mix of the concepts in `docs/v2/design/ui-refresh/concepts/`: the **look of A (Flight Deck)** with the **compact layout of B (Graphite)**. This file replaces the visual layer of `UI-DESIGN.md` (DL-141). The structure of `UI-PLAN.md` (sidebar, chat-first session, terminal dock, phone stack navigation) stays. The skill `.claude/skills/fleet-design-system/SKILL.md` tells every agent how to use this file and how to expand it. `tools/design-check.sh` checks the rules that a script can check.*

The reference pictures: concept A `docs/v2/design/ui-refresh/concepts/a/shots/*.png` (look, color, type, strips, approval card, composer, phone) and concept B `…/b/shots/*.png` (sidebar, density, list rows, properties panel, New session sheet, right ops panel).

---

## 1. Principles

1. **Calm deck, loud state.** The canvas is a quiet night deck. Color appears only where a state lives. Decoration has no color.
2. **One color, one job** (the carrier-deck jersey colors, §2.2). Yellow is *only* "needs you" and the primary button of a decision.
3. **Every session is a strip.** A status holder at the left edge, fields divided by thin rules. Strips are the signature of the board; use them for sessions everywhere (fleet, sidebar, needs-you, phone).
4. **Compact on a desktop, roomy on touch.** Desktop rows 36–44 px, controls 28 px. Touch (`pointer: coarse`) controls 44 px.
5. **Mona Sans for words, mono only for code.** The width axis carries meaning: condensed = callsign, wide = place (station names, page titles). JetBrains Mono only for paths, branches, commands, code, diffs and ids. Never mono for times, counts, models or labels.
6. **Depth from fills, not borders.** Surfaces step up in lightness. A 1 px rule only divides fields of a strip and sections. No box inside a box.
7. **Show what is known.** No "unknown", no empty labels: a missing value is an absent element (DL-141 rule 4 stays).
8. **Sentence case.** No all-caps labels, no letter-spaced capitals.

---

## 2. Tokens (`web/src/styles/tokens.css`)

The token **names** of DL-141 stay the public API: every area CSS file and `ui/palette.ts` (Omarchy palettes) use them. Their **values** change, and new tokens are added. Never write a raw color, font family or font size in an area CSS file: use a token (`tools/design-check.sh` fails on it).

### 2.1 Surfaces and ink

| Token | Dark (default) | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `#111821` | `#EEF2F6` | page canvas ("deck") |
| `--surface` | `#161E29` | `#F6F8FB` | sidebar, side panels, panels, dock ("bay") |
| `--raised` | `#1C2532` | `#FFFFFF` | strips, rows, inputs, buttons, cards ("strip") |
| `--raised-hi` *(new)* | `#232E3D` | `#EEF4FB` | hover, selected row, pressed segment |
| `--code` | `#0E141C` | `#F1F5F9` | code blocks, diffs, terminal frame |
| `--code-chat` *(new)* | `var(--code)` | `var(--raised)` | fill of a code block in the chat prose (`.fd-md-code`): the light `--code` is about 1.02:1 on `--bg` and the block looked like an empty outline |
| `--border` | `#2A3545` | `#D8E0E9` | strip field rules, section rules, dividers (never the edge of a form control) |
| `--border-soft` | `#212B39` | `#E4E9F0` | the faintest divider (list separators) |
| `--text` | `#E9EEF4` | `#111821` | primary text |
| `--dim` | `#A5B0BF` | `#4A5666` | secondary text |
| `--faint` | `#8896A7` | `#596575` | tertiary text (≥ 4.5:1 on `--bg`, `--surface`, `--raised`, `--raised-hi`, `--badge-bg` and the `--add-bg`/`--del-bg` diff lines) |
| `--faint-decor` | `#5B6779` | `#9AA5B4` | decoration only (hollow rings, ticks), never text |
| `--ink-fill` *(new)* | `#E9EEF4` | `#111821` | fill of the general primary button |
| `--ink-on` *(new)* | `#111821` | `#FFFFFF` | text on `--ink-fill` |
| `--badge-bg` *(new)* | `var(--raised-hi)` | `var(--border-soft)` | fill of a neutral `.fd-badge` and `.fd-count` (`--raised-hi` does not show on the light `--bg`) |
| `--badge-on-selected` *(new)* | `color-mix(in srgb, var(--text) 7%, var(--raised-hi))` (`#313B4A`, 1.2:1 on `--raised-hi`; `--dim` 5.1:1 on it) | `var(--badge-bg)` | fill of a neutral `.fd-count` or `.fd-kbd` on a selected row (`[aria-current=page\|true]`, `[aria-selected=true]`, `[aria-pressed=true]`): the dark `--badge-bg` is the `--selected-bg` (1:1), and the pill vanished. It is made of palette tokens, so it follows an Omarchy palette |
| `--btn-edge` *(new)* | `transparent` | `var(--border)` | edge of a plain button in a panel (a white button on the light `--surface` needs a hairline) |
| `--btn-edge-raised` *(new)* | `color-mix(in srgb, var(--ctl-edge) 25%, var(--border))` (`#3A4556`, 1.6:1 on `--raised`) | `var(--border)` | edge of a plain button on a `--raised` surface (a dialog, a strip, a card), where the button has the same fill. It follows an Omarchy palette through `--ctl-edge` and `--border` |
| `--ctl-edge` *(new)* | `#6B778A` | `#788496` | edge of a form control (input, select, textarea, checkbox, radio, the off switch track) and the ring of a pressed segment or chip: ≥ 3:1 on `--bg`, `--surface`, `--raised`, `--raised-hi` (WCAG 1.4.11) |
| `--pressed-bg` *(new)* | `var(--raised-hi)` | `var(--raised)` | fill of a pressed `.fd-seg` segment (light: a white segment with `--shadow-lift` on the track) |
| `--switch-knob` *(new)* | `var(--text)` (`#E9EEF4`) | `var(--raised)` (`#FFFFFF`) | knob of an off `.fd-switch` (the on knob is `--ink-on`) |
| `--knob-shadow` *(new)* | `0 0 0 0 transparent` | `0 1px 2px rgba(17,24,33,.25)` | shadow of the switch knob |
| `--selected-bg` *(new)* | `var(--raised-hi)` | `var(--raised)` | fill of the selected nav row, sidebar session row or activity channel (`--raised-hi` does not show on the light `--bg` and `--surface`) |
| `--selected-lift` *(new)* | `0 0 0 0 transparent` | `var(--shadow-lift)` | lift of a selected row (light only) |

`.fd-scope-dark` *(new)*: a part that stays dark in every theme (the wall ambient mode). `tokens.css` gives it the dark color block (`:root, .fd-scope-dark`).

### 2.2 Semantic colors (the jersey colors)

| Token | Dark | Light | Jersey | Meaning |
| --- | --- | --- | --- | --- |
| `--act` | `#FFD43B` | `#7F5C00` | yellow (directs) | **needs you**: text, icon, line, holder (light: 4.5:1 also on `--badge-bg` and the `--add-bg`/`--del-bg` diff lines) |
| `--act-fill` *(new)* | `#FFD43B` | `#FFD43B` | | fill of the decision primary (Allow), needs-you badge |
| `--act-ink` | `#1B1600` | `#1B1600` | | text on `--act-fill` |
| `--act-bg` | `#2A2410` | `#FFF6D6` | | tint behind a needs-you row |
| `--act-line` *(new)* | `#FFD43B` | `#A67C00` | | outline of a needs-you strip or card (light: 3:1 on `--bg`, `--raised`, `--act-bg`) |
| `--act-glow` *(new)* | `rgba(255,212,59,.18)` | `rgba(232,185,0,.22)` | | the needs-you glow (only there) |
| `--ok` | `#3DDC97` | `#0A7048` | green (launch) | working, ok, online, additions (light: 4.5:1 also on the `--add-bg` diff line) |
| `--ok-bg` *(new)* | `#10291F` | `#E3F5EC` | | tint behind ok |
| `--hazard` | `#FF6257` | `#B52A22` | red (crash, ordnance) | errors, deny, deletions, offline, conflicts, bypass mode (light: 4.5:1 also on the `--del-bg` diff line) |
| `--hazard-bg` | `#2C1517` | `#FDE8E6` | | tint behind hazard |
| `--fuel` *(new)* | `#B79CFF` | `#6447D6` | purple (fuel) | usage, tokens, cost, rate limits, model help (light: 4.5:1 also on `--badge-bg`, `--fuel-bg` and the diff lines) |
| `--fuel-bg` *(new)* | `#2A2348` | `#E6DFFC` | | track of a fuel meter (a visible track on `--surface` and `--raised` in both themes; light: `--fuel` 4.78:1 and `--faint` 4.61:1 on it) |
| `--focus` *(new)* | `#66B8FF` | `#1A5FBF` | blue (handling) | focus ring, selection marker, links, queued, verifying |
| `--focus-bg` *(new)* | `#13253A` | `#E5EFFC` | | selected tab or row tint, `info` badge (every text token 4.5:1 on it; an Omarchy palette tints its `--focus` 14 % on `--bg`, never the raw `selection` color) |
| `--add-bg` / `--del-bg` | `rgba(61,220,151,.10)` / `rgba(255,98,87,.10)` | `rgba(10,112,72,.10)` / `rgba(181,42,34,.08)` | | diff line fills. A diff sits on `--code` or `--bg` only, never on `--raised` (dark: `--faint` 4.17:1, `--hazard` 4.27:1 on a fill over `--raised`; `tools/design-check.sh` fails on a `--raised` fill of `.fd-diff` or a diff line) |
| `--free-bg` | `#10291F` | `#E3F5EC` | | free slot / allowed tint |
| `--term-selection` | `rgba(102,184,255,.3)` | `rgba(26,95,191,.22)` | | terminal selection (blue) |
| `--term-cyan` *(new)* | `#5CCFE6` | `#0E7490` | | terminal ANSI cyan (the terminal palette only; ANSI blue = `--focus`, magenta = `--fuel`) |
| `--term-black` / `--term-white` *(new)* | `var(--bg)` / `var(--text)` | `var(--text)` / `var(--dim)` | | terminal ANSI black and white (aliases, so they follow an Omarchy palette; light: both are dark ink, 4.5:1 on `--code`) |
| `--term-bright-black` *(new)* | `var(--faint)` | `var(--faint)` | | terminal ANSI bright black |
| `--term-bright-{red,green,yellow,blue,magenta,cyan}` *(new)* | `#FF8A76`, `#7BD3A6`, `#F6BE6A`, `#AECFFA`, `#D8CBFF`, `#9AE4DD` | `#8F1D17`, `#075C3B`, `#6E5000`, `#154C99`, `#5A3CC4`, `#0B5F75` | | terminal ANSI bright colors (light: each 4.5:1 on `--code` `#F1F5F9`) |
| `--term-bright-white` *(new)* | `#FFFFFF` | `var(--text)` | | terminal ANSI bright white (light: the ink, not white) |

Rules:

- **Yellow is never decoration**: not the logo, not New session, not a selected tab, not a focus ring (the focus ring is `--focus`).
- **Models and providers are never colored.** The family tokens `--m-*` and provider tokens `--p-*` remain only as aliases of `--dim`/`--raised` so old CSS keeps working; new CSS never uses them.
- A **general primary** button (New session, Start session, Save, Queue follow-up) is a solid ink button: `--ink-fill` with `--ink-on` text. A **decision primary** (Allow, Approve plan, Answer) is `--act-fill` with `--act-ink`. A screen has at most one of each. The sidebar New session is chrome and does not count; a page header may have its own general primary. On the New session route the sidebar button is the current nav row (`aria-current=page`: `--selected-bg` + `.fd-selbar`, not `.fd-primary`), so Start session is the one primary.
- `--fuel` is the only color of usage meters and cost numbers.

### 2.3 Type

Fonts are bundled (offline LAN rule, DL-141): `@fontsource-variable/mona-sans/standard.css` (weight 200–900 and width 75–125 %) and `@fontsource-variable/jetbrains-mono` (weight 100–800). IBM Plex is removed.

| Token | Value |
| --- | --- |
| `--font-ui` | `"Mona Sans Variable", system-ui, sans-serif` |
| `--font-data` | `"JetBrains Mono Variable", ui-monospace, monospace` (code-like values only) |
| `--font-callsign` | same as `--font-ui`; use it with `.fd-callsign` (below) |
| `--fs-micro` | 11px: kbd hints, tiny captions |
| `--fs-meta` *(new)* | 12px: metadata, timestamps, counts, chips |
| `--fs-data` | 12px: mono values in rows (paths, branches) |
| `--fs-ui` | 13px: interface text, rows, buttons (the default) |
| `--fs-field` *(new)* | the text of a field (`input`, `select`, `textarea`): 13px; 16px with `pointer: coarse` (iOS Safari zooms the page into a field under 16px, and the page stays zoomed). An area rule that sets a field's font size uses it; a larger field (the New session title) keeps its size; a mono path field keeps `--fs-data` on a desktop and uses `max(var(--fs-data), var(--fs-field))` in a coarse rule |
| `--fs-body` | 13px |
| `--fs-chat` | 14px: chat prose, line-height 1.6 |
| `--fs-title` | 14px: card and strip titles (weight 600) |
| `--fs-heading` | 15px: section and station headings (wide) |
| `--fs-strip-title` *(new)* | 17px: the session header title (weight 640) |
| `--fs-code` *(new)* | 12.5px: code blocks and inline code in chat prose |
| `--fs-page` *(new)* | 20px: page title (wide, weight 640, letter-spacing −0.01em) |
| `--fs-hero` *(new)* | 26px: phone large title |
| `--fs-display` *(new)* | 56px: display numbers read across a room (wall ambient counts, the pairing code) |
| `--fs-sheet-title` *(new)* | 18px: the title field of the New session sheet (§4.6, weight 600) |
| `--fs-sheet-task` *(new)* | 15px: a large task or search field (New session task, the palette search); the rule is `max(var(--fs-sheet-task), var(--fs-field))`, so a touch screen gets 16px |
| `--fs-12`, `--fs-13`, `--fs-15`, `--fs-18`, `--fs-24` | 12, 13, 15, 18, 24 px (kept for old CSS; new CSS uses a semantic token, and adds one when no token has the meaning) |

Width utilities (in `base.css`):

- `.fd-callsign`: `font-family: var(--font-ui); font-stretch: 78%; font-weight: 680; letter-spacing: 0;` — every callsign.
- `.fd-wide`: `font-stretch: 115%; font-weight: 620;` — station names, page titles, the wordmark.
- Numbers: the proportional figures of Mona Sans everywhere (no `tabular-nums` on the `body`). The tabular figures of Mona Sans draw a slashed zero, so `2.0`, `20 min` and `01:59` read as code (principle 5). A column of numbers aligns with `text-align: right` (or `justify-content: flex-end`) and a min-width in the area CSS.

Weights: 400 body, 500 UI emphasis, 600 titles, 640–680 page titles and callsigns. Line height: 1.4 UI, 1.6 chat prose, 1.15 titles.

### 2.4 Space, shape, size

| Token | Value |
| --- | --- |
| `--sp-1` … `--sp-5` | 4, 8, 12, 16, 24 px (4 px grid; also use `--sp-6: 32px` *(new)*) |
| `--r-chip` | 5px: chips, badges, kbd |
| `--r-ctl` | 6px: buttons, inputs, segments |
| `--r-card` | 8px: strips, rows, cards |
| `--r-panel` | 12px: panels, the approval card, menus, dialogs |
| `--r-sheet` *(new)* | 16px: the composer, bottom sheets, the New session sheet |
| `--r-pill` | 999px |
| `--ctl-h` / `--ctl-h-sm` | 28px / 24px (desktop); 44px with `pointer: coarse` |
| `--row-h` *(new)* | 36px: one-line list row and sidebar session row (30px in compact density) |
| `--row-h-touch` *(new)* | 44px: a list row on a touch screen (`@media (pointer: coarse)`), where the row is the tap target (the Files rows, list rows) |
| `--strip-h` *(new)* | 44px: a fleet strip (one line + meta line on narrow widths) |
| `--header-h` | 48px: page header bar |
| `--sidebar-w` | 248px |
| `--rail-w` | 300px: right panels (ops panel, properties panel) |
| `--chat-w` | 720px |
| `--tap-min` | 44px |

### 2.5 Elevation and motion

- `--shadow-pop`: dark `0 1px 0 rgba(255,255,255,.04) inset, 0 16px 40px rgba(0,0,0,.45)`; light `0 12px 32px rgba(17,24,33,.12)`. Only on floating layers: menus, dialogs, popovers, the approval card, the composer, toasts, bottom sheets.
- `--scrim` *(new)*: dark `rgba(6,10,15,.6)`; light `rgba(17,24,33,.28)` — the one dim layer behind every dialog, the drawer and a sheet.
- `--shadow-lift` *(new)*: dark `0 0 0 0 transparent`; light `0 1px 2px rgba(17,24,33,.07)` — strips and cards in the light theme.
- A shadow token (`--shadow-lift`, `--selected-lift`, `--knob-shadow`) is **never `none`**: area rules put it in a `box-shadow` list (`inset 3px 0 0 var(--ok), var(--shadow-lift)`), and `…, none` is invalid, so the browser drops the whole declaration. "No shadow" is the zero shadow `0 0 0 0 transparent`.
- `--t-fast` 120ms ease-out (hover, press), `--t-move` 200ms ease (expand, slide), `--t-drawer` 220ms ease-out.
- `--pulse-needs` 2400ms: the needs-you glow breathes (opacity of the glow only). `--pulse-working` 2400ms: the working glyph arc turns. Nothing else moves without a user action. `@media (prefers-reduced-motion: reduce)` stops every animation, and so does the board preference **Reduce motion** (`ui/theme.ts`, stored next to the theme choice; it sets `<html data-motion="reduce">`, and `base.css` applies the same stop), so that a user can pause the two breathing states without the OS setting (WCAG 2.2.2).

---

## 3. Primitives (`web/src/styles/base.css`)

Area CSS uses these and never restyles them. Class names of DL-141 stay; their look changes.

| Primitive | Look |
| --- | --- |
| `button` (bare) | `--ctl-h`, padding 0 10px, `--raised` fill, no border, `--r-ctl`, 13px weight 500, `--text`; hover `--raised-hi`; focus-visible 2px `--focus` outline offset 2px; disabled opacity .45 (a primary has its own disabled look). A plain button (no class, or only `.fd-sm`) in a `.fd-panel` has a `--btn-edge` edge; in a dialog (`.fd-dialog`, `[role=dialog]`, a `--raised` layer) a `--btn-edge-raised` edge. An area rule gives a plain button on another `--raised` surface (a strip, a card) the `--btn-edge-raised` edge |
| `.fd-primary` | **general primary**: `--ink-fill`, `--ink-on` text, weight 600; forced colors: a `ButtonText` fill with `ButtonFace` text. **Disabled** (not ready: Start session before a task, Send before a text): full opacity, a `--badge-bg` fill, `--faint` text (4.5:1), a 1px inset `--border` hairline, cursor `not-allowed`; its key hint loses its slab (`--faint` text); forced colors: `GrayText` text and edge on `Canvas`. Never the ink fill at .45 (a muddy slab, 2.3:1 in light, that reads as broken) |
| `.fd-decide` *(new)* | **decision primary**: `--act-fill`, `--act-ink`, weight 600; forced colors: the same solid `ButtonText` fill as `.fd-primary`. Disabled: the disabled `.fd-primary` look |
| `.fd-ghost` | transparent, `--dim`; hover `--raised` + `--text`. On a `--raised` layer (`.fd-sh`, `.fd-composer`, `.fd-md-code`, `.fd-q-dock`, `.fd-turn`, `.fd-dialog`, `[role=dialog]`), hover `--badge-bg`: there the `--raised` hover is the fill of the layer (1:1). The same holds for `.fd-iconbtn` and the ghost and icon menu summaries (hover and open); a pressed toggle and the `.fd-primary`, `.fd-decide` and `.fd-danger` variants keep their own hover |
| `select.fd-ghost` *(new)* | a quiet select for a view option in a page header (the fleet Group by): transparent fill and edge, `--dim`; hover `--raised` + a `--ctl-edge` edge + `--text`; focus the `--focus` edge of every field; forced colors a `ButtonText` edge. Only the background color changes (the chevron is the background image). Use it with `.fd-sm`; an area file never restyles a select into a ghost |
| `.fd-danger` | transparent, `--hazard` text; hover `--hazard-bg` |
| `.fd-sm` | 24px, padding 0 8px, 12px; on a `<select>` the padding-right stays 28px (the chevron) |
| `.fd-iconbtn` | square ghost (28 or 24), icon 16px. A toggle (`aria-pressed=true`, a panel toggle, compact cards) looks like a pressed segment: `--pressed-bg` fill + a 1px inset `--ctl-edge` ring + `--shadow-lift` + `--text` |
| `input`, `select`, `textarea` | `--raised` fill, 1px `--ctl-edge` edge, `--r-ctl`, `--fs-field` (13px; 16px on touch); focus: edge `--focus` + a 1px `--focus` ring (a 2px blue edge; `--focus-bg` is never a focus cue) and a 2px transparent outline (forced colors paint it); placeholder `--faint`. `select`: `appearance: none` + a chevron (CSS mask of Lucide `chevron-down`) at the right, padding-right 28px |
| `input[type=number]` *(new)* | no spin buttons (`appearance: textfield`; the WebKit spin buttons hidden): Chrome drew them on hover and focus and Firefox at all times, unstyled, inside a 160px field. The arrow keys still step the value |
| `fieldset`, `legend` *(new)* | `:where()` resets (zero specificity): a fieldset has no frame, no margin, no padding and `min-inline-size: 0` (the min-content width of the browser pushed a narrow column sideways); a legend has the `.fd-label` look (12px weight 600 `--dim`, 4px under it). An area sets only the layout of its group; it never copies the reset (§4.7: no boxed fieldsets) |
| `input[type=checkbox]`, `input[type=radio]` | `appearance: none`, 16px (24px with `pointer: coarse`), `--raised` fill, 1px `--ctl-edge` (hover `--faint`); checked: `--ink-fill` fill with an `--ink-on` check (mask) / dot; radius 4px / 50% |
| `.fd-switch` (role=switch) | 32×18 track, `--ctl-edge` off / `--ok` on, 14px knob: off `--switch-knob`, on `--ink-on`, shadow `--knob-shadow` |
| `.fd-seg` *(new)* | segmented control: a `--surface` track (2px padding, `--r-ctl`), segments 24px, the pressed one (`aria-pressed=true` or `aria-selected=true` or `:checked`) `--pressed-bg` fill + a 1px inset `--ctl-edge` ring + `--shadow-lift` + `--text` weight 600, others `--dim` |
| `.fd-chip` | filter toggle: 24px pill (44px with `pointer: coarse`), transparent, `--dim`, a quiet 1px edge at rest (`color-mix(in srgb, var(--ctl-edge) 60%, transparent)`, about 2:1: it tells that the label is a control); pressed `--badge-bg` + 1px `--ctl-edge` edge + `--text` weight 600 |
| `.fd-selbar` *(new)* | the selection marker of a selected row (palette option, slash-menu option, activity channel, subagent row, open integration row, sidebar nav and session row): a straight 2px `--focus` bar, top/bottom inset 6px, at the left edge, drawn as `::before` on `[aria-selected=true]`, `[aria-current=page\|true]`, `[aria-pressed=true]` or `[data-open=true]`; never an inset box-shadow (on a rounded row that draws a curved bracket). The area sets only the fill of the selected row. Forced colors: a `Highlight` outline |
| `.fd-badge` | 20px, padding 0 7px, `--r-chip`, `--raised-hi` fill, `--dim`, 12px weight 500, sans (not mono). `data-tone`: `act` (`--act-bg`/`--act`), `hazard`, `ok`, `info` (`--focus-bg`/`--focus`), `fuel`, `muted` (a quiet text badge: no fill, no edge, no side padding, `--faint`). `data-family`/`data-provider`: neutral (no hue) |
| `.fd-count` *(restyled)* | 18px pill, `--badge-bg`, 11px weight 600; `data-tone="act"`: `--act-fill`/`--act-ink`; `data-tone="info"` *(new)*: `--focus-bg`/`--focus` (news that is not a request: the unread mark of a session); `data-shape="dot"` *(new)*: a 6px dot with no text (an `info` dot is `--focus`), where a number would read as another count (the unread mark in a sidebar session row; it keeps `role="img"` and a name, "3 unread"). On a selected row a neutral count (and a `.fd-kbd`) takes `--badge-on-selected`. The one form of a count after a label (§5) |
| `.fd-dot` | the **status glyph** (12px, CSS only, shape + color): `working` ring `--ok` with a 3/4 arc (conic gradient) that turns slowly; `verifying` ring with half arc `--focus` and a 4px `--focus` center dot (the working glyph has none: the two differ by shape, not by hue alone, also with reduced motion); `needs_you` filled `--act-fill` circle with an `--act` edge and a dark "!" (pseudo-element); `queued` dashed ring `--focus`; `idle` hollow ring `--faint`; `ended` filled `--faint`; `lost`, `unreachable` `--hazard` ring with a slash; `online` *(new)* a small (8px) still filled `--ok` dot — a station link or a provider sign-in (a link is not work: it never turns) |
| `.fd-holder` *(new)* | the 3px status bar at the left edge of a strip, `data-status` colors: needs_you `--act`, working `--ok`, verifying/queued `--focus`, idle `--faint-decor`, ended transparent with a 1px `--faint-decor` edge, lost/unreachable `--hazard` |
| `.fd-glow` *(new)* | the needs-you outline and glow of a strip or card: 1px `--act-line` outline, a 3px `--act-glow` ring that breathes (`--pulse-needs`) on `::after`. The element must be positioned |
| `.fd-label` | section label: 12px weight 600 `--dim`, sentence case (no capitals, no tracking) |
| `.fd-kbd` | 18px, padding 0 5px, `--r-chip`, `--badge-bg`, 11px `--dim`, sans (`--faint` was under 4.5:1 on the light `--badge-bg`). In the help table (`.fd-keys`) the keys are the content: 20px, 12px (`--fs-meta`) |
| `.fd-menu` / `.fd-menu-body` | popover: `--raised` (light `--raised`), `--r-panel`, `--shadow-pop`, padding 4px; rows 28px ghost, left-aligned, icon 14px; row hover `--badge-bg` (the light `--raised-hi` is 1.07:1 on `--raised`). A menu taller than its max-height (`min(70vh, 560px)`) scrolls: its rows never shrink (`flex-shrink: 0`; they collapsed to 16px). `.fd-menu > summary.fd-ghost` *(new)*: a summary that looks like a ghost button (transparent, `--dim`; hover and open `--raised` + `--text`) |
| `.fd-panel` | `--surface`, `--r-panel`, no border |
| `.fd-dialog-head` *(new)* | the head of every dialog: a grid with the title (`h2`, 15px weight 640, line-height 1.15), an optional description under it (any other child: `--dim`, no margin), and the Close icon button (`.fd-iconbtn`, `X` icon, `aria-label="Close"`) in the right column, centered on the title line (negative block margins, so that the title starts at the padding edge). A dialog has no text Close button |
| `.fd-code` | mono 12px, `--code`, `--r-card`, padding 10px 12px |
| `.fd-diff` / `.fd-dl[data-kind]` | mono 12px; an inline-size container with one grid column `minmax(max-content, 1fr)`, so that every line fill reaches the end of the longest line when the diff scrolls sideways (a thread or an editor inside the diff does not widen it: it wraps at the visible width, `100cqi`, and sticks to the left edge); `add` `--ok` on `--add-bg`; `del` `--hazard` on `--del-bg`. The file header row is part of the primitive, its path text too: `.fd-dl-path` holds `.fd-dl-dir` (the folders, in a `<bdi>`, `direction: rtl`, so that they lose their start and keep the trailing slash: `…station/src/shim/reaper.rs`) and `.fd-dl-base` (the file name, never shrinks); `hunk` `--focus`; `meta` `--faint`; `file` `--surface` weight 600, a sticky 28px header row (icon, mono path, a `.fd-badge` status for a new (`ok`), deleted (`hazard`) or mode-changed (neutral) file, +/− counts; the git plumbing lines are not drawn) with a `--border-soft` rule, at most the visible width (`100cqi`): the folders of the path ellipsize and the counts stay at the right edge; with review, two number gutters (old, new; only new on a phone) |
| `.fd-meter` *(new)* | 6px bar, `--fuel-bg` track, `--fuel` fill, radius 3px; over 80 % the fill is `--act`. Markup: one child with an inline `width` (the percent); `data-level="high"` on the meter over 80 %. `data-tone="neutral"`: a load that is not usage (station resources, a slot row over 24 slots): `--badge-bg` track, `--dim` fill; `data-level="high"` fill `--act` |
| `.fd-slots` *(new)* | slot meter: a row of 6×12px ticks (gap 2px), used a filled `--ok` bar, needs-you an `--act` "!" (a 7px bar, a 2px gap, a 3px dot: the sign of the needs-you `.fd-dot`), free a hollow 1px `--ctl-edge` outline (shape, not color alone). Markup: one child per slot with `data-slot="used"` or `"needs_you"` (none = free) |
| `.fd-avatar` *(new)* | the account avatar: 24px circle, `--badge-bg` fill with a 1px inset `--btn-edge` ring, `--text` 11px weight 600 initials. An area file never copies it: a stack (sharing `.fd-avatars`) only adds an overlap and a `--surface` gap ring to `.fd-avatars > .fd-avatar` |
| `.fd-callsign`, `.fd-wide` | §2.3 |
| `.fd-note`, `.fd-empty`, `.fd-vh` | dim small text; faint text; visually hidden |
| `<details>` summary | no native marker: a `chevron-right` mask that turns 90° when open |
| `summary[data-variant="toggle"]` *(new)* | a fold toggle in a row of toggles (the station card folds): no leading marker; a trailing 12px `chevron-down` mask in `--faint` that turns 180° (`--t-move`) when open (forced colors: `CanvasText`). The area sets the size and the fill. On a ghost menu summary (`.fd-menu > summary.fd-ghost[data-variant=toggle]`, the chat Show menu) the chevron is `currentColor` (the `--dim`/`--text` of the button) |
| `@media (pointer: coarse)` | buttons, summaries, inputs, selects, segments, chips, `.fd-tap`: min-height 44px (`--tap-min`); `.fd-iconbtn` 44px; checkbox and radio 24px. The rule has zero specificity (`:where()`): an area rule that sets `min-height: 0` on a button, a summary or a button class must give the target back in a `@media (pointer: coarse)` rule of the same file for the same selector (`tools/design-check.sh` fails otherwise; inline controls in a line of text are on its allowlist) |
| scrollbars | 8px, thumb `--ctl-edge` (3:1 on `--bg` and `--surface`, WCAG 1.4.11), transparent track; Firefox: `scrollbar-color` |
| links | `--focus`, no underline; underline on hover and focus. A link without a class, or with only `.fd-tap`, inside prose (`.fd-note`, `.fd-md`, `p`) is always underlined (1px, offset 2px): color alone does not mark it (WCAG 1.4.1) |

---

## 4. Components

### 4.1 The strip (session row)

```
┌─┬────────────┬─────────────────────────────────────┬──────────────────┬─────────┬──────┐
│▌│ otter-n1rr │ Fix flaky shim test   ▲ Bash cargo… │ ⎇ fleet-sync     │ +9 −3   │ 2 min│
└─┴────────────┴─────────────────────────────────────┴──────────────────┴─────────┴──────┘
 holder callsign  title (600) + now (dim, tool icon)   where (mono 12px)  changes   age
```

- `--raised` fill, `--r-card`, height `--strip-h` (44px), 1px `--border` vertical rules between fields, the `.fd-holder` at the left edge. Light theme adds `--shadow-lift`.
- Callsign cell 104px: `.fd-callsign` 14px `--text` over the model (12px `--dim`) — or one line (callsign only) when the strip is one line.
- Title 14px weight 600; the "now" text (current tool icon + name + mono target, or focus line) in `--dim` on the same line after the title, truncated with an ellipsis.
- Changes: `+142` `--ok`, `−37` `--hazard`, 12px; files count `--faint`.
- Chips that are links of their own (PR, issue, project, batch compare): `.fd-badge` with a Lucide icon, in the second line of the where cell. Counters and flags of the session (mail, subagents, artifacts, background tasks, terminal, permission mode, conflict, archived, stale) are the quiet form `.fd-strip-ico` in the meta cell: a 12px Lucide icon and the count (12px `--dim`; a hazard flag `--hazard`), with no fill. Each one is `role="img"` with an accessible name ("Subagents running: 2"), because the strip is a link.
- Hover `--raised-hi`. Selected (the open session): `--raised-hi` + 1px `--focus` outline. A selected needs-you card shows its selection as a 2px inset `--focus` ring inside the `--act-line` outline (fine pointer only, never on the phone), with no tint.
- **Needs you**: holder `--act`, 1px `--act-line` outline, `--act-glow` glow (breathing), the request text in `--act`, inline action buttons (`.fd-decide` Allow with kbd `a`, Deny ghost danger with kbd `d`, or the question's options). The other plain answers have a `--raised-hi` fill and a 1px `--ctl-edge` edge on hover. In a station lane under the Needs you panel, the needs-you strip has the holder, the 1px `--act-line` outline and the yellow request text; the breathing glow is the panel's (one glow for one request on the screen).
- **Cleared (archived)**: no fill, a 1px inset `--border-soft` edge, title weight 500; the text keeps the `--faint` of an ended strip. Never opacity: a dimmed text falls under 4.5:1.
- The lane activity sparkline is a data graphic: bars `--faint` (3:1 or more), the last bar `--ok`.
- **Ended/lost**: text `--faint`, holder hollow.
- Below 900px the strip becomes two lines (title line, meta line) and drops the field rules.

### 4.2 Sidebar (desktop ≥ 1024px, the tablet drawer)

From concept B, with A's look. Width `--sidebar-w`, `--surface`, no right border (the fill step separates it).

1. Brand row (48px): the mark (24px rounded square, `--ink-fill`, "FD" in `--ink-on`, `.fd-wide` weight 800 10px), "Fleet Deck" 14px weight 600, the connection pill at the right (`Live`: `--ok` dot; `Stale`: `--act`; `Offline`: `--hazard`).
2. A row: **New session** (`.fd-primary` `.fd-sm`, `plus`, kbd `N`) and a search button (`search`, kbd `/`; ⌘K opens the palette).
3. Nav (rows 28px, Lucide 16px icon + label, active: `--raised-hi` fill + a 2px `--focus` bar at the left): Fleet (with the needs-you `.fd-count data-tone=act` when > 0), Watch, Activity, Stations, then the "More" disclosure (Plans, Automations, Artifacts, Projects, Files, Shares, Terminals).
4. Sessions: groups (Needs you first, then each station: `.fd-wide` 12px weight 600 name, the count right after the name, then the reach dot; every group header and the fleet lanes put the count inline after the name), rows `--row-h` with `.fd-dot` + title (13px) + age (12px `--faint`) at the right; the selected row: `--raised-hi` + 2px `--focus` bar. Ended sessions in a collapsed "Ended" group with its `.fd-count` pill (the fleet lanes use the same row).
5. Footer (44px): settings icon, the theme segmented control (sun / moon / monitor), the compact cards icon toggle (pressed like a segment), the account avatar (24px circle, initials).

### 4.3 Page header

48px bar (`--header-h`), `--bg`, bottom 1px `--border-soft`: page title (`--fs-page` `.fd-wide`) + one summary sentence in `--dim` (`7 sessions on 3 stations. 2 need you.` with "2 need you" in `--act`), filters as `.fd-seg` at the right, then view options (a view option select is `select.fd-ghost.fd-sm`). No boxed toolbars. The `Page` component has a `filters` slot (`.fd-page-filters`) before the actions; a narrow column wraps it under the title, and a phone gives it a full row.

A `Page` (`ui/page.tsx`: Stations, Settings, Shares, the list pages) has a 48px title row (title + count, actions level with it) and its summary or description under it in `--dim`, max 80ch, balanced. A soft rule closes the header. The fleet home keeps the one-row bar because its filters share the row. Every page and the fleet home start on the same 24px gutter (`--sp-5`; phone `--sp-4`).

Narrow main columns (the fleet home header): under 1100px the search folds to its icon until it has focus or text, and Share board shows its icon only (the text stays as the accessible name); between 841 and 960px the sentence drops "on N stations", and a "Nothing needs you." next to a conflict (the calm part goes first, so the conflict stays readable); under 840px the controls take a second header row.

### 4.4 Fleet home

```
┌ sidebar 248 ┬ main ──────────────────────────────────────────────┬ ops 300 ───────┐
│             │ Fleet  7 sessions on 3 stations. 2 need you. [seg]  │ Usage          │
│             │ ┌ Needs you 2 ────────────── [ ] a d ─┐             │ meters (fuel)  │
│             │ │ strip + inline actions               │             │ totals         │
│             │ └──────────────────────────────────────┘             │ Conflict radar │
│             │ Lighting  Linux x86_64 ● ▮▮▮▯▯▯▯▯▯▯▯▯ 3 of 12        │ Mail & tasks   │
│             │ strip / strip / strip (6px gap)                     │ [Activity]     │
│             │ dev1 …    mac-m4 …    Ended (1) ›                    │ [Assign]       │
└─────────────┴──────────────────────────────────────────────────────┴────────────────┘
```

- The needs-you requests move from the right rail into a **Needs you panel** at the top of main (`--surface` panel, `--r-panel`, header "Needs you" + count + key hints `[` `]` Move `a` First `d` Last: the keys answer with the first and the last option, whatever their labels; while an answerable request has more than two options, the hints add `2`–`9` Option (the range up to the largest middle option, `2` Option for three options): a number key answers the option with that number, and its button shows the number as its key hint), each request a needs-you strip with its inline actions. The keyboard cursor (the 2px `--focus` ring and the key hints on the buttons) is always on a request that the keys can answer: it starts on the first answerable request, and `[` `]` skip a request that only opens its session. With no request the panel is absent and the header sentence says "All clear." in `--ok` — only when there is also no conflict; with a conflict it says "Nothing needs you." (`--ok`), so that green "all clear" never stands next to a red conflict.
- Station groups: a group header (station `.fd-wide` 15px weight 620, platform `--dim`, reach dot + "online"/"offline", `.fd-slots` + "3 of 12" at the right), then strips with 6px gaps. Collapsible.
- The **ops panel** (right, `--rail-w`, `--surface`) replaces the bottom tab bar: sections Usage (`.fd-meter` per rate limit with percent and reset time, totals in `--fuel`), Conflict radar (file in mono, the sessions), Mail & tasks (the waiting rows; the new-mail form folds under "New mail or task"), and the secondary pages (Activity, Assign, All usage — every meter and the unknown parts; the section above is the glance, so the button does not repeat its "Usage" name) as ghost buttons in a sticky foot that open them. The Usage section and All usage ignore the status segment of the header: a rate limit belongs to an account, not to a status. The Mail & tasks section stays mounted, so it loads its tasks and grants again when the fleet changes, at most once in 10 s (a busy fleet changes on every event). Under 1280px the ops panel folds under main.
- The conflict radar never says "Conflict radar unavailable" in a banner: an unavailable radar shows a quiet `--faint` line inside its ops section.

### 4.5 Session

- Header (56px): a strip header — the holder, the callsign cell (`.fd-callsign` 15px over "Sonnet 5.5 · Claude" in `--dim`), the title (17px weight 640), then chips (station with `server`, branch mono with `git-branch`, folder mono with `folder`, mode). Actions at the right: Interrupt, Terminal (ghost buttons with icons), `folder-open`, the session menu `ellipsis`.
- Tabs: Chat, Changes (count), Files, Artifacts — 36px, the selected one `--text` with a 2px `--text` underline (not yellow). The chat Show filter is a ghost menu (12px, chevron) at the right end of the tab row, over the chat panel; with the Agents switcher it shares the Agents bar. It never floats over the transcript, where tool rows show their results. On a phone the tab row keeps room at its right end for the menu.
- Chat column `--chat-w`, centered in the space left of the properties panel.
- **Properties panel** (right, 280px, `--surface`, collapsible with `panel-right`): Status (glyph + text + since), Station (reach + slots), Branch, Folder, Model, Mode, Changes (+/− and the files), Usage (tokens and cost in `--fuel`), Subagents.
- Transcript: the user message in a `--raised` bubble, right-aligned, max 75 %. Agent prose plain, 14px/1.6. **Tool calls on a flight path**: a 2px `--border` vertical line at the left of the column; each tool call a 30px row with a 10px node (filled `--ok` ok, ring `--act` waiting (on the wall with an `--act` center dot), `--hazard` failed; a running call without a request: the working glyph in the chat, an `--ok` ring on the wall; an ended call without a result: a dashed `--faint` ring on the wall; on the wall each line that is not done also carries its state as `.fd-vh` text), tool icon, tool name weight 600, target mono `--dim`, result at the right in `--faint`. Reasoning collapsed ("Thinking · 12 s").
- Code: `--code-chat` with a 1px `--border` inset ring, `--r-panel`, a 28px header (language + copy icon button), mono 12.5px/1.55, token colors: keywords `--focus`, strings `--ok`, comments `--faint`.
- Tables: no outer border, header 12px weight 600 `--dim`, rows divided by `--border-soft`.
- **Approval card**: `--raised`, `--r-panel`, holder `--act` at the left, `--act-line` outline, `--act-glow`, `--shadow-pop`. Header: shield icon + "Permission request" (or "Question", "Plan") weight 600, the tool, the age at the right. The command in a `.fd-code` line, the reason in `--dim`, buttons: `.fd-decide` Allow, Allow for this session (plain, with the `--btn-edge-raised` edge on the `--raised` card), Deny (`.fd-danger`); no key hints: the a/d keys work only in the fleet Needs you panel.
- **Composer**: a `--raised` dock, `--r-sheet`, `--shadow-pop`, textarea 2 lines min, toolbar: attach (`paperclip`), the model chip, at the right **Steer now** (ghost, kbd ⌘↵) and **Queue follow-up** (`.fd-primary`, kbd ↵).
- Terminal dock: `--surface` bar 32px, tabs like the session tabs, the terminal on `--code`. With the Properties panel open, the dock stops at the panel and the panel's `--surface` column goes on to the window bottom.

### 4.6 New session (a sheet, B's modal look, on its own route)

The `#/new` route keeps its URL and its form. It renders a centered **sheet** (max 720px, `--raised`, `--r-sheet`, `--shadow-pop`, 24px padding) on the page canvas (`--bg`; dim with `--scrim` only when a page is behind the sheet):

1. A title input without a border (18px `--fs-sheet-title` weight 600, "Session title (optional)").
2. The task textarea without a border (15px `--fs-sheet-task`, 16px on touch, 6 lines, "What should the agent do?"); the batch hint under it.
3. Property rows (label 12px `--dim` at the left, 72px): **Agent**: provider `.fd-seg` (with instance limits, R4.3, the segments stack and the limit sits right-aligned in `--dim` 12px), model select pill and permission mode select pill, each with a leading 14px icon (sparkles: model, shield: mode) (Bypass shows a `--hazard` hint "Runs every tool without asking"); **Station**: station radio pills (name, reach dot, "9 free"; the chosen pill has the pressed `.fd-seg` look: `--pressed-bg`, a 1px inset `--ctl-edge` ring, `--shadow-lift`, weight 600; on a phone two equal columns, `--raised` + `--shadow-lift` at rest); **Where**: workspace `.fd-seg`, directory input (mono) + Browse…
4. "More options" disclosure.
5. Footer (a 1px `--border` top rule; on a phone sticky above the bottom bar on `--surface` with a `--border` top rule, and the form fades over 24px above it while the page scrolls on): Batch switch, then at the right Cancel (ghost) and **Start session** (`.fd-primary`, kbd ⌘↵).

### 4.7 Panels, settings pages and forms

- A settings page: page header, then sections separated by 24px space and a 12px `.fd-label`-free heading (15px weight 600). Fields in a two-column grid (label 200px + control), no boxed fieldsets. Group related fields in one `--surface` panel only when the group has its own actions.
- **Fields.** The `Field` primitive (`ui/page.tsx`, `page.css`) has two layouts. Stacked (the default): a 12px `--dim` label over the control, for a narrow form (a dialog, the ops panel, a sheet). Row (`layout="row"`, `.fd-fieldwrap[data-layout="row"]`): a 200px label column (13px weight 500 `--text`, min-height `--ctl-h`, so that it sits on the control line), a `--sp-5` gap, controls at most 360px (a number field 160px), the hint under the control in column 2, at most 72ch; under 720px the row stacks. Every settings page (Station, Deck, Displays, Tailnet, Accounts) uses the row layout: one field look for one kind of page. A `<fieldset>` has no frame (base.css): its `<legend>` is a `.fd-label`.
- Tables: rows 36px, `--border-soft` dividers, header `--dim` 12px weight 600, hover `--raised`.
- Empty states: a 20px `--faint` icon, one sentence in `--dim` that says what to do, one button.
- Notices: `fd-notice` (shell) and `fd-set-notice` (settings) rows 36px, `--surface` with a 3px tone holder at the left and a 14px tone icon before the text (`error` `--hazard` octagon-alert, `warn` `--act` triangle-alert, `info` `--focus` info, `ok` `--ok` circle-check); forced colors: a 3px `CanvasText` left edge.
- Dialogs: `--raised`, `--r-panel`, `--shadow-pop`, 20px padding, a `.fd-dialog-head` (title 15px weight 640, an optional `--dim` description, the Close icon button at the right), actions right-aligned. A dialog closes with its X, Escape and a click on the scrim; never with a second, text Close button.

### 4.8 Phone (< 640px)

From concept A: top bar 52px (mark, page title `.fd-wide` 18px, Live, avatar) on every page with the bottom bar; the page h1 and the New session caption stay only for screen readers, and a page under More shows a Back chevron in place of the mark (to its list for a detail page: Artifact to Artifacts, Project to Projects, Compare to Fleet; else "Back to More"); needs-you cards first (strip + full-width 44px buttons; Deny there has a `--raised-hi` fill, as in concept A phone); station groups of two-line strips; the bottom bar (60px + safe area, `--surface`): Sessions, Fleet, **New** (a 44px `--ink-fill` circle with `plus`), More. Session: back + callsign over title + status; the approval is a **bottom sheet** (`--r-sheet` top corners, a 4px `--act` top bar, three stacked 44px buttons); the composer under it.

**Dialogs on the phone are bottom sheets** (under 640px): the dialog is anchored to the bottom edge at the full width, with `--r-sheet` top corners and square bottom corners; its bottom padding adds `env(safe-area-inset-bottom)`; max-height 85dvh with its own scroll; a tap on the `--scrim` above it closes it. The Close X of `.fd-dialog-head` and the actions are in thumb reach.

### 4.9 Wall display (`/display`)

The wall keeps its own layout (R13) and uses the same tokens and the strip; type one step larger (`--fs-ui` 15px) for distance reading.

The wall scope (`.fd-wall-root` in `wall.css`) re-declares the size tokens (`--fs-*`, `--sp-*`, `--r-*`, `--ctl-h*`, `--row-h`, `--strip-h`) as multiples of the wall unit `--fd-wall-u` (1px on a 1080p screen at scale 1; `clamp(14px, 100dvh/80, 28px)` × scale / 14). The type is the board scale × 15/13. The fixed-size primitives (`.fd-dot`, `.fd-holder`, `.fd-mark`) zoom by `--fd-wall-k`, which `WallPage` sets. The ambient mode adds `.fd-scope-dark` and stays dark in the light theme.

Tiles fill the tiles area: `tileGrid` picks the columns, and the tile type grows up to 1.25× (`--fd-wall-tile-k`, set on the grid) when there are few tiles; the needs area grows by the same factor, so a request never reads smaller than a tile. The Activity stream runs newest first under its heading; an ultra wall has a unit floor of 16px. Below 640px a portrait wall scrolls, and the needs and activity areas take their content height.

---

## 5. Copy

Sentence case; plain verbs; one name per action across a flow (Allow → "Allowed"). Times: "2 min", "14 min", "2 h", "1 d". Counts: "3 of 12". No "unknown". Errors say what happened and what to do.

**Numbers.** One kind of number has one format on every surface, from one helper of `ui/format.ts` (never `toFixed`, `toLocaleString` or a local formatter in a component):

| Kind | Helper | Examples |
| --- | --- | --- |
| a count (events, files, lines) | `countText` | `940`, `1,286`, `Events 7,405` |
| the lines of a diff | `addedText`, `removedText` | `+1,286` `−400` (U+2212 minus) |
| a large count where room is short (tokens) | `compactCount` | `940`, `9.4K`, `236K`, `1.3M` (two significant digits, no trailing `.0`, no space before K or M: `236 k out` read as a word) |
| money | `usdText` | `$1.20`, `$14,876.41` (separators and cents everywhere, so that every cost reads the same) |

**Counts after a label.** A count after a label (a nav row, a group, a tab, a filter, a button: Ended, Archived, Resume all, Clear, Filters) is a `.fd-count` pill after the label: `Archived` and the pill `31`. Never `Archived (31)`: two notations for one thing. The accessible name is the label and the number ("Resume all 2"); a pill whose number alone is unclear adds a `.fd-vh` word. A count of zero on a disabled button shows no pill.

**Paths.** A shown path starts at its root: `~/projects/app` under a home folder (`homeShort`), else `/srv/data`; never a root-less `home/me`. A folder row shows its name and its parent (`pathParts`: `v2` and `~/fleet-deck-worktrees/`, the parent in `--faint` mono 12px with an ellipsis) and the full path in its `title`. Copy actions copy the full path.

**Models.** A model is its family and version: `Opus 5.5` (`shortModel`). The context window is a fact of its own, `1M context` (`contextLabel`), in the Properties panel; a bare `1M` after the version read as a version or a count. The `title` keeps the raw id.

## 6. Accessibility floor

Text 4.5:1 (large text and icons 3:1) in both themes and with an Omarchy palette (`ui/palette.ts` computes the palette tokens and steps them until they pass: every text token 4.5:1 on `--raised-hi` and on every tint, `--act-bg`, `--hazard-bg`, `--ok-bg`, `--fuel-bg`, `--focus-bg`; `--ctl-edge` 3:1 on `--raised-hi`; `--act-line` 3:1 on `--bg`, `--raised` and `--act-bg`); focus visible on every control (`--focus` 2px outline); state never by color alone (glyph shapes, holder + text); touch targets 44px; `prefers-reduced-motion` and the board preference Reduce motion honored (§2.5); every icon-only button has an `aria-label`. In forced-colors mode (Windows High Contrast) the page takes the theme of the OS color scheme (`prefers-color-scheme`), not the stored choice or an Omarchy palette (`resolvedTheme()` in `ui/theme.ts`): the kept glyph colors below are made for their own theme, and a dark `--act` on a white `Canvas` is 1.43:1. `base.css` keeps the glyphs, holders, slots and meters in their own colors (`forced-color-adjust: none`); gives a `Highlight` ring to the pressed segment, chip and toggle (any `aria-pressed=true` button), the selected tab (`[role=tab][aria-selected=true]`), the selected option of a list box (`[role=option][aria-selected=true]`), the current nav or session row (`[aria-current=page]`), the current subagent row (`[aria-current=true]`), the open integration row (`.fd-selbar[data-open=true]`) and the needs-you keyboard cursor (`.fd-q[data-selected=true]`); draws the switch and a checked checkbox or radio in `Highlight`/`HighlightText`; draws the summary chevron in `CanvasText` and the select chevron as an `url()` image; gives key hints, badges, counts and the avatar a 1px `CanvasText` edge; keeps `.fd-primary` and `.fd-decide` solid (`ButtonText` fill, `ButtonText` edge, `ButtonFace` text and key hint; not `Highlight`, which marks the selection and the focus), so that one primary still stands out of a row of plain buttons; gives the ghost select a `ButtonText` edge and a segmented control (`.fd-seg`) a 1px `ButtonText` group edge. An unselected underlined tab (session pane tabs, settings tabs) has no rule in forced colors: only the selected tab is marked. An area part whose edge or selection is only a `box-shadow` or a fill adds its own `@media (forced-colors: active)` rule. The chat flight path (its line and nodes) keeps its own colors (`forced-color-adjust: none` in chat.css), so that a filled node and a ring still differ by fill. Check it with Playwright `page.emulateMedia({ forcedColors: "active" })`.

## 7. Expansion log

When an agent adds a token, a primitive or a component pattern (see the skill, "When the system is not enough"), it adds one line here: date, name, why, where it is used.

| Date | Addition | Why | Used in |
| --- | --- | --- | --- |
| 2026-10-22 | v3 tokens and primitives (this file) | the UI refresh | everywhere |
| 2026-10-22 | `.fd-glow`, keyframes `fd-turn` and `fd-breathe`; `.fd-switch` moved from settings.css to base.css | one needs-you glow for every needs-you strip and card; the switch is a primitive (§3) | fleet, sidebar, session approval card; every switch |
| 2026-10-22 | `--scrim` | one dim layer behind every dialog, drawer and sheet (was raw rgba in shell.css and spawn.css) | shell.css `.fd-overlay`, `.fd-drawer-backdrop` |
| 2026-10-22 | `--fs-strip-title` (17px) | §4.5 asks for a 17px session title and no token had that size | session header (session.css `.fd-sh-title`) |
| 2026-10-22 | `--fs-code` (12.5px) | §4.5 says code is mono 12.5px and no token had that size | chat code blocks, inline code (chat.css `.fd-md`) |
| 2026-10-22 | `--fs-display` (56px), the wall scope of the size tokens, `.fd-scope-dark` | the wall is read from a distance and scales with the screen; ambient stays dark in every theme | wall, pair page, ambient |
| 2026-10-22 | `--term-cyan` | the terminal ANSI cyan had no token after the `--m-*` model hues became `--dim` aliases | Terminal.tsx `termTheme` |
| 2026-10-22 | `.fd-dot[data-status=online]` | a link or a sign-in is not work: the turning working arc was wrong for it | stations page header and provider rows |
| 2026-10-22 | `.fd-menu > summary.fd-ghost` | a menu summary that looks like a ghost button; `.fd-menu > summary` beat `.fd-ghost` | chat Show and Agents menus, the station config drift menu |
| 2026-10-22 | `select.fd-sm` keeps padding-right 28px | `.fd-sm` reset the select padding and the chevron overlapped the text | fleet Group by, session files search |
| 2026-10-22 | `--badge-bg`, `--btn-edge` | in the light theme a neutral badge (`--raised-hi`) vanished on `--bg`, and a white plain button vanished on `--surface` and on a `--raised` dialog | every neutral badge and count; plain buttons in panels and dialogs |
| 2026-10-22 | `--ctl-edge` | the `--border` edge of inputs, selects, checkboxes, radios and the off switch was 1.1–1.4:1 (WCAG 1.4.11 asks 3:1); `--border` stays for dividers | every form control; the ring of a pressed segment and chip; Omarchy mapping `mix(fg 50%, bg)` |
| 2026-10-22 | `--pressed-bg`; pressed `.fd-seg` and `.fd-chip` get a `--ctl-edge` ring and weight 600; `.fd-kbd` on `--badge-bg` | in the light theme `--raised-hi` vanished on `--bg` and `--surface`: the selected segment, chip and the key hints did not show | theme segments, sidebar group chips, filter chips, key hints |
| 2026-10-22 | `--switch-knob`, `--knob-shadow`; on knob `--ink-on` | the light knob was near-black (`--text`), and the dark on knob was 1.5:1 on `--ok` | every switch |
| 2026-10-22 | light `--ok` `#0A7048`, `--hazard` `#B52A22`, `--act-line` `#A67C00`; dark `--fuel-bg` `#2A2348` | diff text on its line tint was under 4.5:1; the needs-you outline was under 3:1; the dark meter track did not show on `--surface` | diffs, needs-you strips and cards, usage meters |
| 2026-10-22 | no `tabular-nums` on the `body` (§2.3) | the tabular figures of Mona Sans draw a slashed zero: prose numbers looked like code | everywhere |
| 2026-10-22 | `.fd-avatar` | §4.2 and §4.8 ask for a 24px initials avatar and no primitive had it; `--badge-bg` + `--btn-edge` so it shows on the light `--surface` and `--bg` | sidebar footer (account menu), phone top bar |
| 2026-10-22 | `--selected-bg`, `--selected-lift` | the selected nav or channel row (`--raised-hi`) vanished on the light `--bg` and `--surface`; replaces a `[data-theme=light]` override in views.css | sidebar nav and session rows, activity channels |
| 2026-10-22 | free `.fd-slots` tick is a hollow `--ctl-edge` outline; the `.fd-diff` file header row moved from chat.css to base.css | used and free slots differed by color alone (WCAG 1.4.1); every diff (chat, review, wall) has the same file header | slot meters (fleet, session properties, stations); every diff |
| 2026-10-22 | `.fd-diff` grid column; prose links underlined; input focus 2px blue edge; needs-you dot `--act` edge; ended dot `--faint`; muted badge without edge; coarse chips and `.fd-tap` 44px, checkbox and radio 24px | accessibility review: state by fill alone, link by color alone, 16px and 36px targets, glyphs under 3:1 | every diff, prose link, input, status glyph and touch control |
| 2026-10-22 | dark `--shadow-lift`, `--selected-lift`, `--knob-shadow` are `0 0 0 0 transparent` (not `none`) | `--shadow-lift` dark is a transparent zero shadow, so it can sit in a box-shadow list: with `none` the dark theme dropped every holder, the pressed segment ring and the selection ring of a needs-you card | every strip, card, segment and switch |
| 2026-10-22 | light `--faint` `#596575`; the `.fd-diff` file header row is at most `100cqi` wide | faint text was 4.3–4.4:1 on `--badge-bg` and the diff line fills; on a phone the header was as wide as the longest line and cut the +/− counts | key hints, neutral badges, diff line numbers; every diff header |
| 2026-10-22 | `@media (forced-colors: active)` block; input focus keeps a transparent outline | forced colors drop box-shadow and backgrounds: input focus, the glyph shapes, the pressed segment and the switch state were invisible | every input, status glyph, holder, meter, segment, switch, the terminal dock handle |
| 2026-10-22 | chat Show menu in the tab row | the floating pill covered the result of the tool row under it, and did not read as floating in the dark theme | session chat |
| 2026-10-22 | wall: `--fd-wall-tile-k` on `.fd-wall-grid`, capped at 1.25, shared by the needs area | the tiles read louder than the needs-you requests | wall mission mode |
| 2026-10-22 | flight-path node states for a running and an ended call | §4.5 named only ok, waiting and failed; the wall needs a node for every call | wall chat pane |
| 2026-10-22 | New session: provider segments stack with instance limits; model and mode pills with a leading icon; the sheet on `--bg` on its own route | §4.6 did not say how R4.3 limits show, nor name the pill icons; nothing is behind the sheet on `#/new` | New session sheet |
| 2026-10-22 | `tools/design-check.sh` fails on a shadow token set to `none` | `none` in a box-shadow list drops the whole declaration (§2.5) | tokens.css, palette.ts |
| 2026-10-22 | `--row-h-touch` (44px) | touch list rows used `--tap-min` (a control size) in local media rules; a row height has its own token | the Files rows and other list rows on touch screens |
| 2026-10-22 | `--btn-edge-raised` | in the dark theme a plain button on a `--raised` strip or card had no edge (`--btn-edge` is transparent, and the button has the strip's fill); the dialog rule used `--border` (1.25:1) | plain buttons in dialogs (base.css); the turn strip buttons (session.css) |
| 2026-10-22 | `.fd-kbd` text `--dim`; needs-you `.fd-slots` tick is an "!"; verifying `.fd-dot` has a center dot; a prose link with `.fd-tap` is underlined | review: the light key hints were 4.44:1; needs-you and used slots, and the verifying and working glyphs, differed by hue alone (1.14–1.24:1); the Spaces link was marked by color alone | key hints, slot meters, status glyphs, page descriptions |
| 2026-10-22 | `tools/design-check.sh` fails on `min-height: 0` on a button or summary without a `pointer: coarse` rule | the coarse tap-size rule has zero specificity, so an area reset silently dropped the 44px target | every area style sheet |
| 2026-10-22 | forced colors: `Highlight` ring for a pressed chip, the selected tab and the `aria-current=page` row; checked checkbox and radio in `Highlight`; summary and select chevrons; key hint, badge and count edges | a Playwright forced-colors run of the home, the session, settings and New session: checked and unchecked boxes looked the same, the selected tab and nav row and the pressed chip had no cue, and both chevrons vanished | every checkbox, radio, chip, tab, nav row, details summary, select, key hint and badge |
| 2026-10-22 | ended row = "Ended" + `.fd-count` | the fleet lane said "Ended · N" with a dot and the sidebar "Ended" + a pill: two names for one thing | sidebar, fleet lanes |
| 2026-10-22 | Page `filters` slot (`.fd-page-filters`) | §4.3 puts filters in the header row; Watch and Artifacts had scoped overrides of `.fd-page-actions` | page header (Watch, Artifacts) |
| 2026-10-22 | scroll body fade (mask to `--text`, only while more is below) | a scrolled dialog cut its last heading at the rounded edge with no cue | help dialog, desktop sidebar session list |
| 2026-10-22 | the dock stops at the ops panel (≥ 1280px, `--fd-dock-h` from Layout) | §4.5 rule for the right panel, applied to the fleet home ops panel | fleet home with the terminal dock |
| 2026-10-22 | `.fd-num` table column (right-aligned numbers) | §2.3 says a column of numbers aligns right; tables had no class for it | Shares table (Uses left) |
| 2026-10-22 | forced colors in area files: composer outline, selected station pill outline, reach dots and the chat flight path (line and nodes) keep their color | the forced-colors run (e2e/polish.spec.ts 4a–4c): these parts drew their edge or state only with a box-shadow or a fill | session composer, New session station pills, fleet lanes, sidebar, connection pill |
| 2026-10-22 | diff file status badge (`.fd-dl-status`) | the hidden git plumbing lines also hid that a file was new, deleted or changed its mode | every diff (chat, review, wall) |
| 2026-10-22 | `.fd-iconbtn[aria-pressed=true]` (the pressed-segment look); its forced-colors `Highlight` ring; the session.css and shell.css copies removed | the pressed Properties toggle was `--raised-hi` on `--raised` (1.1:1 dark, 1.07:1 light), and the sidebar compact toggle had a second look for the same state | session header panel toggles, sidebar compact cards, needs-you notifications toggle |
| 2026-10-22 | light `--fuel-bg` `#E2DAFC` | the light meter track (`#EFEAFE`) was 1.04:1 on `--surface`: the meter had no scale | usage meters, fuel badges |
| 2026-10-22 | `--fs-field` (13px; 16px on touch) | fields were 12–15px on a phone: iOS Safari zooms the page into a field under 16px and stays zoomed | every input, select and textarea; the area rules that set a field size (spawn, page, settings, …) |
| 2026-10-22 | `.fd-avatar` is never copied: the sharing stack only adds its overlap; the avatar gets a forced-colors edge | sharing.css copied the primitive and overrode it (no `--btn-edge` ring, invisible in light; a second ring in forced colors) | sidebar footer, phone top bar, sharing avatars |
| 2026-10-22 | `--term-black`, `--term-white`, `--term-bright-*` | the light terminal used `--bg` for black (1.05:1) and dark-theme pastels for the bright colors (bright yellow 1.6:1, bright white invisible) | Terminal.tsx `termTheme` |
| 2026-10-22 | forced colors: the chat flight path (line and nodes) keeps its colors | a filled ok node and a waiting ring differed only by stroke width, and the line vanished (§6: state never by fill alone) | session chat |
| 2026-10-22 | `ui/palette.ts` computes the palette tokens as hex and steps them to the floor | fixed mix steps and raw palette colors gave 1.8–4.3:1 text with Catppuccin Latte, Rose Pine Dawn and Tokyo Night; text tokens now reach 4.5:1 and `--ctl-edge` 3:1 on `--raised-hi`; semantic colors darken (light) or lighten (dark) to keep their hue; a palette whose foreground fails gives no tokens | every screen with an Omarchy palette |
| 2026-10-22 | forced colors: `Highlight` ring for any pressed toggle, selected option, `aria-current=true` row and the needs-you cursor | these states were only a fill or a box-shadow | session header toggles, terminal Ctrl key, session Mode buttons, subagent menu, composer slash menu, palette, home needs-you cursor |
| 2026-10-22 | light `--act` `#7F5C00`, `--fuel` `#6447D6` | they were 4.41 and 4.46:1 on `--badge-bg`, and 4.2–4.4:1 on the diff line fills | needs-you text, fuel badges and numbers, diffs |
| 2026-10-22 | scrollbar thumb `--ctl-edge` (Firefox `scrollbar-color`) | the `--border` thumb was 1.3:1, under 3:1 for an operable control (WCAG 1.4.11) | every scroll area |
| 2026-10-22 | lane needs-you strip: holder + outline, glow on the panel; `.fd-strip-ico` quiet counter form with a name; cleared strip without opacity; sparkline bars `--faint`; "Nothing needs you." with a conflict; foot button "All usage"; rail key hint `a` First `d` Last | fleet review: the lane strip had no outline (§4.1), the chips were drawn two ways, opacity put the cleared text and the `--faint-decor` bars under 4.5:1 and 3:1, "All clear" stood next to a red conflict, two "Usage" labels, the hint named Allow/Deny for any option | fleet home lanes, strips, ops foot, Needs you panel |
| 2026-10-22 | sidebar group count inline after the name | one placement with the Needs you group and the fleet lanes | desktop sidebar station groups |
| 2026-10-22 | approval card: no a/d key hints; the plain option has the `--btn-edge-raised` edge | the session page has no a/d keys (c94d93b9); `--raised-hi` on the white `--raised` card was 1.07:1 in light | session approval card (desktop dock, phone sheet) |
| 2026-10-22 | `--code-chat` (dark `--code`, light `--raised`) | the light `--code` (#F1F5F9) is 1.02:1 on `--bg`, so the chat code block showed only its hairline; a darker light `--code` (#E9EEF4) drops `--faint` on the `--add-bg` diff line to 4.42:1 | chat code blocks (`.fd-md-code`) |
| 2026-10-22 | `summary[data-variant="toggle"]` | stations.css copied the summary chevron to turn it into a trailing chevron-down for the fold toggles (a primitive changed in an area file) | station card folds: Details, Shells, Worktrees, Desktop |
| 2026-10-22 | selected option tint: the composer slash menu uses `--focus-bg` + the 2px `--focus` bar, the same as the palette | §2.2: `--focus-bg` is the selection tint; the two option lists had two looks for one state | composer slash menu, Go to session palette |
| 2026-10-22 | `select.fd-ghost`; `.fd-ghost` and `.fd-iconbtn` set `background-color` (not the shorthand) | home.css restyled the Group by select into a ghost (an area file restyled a primitive), and at rest it had no edge in forced colors; the shorthand would drop the select chevron | fleet home Group by (`fd-ghost fd-sm fd-home-group`) |
| 2026-10-22 | ghost and icon button hover `--badge-bg` on a `--raised` layer | the `--raised` hover on the `--raised` header strip, composer, light code block, approval dock, turn strip and dialogs was 1:1: Interrupt, Files, the session menu, attach, Steer now and the code copy button had no hover | session header, composer, chat code blocks, approval dock, turn strips, dialogs |
| 2026-10-22 | `--fs-sheet-title` (18px), `--fs-sheet-task` (15px) | §4.6 asks for these sizes and spawn.css used the legacy `--fs-18` and `--fs-15`; not in the wall scope (the wall has no New session sheet) | New session sheet title and task fields (spawn.css) |
| 2026-10-22 | forced colors: `.fd-primary` and `.fd-decide` keep a solid `ButtonText` fill | forced colors dropped the fill: Allow, Allow for this session and Deny were three equal outlined buttons, and Start session, Save and Queue follow-up did not stand out | approval card, phone request sheet, needs-you rail, Watch, New session, settings |
| 2026-10-22 | Needs you panel legend: `2`–`9` Option | the middle option of a request showed a number key hint that the legend and §4.4 did not explain | fleet home Needs you panel |
| 2026-10-22 | `.fd-selbar` | the selection marker was an `inset 2px 0 0 var(--focus)` box-shadow that showed as a curved blue bracket on rows with `--r-ctl` corners; the sidebar nav and the palette had their own straight `::before` bar in shell.css | palette, composer slash menu, activity channels, subagent rows, open integration rows, sidebar nav and session rows |
| 2026-10-22 | `.fd-meter[data-tone="neutral"]` | stations.css copied the meter as `.fd-load-bar` with other colors (`--border` track, `--dim`/`--ok` fill) for loads that are not usage: a primitive copied into an area file | station Resources rows (data, memory, CPU), station slot bar over 24 slots |
| 2026-10-22 | notice tone icon (`Info`, `OctagonAlert`, `CircleCheck` in `ui/icons.tsx`); forced-colors notice edge | the tone was only the hue of a box-shadow holder (red/green, WCAG 1.4.1) and forced colors removed it | shell notices and start failures, settings deck and station tabs, Accounts, Omarchy and desktop tools cards |
| 2026-10-22 | New session: chosen station pill = pressed `.fd-seg` look; sheet rules and phone footer rule `--border`; plain sheet buttons `--raised` + `--btn-edge-raised` (phone `--btn-edge`) | one sheet had two chosen looks (the station pill used a `--focus` ring like a focused field); the `--border-soft` rules were 1.06:1 on the dark sheet and 1.05:1 on the light `--bg`; Browse… had a `--border` edge (§2.1) | New session sheet (desktop, phone) |
| 2026-10-22 | menu row hover `--badge-bg` | the `--raised-hi` hover of a menu row, a confirm row, a palette or slash-menu option and a subagent row was 1.07:1 on the light `--raised` layer: no hover in light | every `.fd-menu-body` row, session header confirms, chat Show menu and subagent menu, palette, composer slash menu |
| 2026-10-22 | diff file header path: `.fd-dl-path`, `.fd-dl-dir` (`<bdi>`, `direction: rtl`), `.fd-dl-base` moved from chat.css to base.css | the folders ellipsized at their end and lost the slash (`crates/fleetdeck-station/src/s…reaper.rs` read as a file name); §3 makes the file header a primitive | chat diffs, review, wall DiffPane |
| 2026-10-22 | `.fd-menu > summary.fd-ghost[data-variant="toggle"]`: the toggle chevron in `currentColor` | chat.css copied the chevron mask and the turn of the toggle primitive (with `--t-fast`, not `--t-move`) for the Show menu | chat Show menu |
| 2026-10-22 | §2.2: the sidebar New session does not count as the screen's general primary; on `#/new` it is the current nav row | the New session route had two solid ink buttons (sidebar New session, Start session), and the sidebar button did nothing there; Stations and Shares did not say whether the sidebar button counts | sidebar, New session, Stations, Shares |
| 2026-10-22 | §4.3: the `Page` header (title row + summary under it) and one 24px page gutter | the file described only the one-row fleet bar, and `page.css` called the two-line Page header deliberate; the Page gutter was 32px, the fleet 24px, so the title moved 8px | Stations, Settings, Shares, list pages, fleet home |
| 2026-10-22 | `ui/palette.ts`: `--focus-bg` = 14 % `--focus` tint; `readableAll()` steps `--act`, `--hazard`, `--fuel`, `--focus` and the new palette `--ok` to 4.5:1 on `--raised-hi` and every tint, `--faint`/`--dim` too; `--act-line` to 3:1 on `--bg`, `--raised`, `--act-bg` | the raw `selection` color gave 2.84–3.69:1 text with Catppuccin Latte; tone badges were 4.30–4.48:1 on their tints; the Latte needs-you outline was 2.31:1 on `--bg` | every screen with an Omarchy palette: palette option, slash menu, info and tone badges, terminal dock tab, selected tile and row, needs-you strips |
| 2026-10-22 | light `--fuel-bg` `#E6DFFC`; dark `--add-bg`/`--del-bg` "never on `--raised`"; design-check rule for a diff on a `--raised` fill | `--faint` was 4.42:1 on the light `--fuel-bg`; a diff fill over the dark `--raised` puts `--faint` at 4.17:1 | usage meters, fuel badges, diffs |
| 2026-10-22 | forced colors follow the OS color scheme (`resolvedTheme()`) | with the dark theme on a white HC `Canvas` the kept glyph colors were 1.43–2.13:1 | every screen in forced colors |
| 2026-10-22 | board preference Reduce motion (`data-motion="reduce"`) | the needs-you glow and the working arc moved for as long as the state stayed, with no board control to pause them (WCAG 2.2.2) | fleet, watch, wall, More page |
| 2026-10-22 | palette rules `--border`; palette search `--fs-sheet-task` | the search rule and the key-hint footer rule were `--border-soft` (1.06:1 on the dark `--raised` dialog, so the footer looked like it floated with no divider), the same fix as the New session sheet; the search used the legacy `--fs-15` | command palette (shell.css `.fd-palette > input`, `.fd-palette-foot`) |
| 2026-10-22 | scroll body fade at the top of the chat log (`mask-image: linear-gradient(to bottom, transparent, var(--text) var(--sp-4))`, only while the log is away from its top: `data-scrolled`, set by the scroll handler) | the transcript ended in a hard cut at the session tab rule: the 'File Problem Fix' table header was cut in half and a tool row showed only its bottom 8px | session chat log (chat.css `.fd-chat-scroll[data-scrolled]`) |
| 2026-10-22 | `PathTail` (`chat/PathTail.tsx`, `shortPath`): a mono path drops whole leading folders behind '…/' to fit its box; the CSS start ellipsis (`direction: rtl`) stays as the fallback | the rtl ellipsis cut a mono path in the middle of a word ('…etdeck-station/src/shim/reaper.rs'), which looks like a bug | chat tool row file targets (`.fd-chat-tool-arg[data-path]`); the Properties changed-file list |
| 2026-10-22 | wall flight path: the waiting node has an `--act` center dot, the ended node is a dashed `--faint` ring, and the line has a `.fd-vh` state word | running, waiting and ended were the same ring with only a color change (WCAG 1.4.1), and the `--faint-decor` ended ring was 2.5:1 on the light `--surface` (1.4.11) | wall chat pane (wall.css, wall/ChatPane.tsx) |
| 2026-10-22 | forced colors: `.fd-seg` group edge (`ButtonText`); no rule under an unselected underlined tab | forced colors dropped the track fill, so unpressed segments read as plain text; the transparent rule of every unselected tab showed as a black underline, so all tabs looked selected | New session pills, home panel tabs, every segmented control; session pane tabs (session.css), settings tabs (settings.css) |
| 2026-10-22 | §5 number formats: `countText`, `compactCount`, `usdText`, `addedText`/`removedText` in `ui/format.ts` | the live audit: `$51735.86` and `$14876.41` without separators, two `compactCount` copies (`1.0 M` and `1 M`), `+36952`, `Events 7405`: the same kind of number had a different format on each surface | ops Usage, Properties tokens and cost, fleet strips, turn and file stats, diff headers, session details |
| 2026-10-22 | §5 paths: `homeShort` moved to `ui/format.ts`; `pathParts` | adjacent chat tool rows read `…/luismorales/…` and `/home/luismorales/…`; the folder picker and Files showed a root-less `home/luismorales` next to `/home/luismorales` | chat tool rows, Activity, Watch, Projects, Files, the folder picker, the session header |
| 2026-10-22 | §5 models: `shortModel` without the context suffix; `contextLabel` | `Opus 5.5 1M` in every strip, header and chip: the bare `1M` read as a version or a count | strips, session header, Properties (Context row), composer chip |
| 2026-10-22 | §5 counts after a label: a `.fd-count` pill, never `(N)` | the board had `Ended` with a pill next to `Archived (31)`, `Resume all (2)`, `Clear (2)` and `Filters (1)` | sidebar Archived, fleet offline tools, Filters menu, Archived filter chip |
| 2026-10-22 | `fieldset` and `legend` resets in base.css | a form without an area rule showed the 2px groove frame of the browser and a floating legend (the ops grant and access forms: a box in a box), and the min-content width of the fieldset pushed the 300px ops panel sideways; spawn.css and sharing.css had area copies of the reset | every form; the copies in spawn.css and sharing.css removed |
| 2026-10-22 | `.fd-dialog-head` | the help dialog had an icon X and Share board an absolutely placed `× Close` text button; every dialog drew its own head | help, Share board, Add a device, every dialog |
| 2026-10-22 | `.fd-menu-body > *` does not shrink | a menu taller than its max-height did not scroll: the flex rows shrank to their 16px min-content height (the session menu, also on a phone) | session menu, Terminal menu, account menu, chat Show menu |
| 2026-10-22 | disabled `.fd-primary` and `.fd-decide`: `--badge-bg`, `--faint`, a `--border` hairline, full opacity; forced colors `GrayText` | the ink fill at .45 was a muddy slab (light: white text at 2.3:1) that read as broken, not as "not ready" | Start session, Use this folder, Send, Open, every primary |
| 2026-10-22 | `--badge-on-selected` | in the dark theme `--badge-bg` is the `--selected-bg`: the count on the current Stations row and the `N` hint on the current New session row lost their pill (1:1); the light theme showed them | sidebar nav rows, pressed segments, selected options |
| 2026-10-22 | `input[type=number]` without spin buttons | Chrome and Firefox drew unstyled native spin buttons inside the 160px number fields | settings number fields (Most sessions, Output buffer, limits) |
| 2026-10-22 | `.fd-chip` rest edge (60 % `--ctl-edge`) | an unpressed chip was transparent with a transparent edge: Needs you only, Focus and the fleet filters read as plain text, not as toggles | Watch toggles, fleet filters, every chip |
| 2026-10-22 | `.fd-count[data-tone="info"]`, `.fd-count[data-shape="dot"]` | the unread mark `.fd-unread` was a third count style (a 16px pill) in an area file (views.css), and a bare `1` in a sidebar row read as a station count | unread marks in strips and tiles (pill), sidebar session rows (dot) |
| 2026-10-22 | `Field` `layout="row"` (`.fd-fieldwrap[data-layout="row"]`, page.css) | §4.7 asks for a two-column field grid; settings.css, displays.css and tailnet.css each built their own 200px grid (gaps 24/12/24, three label looks), and Accounts used the stacked form | every settings page |
| 2026-10-22 | §4.8 phone dialogs are bottom sheets | a centered dialog put the Close X out of thumb reach and left an empty band under its last row | every dialog under 640px |
