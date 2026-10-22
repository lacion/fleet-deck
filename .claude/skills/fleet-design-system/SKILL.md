---
name: fleet-design-system
description: Use for ANY change to the Fleet Deck board UI (web/src/**/*.tsx, web/src/styles/*.css, the wall display, e2e screenshots) — new screens, restyles, components, copy. Applies the "Flight Deck" design system (docs/v2/DESIGN-SYSTEM.md), checks the change against it, and says what to do when the system has no token, primitive or pattern for what you need (expand it the right way, never hack around it).
---

# Fleet Deck design system

The board has one design system: **`docs/v2/DESIGN-SYSTEM.md`** ("Flight Deck", v3). Tokens live in `web/src/styles/tokens.css`, primitives in `web/src/styles/base.css`, area styles in the other files of `web/src/styles/`. This skill is the procedure. The design file is the content. When they disagree, the design file wins; fix this skill.

## 1. Before you change UI

1. Read `docs/v2/DESIGN-SYSTEM.md` §1 (principles) and §2 (tokens) completely, and the section of the component or screen you touch (§3 primitives, §4 components).
2. Look at the reference pictures named at the top of that file for the screen you touch.
3. Find the existing primitive or component that already does what you need (`base.css`, then the area CSS, then the components in `web/src/ui/`). Reuse beats new.

## 2. Rules while you build

- **Tokens only.** No raw color, font family or font size in an area CSS file or an inline style. Use `--bg/--surface/--raised/--raised-hi/--code`, `--text/--dim/--faint`, the semantic colors, `--fs-*`, `--sp-*`, `--r-*`.
- **One color, one job.** `--act` (yellow) = needs you and the decision primary only. `--ok` = working/ok/additions. `--hazard` = errors/deny/deletions/offline/conflicts/bypass. `--fuel` (purple) = usage, tokens, cost. `--focus` (blue) = focus ring, selection, links, queued. Models and providers are never colored.
- **Buttons.** General primary = `.fd-primary` (solid ink). Decision primary = `.fd-decide` (yellow). At most one of each per screen (the sidebar New session is chrome and does not count). Everything else is plain, `.fd-ghost` or `.fd-danger`.
- **Type.** Mona Sans for words. `.fd-callsign` for every callsign, `.fd-wide` for station names and page titles. Mono (`--font-data`) only for paths, branches, commands, code, diffs and ids — never for times, counts, models or labels. Sentence case; no capitals, no letter-spaced labels.
- **Sessions are strips** (§4.1): holder + fields with rules. Status uses `.fd-dot` (glyph: shape + color) or `.fd-holder`, never a bare colored text.
- **Depth from fills.** No border around a panel, no box inside a box. Shadows only on floating layers (`--shadow-pop`).
- **Density.** Desktop controls `--ctl-h` 28px, rows `--row-h` 36px, strips `--strip-h` 44px. Touch: 44px (`pointer: coarse`): controls `--tap-min`, list rows `--row-h-touch`. The touch rule of `base.css` has zero specificity: if an area rule sets `min-height: 0` on a button or a summary, give the 44px back in a `@media (pointer: coarse)` rule for the same selector (`tools/design-check.sh` checks it).
- **Edges.** A plain button on a `--raised` surface (a dialog, a strip, a card) has the same fill as the surface: give it `border-color: var(--btn-edge-raised)`.
- **Native controls are styled** by `base.css` (select chevron, checkbox, radio, switch, details marker). Do not add a second style for them.
- **Field text** is `--fs-field` (13px, 16px on touch: iOS Safari zooms into a field under 16px). An area rule that sets the font size of an input, select or textarea uses `--fs-field`; a mono path field keeps `--fs-data` and adds `font-size: max(var(--fs-data), var(--fs-field))` in a `@media (pointer: coarse)` rule.
- **Pressed toggles** (`aria-pressed=true`) take the look of `base.css` (`.fd-seg`, `.fd-chip`, `.fd-iconbtn`); do not give a toggle a second pressed look in an area file. A state shown only by a fill or a box-shadow needs a forced-colors cue (§6).
- **Behavior and tests stay.** Keep roles, accessible names, visible texts and the class names that tests use (`rg "\.fd-" web/e2e`). Add classes; do not rename tested ones. If a test asserts a text you must change, change the test in the same commit and say why.
- **Every state** has a look: hover, focus-visible, pressed/selected, disabled, loading, empty, error — in the dark and the light theme, on desktop and phone.
- **Motion** only answers a user action, plus the two breathing states (needs-you glow, working glyph). Honor `prefers-reduced-motion` and the board preference Reduce motion (`:root[data-motion="reduce"]`): `base.css` stops every animation and transition for both, so an area animation needs no rule of its own.
- **Diffs** sit on `--code` or `--bg`, never on a `--raised` fill (dark: the diff text falls under 4.5:1 there; `tools/design-check.sh` checks it).

## 3. Check your change (always, before you report done)

1. `sh tools/design-check.sh` — must pass (0 violations) for the files you own.
2. `cd web && bun run typecheck && bun run lint` and the unit tests of your area (`bun test src/<area>` through `tools/heavy.sh`).
3. **Look at it.** Build and take the gallery shots of your pages, both themes, desktop and phone:
   ```sh
   cd web
   sh <v2>/tools/heavy.sh bun run build
   FD_GALLERY=1 FD_E2E_PORT=<free port> sh <v2>/tools/heavy.sh ./node_modules/.bin/playwright test e2e/gallery.spec.ts -g "<NN name>"
   ```
   Open the PNGs in `web/test-results/gallery/` with the Read tool. Compare them with the design file and the reference pictures. Fix what differs. (`<v2>` = `/home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2` on the owner's Linux machine.)
4. Review list (answer each with yes): only tokens? yellow only for needs you / decision? no mono for times or counts? no all-caps? strips for sessions? no box in a box? both themes readable (4.5:1)? phone 44px targets? focus visible? empty and error states designed? nothing overflows at 390px and 1440px?

## 4. When the system is not enough

The system is not enough when no token, primitive or pattern expresses a **meaning** that the UI must show (a new state, a new kind of data, a new interaction), or when following a rule makes a screen worse for its user. It is *not* "not enough" when you only want a different look for one place.

Decide with these questions, in order:

1. **Is it a new meaning or only a new place?** A new place of an existing meaning reuses the existing token or primitive. Stop here.
2. **Does an existing piece fit with a variant?** Prefer a `data-*` variant of an existing primitive (`.fd-badge[data-tone=…]`, `.fd-holder[data-status=…]`) over a new class.
3. **Will it be used in two or more places, or is it a new semantic state?** Then **expand the system**:
   - Add the token to `tokens.css` for **both** themes (and to the Omarchy mapping in `web/src/ui/palette.ts` if it is a color that must follow a palette), with a one-line comment of its meaning.
   - Add the primitive to `base.css` (never to an area file), with its hover, focus, disabled and touch states.
   - Document it in `docs/v2/DESIGN-SYSTEM.md` in the right table, and add one line to **§7 Expansion log** (date, name, why, where used).
   - Check contrast in both themes; check that it does not reuse a color with another meaning.
   - Mention the expansion in your report/commit message ("design system: added `--x` because …").
4. **Is it truly a one-off** (a third-party embed, the terminal's ANSI palette, a QR code, a canvas drawing)? Then a **local exception** is allowed: write the raw value with a comment `/* ds-exception: <reason> */` on the same line. `tools/design-check.sh --list` shows every exception; keep them few and explained.
5. **Does a rule of the design file itself look wrong** (it hurts users, it contradicts another rule)? Do not break it silently. Follow it, and write the problem and a proposed change in your report so the architect can change the design file. If you are the architect: change the design file first, log it in §7, then the code.

Never: invent a new color for one screen, copy a primitive into an area file to change it, use `!important` to beat `base.css`, or add a token without documenting it.

## 5. Where things are

| What | Where |
| --- | --- |
| Rules, tokens, components | `docs/v2/DESIGN-SYSTEM.md` |
| Concept pictures (look A, layout B) | `docs/v2/design/ui-refresh/concepts/{a,b}/shots/` |
| Tokens / primitives | `web/src/styles/tokens.css` / `web/src/styles/base.css` |
| Icons (Lucide through one wrapper) | `web/src/ui/icons.tsx` |
| Omarchy palette → tokens | `web/src/ui/palette.ts` |
| Screenshot gallery | `web/e2e/gallery.spec.ts` (`FD_GALLERY=1`) |
| Automatic check | `tools/design-check.sh` (part of `make check`) |
