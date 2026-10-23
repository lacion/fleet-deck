# A new logo for Fleet Deck (B16)

- **Status:** Draft, revision 1 (2026-10-23), design agent of the backlog program. The architect decides. The owner chooses the concept (§7, question 1); the build plan works for every concept.
- **Risk:** normal (static files, one board component, one Omarchy plugin file, docs). No API, protocol, storage or authorization change. One Codex review of the final branch.
- **Owner request (BACKLOG.md B16):** "a strong, distinctive logo for Fleet Deck: a mark and a wordmark that fit the 'Flight Deck' look of the new board."
- **Concepts artifact:** "Fleet Deck logo: 3 concepts" (artifact `01M5MT0K1JCDBX8AXXWZ3726VJ`, version 3). Source and scratch files: `~/fleetdeck-briefs/backlog/logo/` (`gen.py` makes `concepts/*.svg` and `compare.html`; `shots/` has the review screenshots; `recordings/dev1-omarchy-icon-facts.md` has the Omarchy facts).

## 1. Problem and verified facts (HEAD 497b2fbc)

The board has a placeholder mark: an ink tile with the letters "FD". The repository has one old icon file that nothing uses, in a color that the design system does not have. No browser tab, phone home screen, README or desktop bar shows a Fleet Deck logo.

### 1.1 The board

| Fact | Where |
| --- | --- |
| The mark is `Mark({ size = 24 })`: an SVG with a `rect` (`.fd-mark-tile`, rx 6) and a `<text>` "FD" (`.fd-mark-text`), `aria-hidden`. | `web/src/ui/Mark.tsx:1-25` |
| `.fd-mark-tile` fills `--ink-fill`; `.fd-mark-text` fills `--ink-on`, width 115 %, **weight 800**. The wordmark class `.fd-wordmark` is width 115 %, **weight 600**, `--fs-title` (14px). | `web/src/styles/base.css:225-246` |
| The design system says the wordmark is `.fd-wide` (width 115 %, **weight 620**), and the brand row is "24px rounded square, `--ink-fill`, "FD" … weight 800 10px", "Fleet Deck" 14px weight 600. | `docs/v2/DESIGN-SYSTEM.md:126`, `:244` |
| "Yellow is never decoration: not the logo …". | `docs/v2/DESIGN-SYSTEM.md:91` |
| The mark is drawn in **six** places: the desktop brand row, the phone top bar, the tablet top bar, the login page (32px), the wall pairing page (32px) and the wall corner (20px). | `web/src/app/Sidebar.tsx:668-673`, `web/src/app/Layout.tsx:310-311`, `:464-465`, `web/src/runtime/RuntimeApp.tsx:438-439`, `web/src/display/PairPage.tsx:122-123`, `web/src/wall/WallPage.tsx:374-377` |
| The wall zooms the mark with the wall scale; the login page gives it a margin. | `web/src/styles/wall.css:88-90`, `web/src/styles/shell.css:2061-2063` |
| No test asserts the "FD" text, the tile or the mark classes (`git grep` of `web/src`, `web/e2e`). No e2e test compares screenshots. | — |

### 1.2 Icons, favicon and the deck

| Fact | Where |
| --- | --- |
| `web/index.html` has no `<link rel="icon">`, no `apple-touch-icon`, no manifest. | `web/index.html:1-13` |
| `web/public/` does not exist. Vite uses `base: './'` (the board also loads under a path prefix of a reverse proxy). | `web/vite.config.ts:7-13` |
| The deck embeds `web/dist/` (rust-embed). A root file gets `Cache-Control: no-cache`; a missing file with an extension is a 404 (so today a browser's `/favicon.ico` request is a 404). | `crates/fleetdeck-deck/src/http/assets.rs:22-25`, `:166-183` |
| The deck already sends the right type for `.svg`, `.png`, `.ico` and `.webmanifest`, and the board CSP already allows `img-src 'self' data:` and `manifest-src 'self'`. **No deck change is needed.** | `assets.rs:40-42`, `:110-124` |
| `docs/v2/assets/fleet-deck-icon.png` (512 px): three dark cards, an orange outline on the front card, a green dot. Only `BACKLOG.md:182` names it. Its orange is no token of the design system (`DESIGN-SYSTEM.md` §2.2). | `docs/v2/assets/` |
| The README starts with `# Fleet Deck` and the line "One control deck that drives Claude Code and Codex sessions on many machines." The remote is GitHub (`lacion/fleet-deck`): a social preview is uploaded in the repository settings, not read from a file. | `README.md:1-3` |

### 1.3 Omarchy and the desktop (P4)

| Fact | Where |
| --- | --- |
| The bar widget draws the Nerd Font glyph U+F0E8 (a "sitemap" glyph) in `root.tint` (foreground, urgent, or dim). | `crates/fleetdeck-cli/assets/omarchy/plugin/Widget.qml:37-43` |
| The menu rows use Nerd Font glyphs U+F0E8, U+F0AC and U+F188. | `crates/fleetdeck-cli/assets/omarchy/omarchy-menu.jsonc:3-5`, `docs/v2/design/omarchy-p4.md:244-249` |
| The Omarchy menu draws the `icon` field of an extension row as **text** (a glyph). Only `kind: "app"` rows (from `.desktop` files) draw an image. | dev1 recording item 5 (`/usr/share/omarchy/shell/plugins/menu/Menu.qml:1239-1264`) |
| `QtQuick.Shapes` (with `PathSvg`) is installed on Omarchy (qt6-declarative 6.12.0-2), and the Omarchy shell uses it itself (`shell/Ui/BorderOverlay.qml`). | dev1 recording items 1-2 |
| P4 defines **no** `.desktop` launcher for the board. `fleetdeck desktop open` runs `xdg-open <board URL>` (a browser tab, where the favicon shows). The wall and preview windows are Chromium `--app` windows. | `crates/fleetdeck-cli/src/desktop_open.rs:95-109`, `crates/fleetdeck-station/src/desktop/actions.rs:419-421`, `:596-600` |
| `omarchy-webapp-install name url [icon]` makes a launcher; with no icon argument it downloads `<origin>/apple-touch-icon.png`, then falls back to a Google favicon service. | dev1 recording item 4 |
| Notifications use `omarchy-notification-send --app-name "Fleet Deck"` with no icon; the script takes `-i <icon>` (D-Bus `app_icon`) and `-g <glyph>`. | `crates/fleetdeck-cli/src/notify.rs:107`, dev1 recording item 3 |
| A changed plugin file makes the integration "outdated"; the board then offers **Install** (`fleetdeck omarchy install --yes`), which replaces the file. | `crates/fleetdeck-cli/src/omarchy/status.rs:67-97`, `docs/v2/operations.md:504` |
| A test asserts that Widget.qml has at least two `Text` blocks. | `crates/fleetdeck-cli/src/omarchy/assets_qml_tests.rs:117` |

### 1.4 Corrections to BACKLOG.md B16

1. The "FD" letters are SVG text with `.fd-mark-text` (width 115 %, weight 800), not `.fd-wide`. The board wordmark is `.fd-wordmark` with weight **600**; `DESIGN-SYSTEM.md:126` says the wordmark is `.fd-wide` (weight **620**). The code and the design file disagree; this design aligns the code to 620 (§3.2).
2. The mark is not only in the brand row: six places use `ui/Mark.tsx` (§1.1). One component change covers all of them.
3. "The Omarchy and desktop icons (P4, launcher and panel entries)": P4 has no launcher entry. Its icons are font glyphs: the bar widget and the menu rows. The menu can draw only glyphs, so the brand mark can replace only the bar widget glyph.
4. The deck needs no change for icons: the content types and the CSP are already there.

## 2. Answers to the questions of B16

**Q1. Keep the idea of the current icon (a deck of session cards) or start again?** Start again, with the carrier idea. The comparison page shows three concepts:

- **A. Strip deck** keeps the old idea in the new look: three session strips in depth, the front one with its status holder. At 16-24 px it reads as a stack of layers, and the board already uses the Lucide `Layers` icon for the Sessions tab ("one icon, one meaning", fleet-design-system skill). It is the safe choice.
- **B. Angled deck** (recommended): a carrier flight deck seen from above, bow up. The angled landing deck flares out to port at the stern, its centre line is cut out of the deck, and the island stands on the starboard edge. The board look is named after this deck (the jersey colors of §2.2, "Flight Deck"). No other product could use it, and its silhouette (a straight starboard side, a flared port stern) stays clear at 16 px (`shots/b-sizes-2x.png`).
- **C. Formation**: five rounded squares in a V, the deck in the lead. It reads as "network" or "rank" first, and the five nodes blur at 16 px.

**Q2. Concepts first, then the owner chooses?** Yes. The artifact "Fleet Deck logo: 3 concepts" shows each concept at 16, 24, 32, 64 and 512 px in light, dark and two forced-colors simulations, the wordmark and lockup, the brand row and phone bar, a tab strip with the favicon, the app icons with the maskable safe zone, and the Omarchy bar in two palettes. The build does not wait: every artifact of the build comes from one path constant (§3.1), so the choice of A or C changes one string (the paths are in §10).

**Q3. Monochrome first?** Yes, and it is the core of the design. Each mark is one path of filled shapes (even-odd fill rule, no strokes, no masks, no text), on a 24-unit box:

- On the board it fills `currentColor` with `color: var(--text)`, so it follows the light and dark themes, every Omarchy palette (`--text` is a palette token, `ui/palette.ts`) and the wall ambient mode (`.fd-scope-dark`).
- In forced colors the forced `color` (CanvasText) draws it: a Playwright run with `emulateMedia({ forcedColors: 'active' })` drew the three marks in CanvasText (`shots/cmp-forced.png`), while the fixed fills of the old tile kept their colors.
- In the Omarchy bar the same path is a `PathSvg` filled with `root.tint`, the color the glyph has today (foreground, urgent or dim).
- The favicon is the bare mark with a media query (ink on a light browser frame, light on a dark frame); the raster fallbacks put the mark on a dark tile.

## 3. Design

### 3.1 One source: `web/src/ui/brand.ts` (new)

```ts
// The Fleet Deck mark (DESIGN-SYSTEM.md §2.6, DL-461): one path on a 24-unit box, even-odd fill.
// Every brand file comes from this constant: ui/Mark.tsx, tools/brand/render.mjs (favicon, app icons,
// README art, social preview) and the Omarchy bar widget (assets/omarchy/plugin/Widget.qml).
// Concept B, "Angled deck": a carrier flight deck from above, bow up; the angled landing deck flares
// to port at the stern, its centre line is cut out, the island stands on the starboard edge.
export const MARK_PATH =
  'M13.15 2.38Q13.43 1.11 14.73 1.11L16.75 1.11Q18.05 1.11 18.72 2.22L19.36 3.3Q20.03 4.41 20.03 5.71L20.03 21.59Q20.03 22.89 18.73 22.89L9.23 22.89Q7.93 22.89 7.13 21.87L4.77 18.85Q3.97 17.83 4.79 16.82L11.07 9.16Q11.89 8.15 12.17 6.88ZM9.9 21.68L11.95 16.33L10.2 15.67L8.16 21.02ZM12.77 14.16L14.24 10.31L12.49 9.65L11.03 13.49ZM15.01 8.31L15.96 5.8L14.22 5.13L13.26 7.64ZM16.18 9.91L18.27 9.91L18.27 15.41L16.18 15.41Z';

/** The box of MARK_PATH. */
export const MARK_BOX = 24;
```

Rules for `MARK_PATH`: one line, single quotes, only the commands `M L Q A H V Z` (upper case, absolute), every number in `0..24`. The fill rule is always `evenodd` (concepts A and C have no overlapping parts, so even-odd draws them the same).

### 3.2 The board mark and wordmark

`web/src/ui/Mark.tsx` (whole file):

```tsx
import { MARK_BOX, MARK_PATH } from './brand';

/** The Fleet Deck mark (DESIGN-SYSTEM.md §2.6): one path in the text color; the wordmark next to it names it. */
export function Mark({ size = 24 }: { readonly size?: number }) {
  return (
    <svg
      className="fd-mark"
      width={size}
      height={size}
      viewBox={`0 0 ${MARK_BOX} ${MARK_BOX}`}
      aria-hidden="true"
      focusable="false"
    >
      <path fillRule="evenodd" d={MARK_PATH} />
    </svg>
  );
}
```

`web/src/styles/base.css`: replace the block at lines 233-246 (the comment, `.fd-mark`, `.fd-mark-tile`, `.fd-mark-text`) with:

```css
/* The brand mark (ui/Mark.tsx, DESIGN-SYSTEM §2.6): one path in the text color. Forced colors
   force `color`, so the mark draws in CanvasText with no rule of its own. */
.fd-mark {
  flex: none;
  display: block;
  color: var(--text);
}
.fd-mark path {
  fill: currentColor;
}
```

and in `.fd-wordmark` (line 228) `font-weight: 600;` becomes `font-weight: 620;` (the `.fd-wide` weight of `DESIGN-SYSTEM.md:126`).

Nothing else changes in the six places of §1.1: the same component, sizes and classes. The phone top bar, the login page and the wall corner get the new mark through it.

### 3.3 Logo rules (new `DESIGN-SYSTEM.md` §2.6 "Logo")

Exact text for the design file (task T1):

> ### 2.6 Logo *(new)*
>
> - **The mark** is one path on a 24-unit box (`MARK_PATH` in `web/src/ui/brand.ts`, even-odd fill). Concept B "Angled deck": a carrier flight deck from above, bow up. It has one color: on the board `currentColor` with `color: var(--text)` (`ui/Mark.tsx`, `.fd-mark`). Never yellow, never a semantic color, never on a tile on the board, never outlined, rotated or stretched.
> - **Sizes:** 16px is the smallest. The board uses 24px (brand row, phone and tablet top bars), 32px (login, wall pairing) and 20px (wall corner, zoomed with the wall).
> - **The wordmark** is the text "Fleet Deck" in Mona Sans, width 115 %, weight 620 (`.fd-wordmark`, the `.fd-wide` cut), sentence case, letter spacing 0. On the board it is live text; in images it is rendered from the bundled font.
> - **The lockup** is the mark, then the wordmark on the same centre line. The mark box is 1.7 times the font size of the wordmark, and the gap is a third of the mark box (the brand row: 24px, 8px, 14px). Keep a clear space of half the mark box around a lockup.
> - **Tiles** only outside the board: app icons, the favicon fallback and the social preview use the mark in `#E9EEF4` on a `#111821` tile (the dark `--text` on the dark `--bg`). `tools/brand/render.mjs` makes every brand file from `MARK_PATH`; never edit a brand PNG or SVG by hand.

Line 244 (§4.2 Brand row) becomes: "1. Brand row (48px): the mark (§2.6, 24px, `--text`), the wordmark "Fleet Deck" (`.fd-wordmark`: 14px, width 115 %, weight 620), the connection pill at the right (`Live`: `--ok` dot; `Stale`: `--act`; `Offline`: `--hazard`)."

One row at the end of §7 (Expansion log):

| Date | Change | Why | Where |
| --- | --- | --- | --- |
| (merge date) | the Fleet Deck mark (§2.6, `ui/brand.ts`, `ui/Mark.tsx`) replaces the "FD" tile; `.fd-wordmark` weight 620 | B16: a logo that no other product could use; the tile was a placeholder, and the wordmark had weight 600 against the 620 of `.fd-wide` | brand row, phone and tablet top bars, login, wall pairing, wall corner |

### 3.4 Brand files (generated) and where they live

| File | Size | Content | Used by |
| --- | --- | --- | --- |
| `web/public/favicon.svg` | viewBox 24 | the bare mark; `<style>path{fill:#111821}@media (prefers-color-scheme:dark){path{fill:#E9EEF4}}</style>` | the tab of the board and the wall |
| `web/public/favicon.ico` | 16, 32, 48 | three PNG images in one ICO: the mark (box 80 % of the side) on a `#111821` tile with corner radius 22.5 % | browsers that ask `/favicon.ico`, Safari |
| `web/public/apple-touch-icon.png` | 180×180 | full-bleed `#111821` square (iOS rounds it), mark box 62 % centred, `#E9EEF4` | iOS home screen; `omarchy-webapp-install` |
| `web/public/icon-192.png`, `icon-512.png` | 192, 512 | the tile (radius 22.5 %, transparent corners), mark box 62 % | manifest, `purpose: any` |
| `web/public/icon-maskable-512.png` | 512 | full-bleed `#111821`, mark box 52 % (inside the 80 % safe circle) | manifest, `purpose: maskable` |
| `web/public/manifest.webmanifest` | — | §3.5 | Android "Add to home screen", desktop Chromium |
| `docs/v2/assets/brand/mark.svg` | viewBox 24 | the mark, fill `#111821` | docs, other uses |
| `docs/v2/assets/brand/mark-dark.svg` | viewBox 24 | the mark, fill `#E9EEF4` | docs on a dark ground |
| `docs/v2/assets/brand/icon.svg` | 512 | the tile of `icon-512.png` as SVG | source of the app icon |
| `docs/v2/assets/brand/lockup-light.png`, `lockup-dark.png` | 640×128 (2× of 320×64) | transparent; mark box 56 at y 4; gap 19; wordmark 33px; ink `#111821` / `#E9EEF4` | README header |
| `docs/v2/assets/brand/social-preview.png` | 1280×640 | §3.7 | GitHub social preview (owner upload) |

`docs/v2/assets/fleet-deck-icon.png` is removed (DL-468). Git keeps it in the history.

The wordmark is not shipped as an outlined SVG: no outline tool is on the machine (no fontTools, no Inkscape), and GitHub images cannot load a web font. The README uses PNGs instead (DL-462).

### 3.5 `web/index.html` and the manifest

`web/index.html`, after the `<title>` (line 6):

```html
    <link rel="icon" href="/favicon.ico" sizes="16x16 32x32 48x48" />
    <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
    <link rel="apple-touch-icon" href="/apple-touch-icon.png" />
    <link rel="manifest" href="/manifest.webmanifest" />
```

Vite copies `web/public/` to the root of `dist/` and, with `base: './'`, writes these URLs relative (`./favicon.svg`) in `dist/index.html`, so a path prefix of a reverse proxy keeps working. If the build keeps the leading `/`, the worker stops and asks (a decision, not a fix).

`web/public/manifest.webmanifest`:

```json
{
  "name": "Fleet Deck",
  "short_name": "Fleet Deck",
  "description": "One control deck that drives Claude Code and Codex sessions on many machines.",
  "start_url": "./",
  "scope": "./",
  "display": "browser",
  "background_color": "#111821",
  "theme_color": "#111821",
  "icons": [
    { "src": "icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any" },
    { "src": "icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any" },
    { "src": "icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" },
    { "src": "favicon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any" }
  ]
}
```

`display: "browser"` keeps today's behavior: a home-screen icon opens the board in the browser, with the browser's cookies (an iOS standalone app has its own cookie store and would ask for a new login). Owner question 2 asks about `standalone`. No `<meta name="theme-color">`: the board has three themes and Omarchy palettes, and one fixed color would be wrong in most of them.

### 3.6 The render tool: `tools/brand/render.mjs` (new)

Run: `cd web && node ../tools/brand/render.mjs` (it is not a heavy command; it starts one headless Chromium for a few seconds).

1. Read `web/src/ui/brand.ts`; take `MARK_PATH` with `/export const MARK_PATH =\s*'([^']+)'/`. No match: print "render: MARK_PATH not found in web/src/ui/brand.ts" and exit 1.
2. Constants: `INK = '#111821'`, `PAPER = '#E9EEF4'`, `DIM = '#A5B0BF'` (the dark `--bg`, `--text`, `--dim`; this file is outside `web/src/styles`, so `tools/design-check.sh` does not scan it).
3. Write the SVG files of §3.4 as text (UTF-8, one line, `\n` at the end). A tile SVG is `<rect width="S" height="S" rx="S*0.225" fill="#111821"/>` plus `<path transform="translate(o o) scale(k)" fill="#E9EEF4" fill-rule="evenodd" d="…"/>` with `k = S*f/24`, `o = S*(1-f)/2`.
4. Load Playwright with `createRequire(<v2>/web/package.json)('playwright')` and `chromium.launch()` (the browser that the e2e tests use; no new dependency).
5. For each PNG: `page.setContent` of a page whose only element is a `div#shot` of the exact CSS size that holds the SVG (or, for the lockups and the social preview, the HTML of §3.4/§3.7), with `@font-face { font-family: 'Mona Sans Variable'; src: url(data:font/woff2;base64,…) format('woff2-variations'); font-weight: 200 900; font-stretch: 75% 125%; }` read from `web/node_modules/@fontsource-variable/mona-sans/files/mona-sans-latin-standard-normal.woff2`; wait for `document.fonts.ready`; then `page.locator('#shot').screenshot({ path, omitBackground })` (`omitBackground: true` for the tile icons with transparent corners and the lockups). Device scale factor 1, except the lockups: 2.
6. `favicon.ico`: render the 16, 32 and 48 px tiles to buffers, then write the ICO: a 6-byte header (`reserved 0`, `type 1`, `count 3`, little-endian u16), three 16-byte entries (`width`, `height`, `0`, `0`, `planes 1` u16, `bit count 32` u16, `size` u32, `offset` u32), then the three PNG buffers in that order.
7. Print one line per file: `wrote <path> (<bytes> bytes)`.

The PNGs are committed. A change of `MARK_PATH` means: run the tool, commit the result. The bun test of T2 fails while an SVG does not carry the current path.

### 3.7 README header and social preview

`README.md` line 1 (`# Fleet Deck`) becomes:

```html
<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/v2/assets/brand/lockup-dark.png">
    <img alt="Fleet Deck" src="docs/v2/assets/brand/lockup-light.png" width="320" height="64">
  </picture>
</h1>
```

The H1 keeps the name for screen readers (the `alt`). The bold tagline (line 3) stays.

The social preview (1280×640, device scale 1): background `#111821`; the lockup in `#E9EEF4` with a 136px mark box, a 45px gap and an 80px wordmark, left edge 112px, centre line at y 280; under it, from x 112 and y 392, the tagline "One control deck that drives Claude Code and Codex sessions on many machines." in Mona Sans 30px, weight 450, `#A5B0BF`, at most 1056px wide (two lines). No other element, no yellow. The owner uploads it: GitHub → the repository → Settings → General → Social preview (owner step after the merge).

### 3.8 The Omarchy bar widget

`crates/fleetdeck-cli/assets/omarchy/plugin/Widget.qml`:

1. Add `import QtQuick.Shapes` after `import QtQuick` (line 1).
2. Replace the first `Text` block of the `Row` (lines 37-43, the glyph `"\uf0e8"`) with:

```qml
    // The Fleet Deck mark (web/src/ui/brand.ts MARK_PATH), in the tint of the widget.
    Shape {
      id: mark
      width: Style.font.icon
      height: Style.font.icon
      anchors.verticalCenter: parent.verticalCenter
      preferredRendererType: Shape.CurveRenderer
      ShapePath {
        fillColor: root.tint
        fillRule: ShapePath.OddEvenFill
        strokeWidth: -1
        scale: Qt.size(mark.width / 24, mark.height / 24)
        PathSvg { path: "<MARK_PATH, the same string>" }
      }
    }
```

Nothing else changes: the counts text, the clicks and the colors stay. The menu rows keep their Nerd Font glyphs (the menu draws only glyphs, §1.3). `assets_qml_tests.rs:117` changes from `>= 2` to `>= 1` Text blocks.

A station whose plugin was installed before this change shows "outdated" on the board; **Install** replaces `Widget.qml` (§1.3). Until then the old glyph stays. This is the existing upgrade path of every plugin change.

### 3.9 The operations guide

`docs/v2/operations.md` §7.5, in the "A bar widget and a panel" bullet (line 412), after "The widget shows": "the Fleet Deck mark, the number of working sessions …". After the bullet list (after line 417), a new paragraph:

> **A launcher for the board (optional).** Fleet Deck installs no app launcher. To add one to the Omarchy app menu, run on the station: `omarchy-webapp-install "Fleet Deck" <board URL> <board URL>/apple-touch-icon.png`. Give the icon URL: without it, the script asks a public favicon service for the icon of your board's address.

## 4. Errors and edge cases

| Case | Behavior |
| --- | --- |
| A browser without SVG favicons (Safari) | It uses `favicon.ico` or `apple-touch-icon.png` (the tile). |
| The browser frame color differs from `prefers-color-scheme` | The SVG favicon follows the scheme; at worst the ink is low on its frame. The tile fallback is not used then. Accepted. |
| Forced colors | The board mark draws in CanvasText (forced `color`). No area rule is needed (DESIGN-SYSTEM §6: the state is not a fill or a shadow). |
| An Omarchy palette | `--text` follows the palette; the mark follows `--text`. |
| The wall ambient mode | `.fd-scope-dark` sets the dark `--text`. |
| A reverse proxy path prefix | Relative URLs in `dist/index.html` and `start_url: "./"`. |
| A deck without board files | Every file is a 404, as today (`assets.rs:184-195`). |
| `/favicon.ico` on an old deck | 404, as today. |
| An Omarchy without `QtQuick.Shapes` | The widget would not load. Omarchy's own shell needs the module (§1.3), so an Omarchy that runs its shell has it. |
| The owner chooses A or C | Replace `MARK_PATH` (§10), run the render tool, update `Widget.qml`; the tests check the three places. |

## 5. Compatibility

- No protocol, API, storage or feature-flag change: older stations and decks are not affected. The board files are embedded in the deck, so a deck build carries the new icons.
- The Omarchy widget changes only through the existing "outdated → Install" path (§3.8). An older station binary keeps its old widget.
- Browsers cache favicons on their own; root files are `no-cache` (`assets.rs:171-175`), so the next visit revalidates them.

## 6. Out of scope

- A Fleet Deck `.desktop` launcher written by `fleetdeck omarchy install`: it needs files under the data home (`~/.local/share/applications`, `~/.local/share/icons`), but the installer writes only under the config home (`Base::Omarchy`, `Base::Fleetdeck`, `omarchy/assets.rs:8-26`), and a new file needs a new `OmarchyAssetId` in the repair protocol (`fleetdeck-proto/src/omarchy_repair.rs:33-46`). A follow-up if the owner wants it (owner question 3). The operations guide names the manual way (§3.9).
- A notification icon (`omarchy-notification-send -i`): it needs the same installed icon file.
- macOS (`.icns`, launchd) icons, a Windows icon, an animated mark, a mark for the wall ambient screen, an outlined-SVG wordmark, a service worker or a standalone PWA, a docs site.

## 7. Owner questions

1. **Which concept?** A (Strip deck), B (Angled deck) or C (Formation), in the artifact "Fleet Deck logo: 3 concepts". **Default: B.** The build uses B until the owner answers; a later choice changes one path (§10).
2. **Should a phone open the board as an app (manifest `display: "standalone"`, no browser bar)?** On iOS a standalone app has its own cookies, so the owner logs in once more inside it. **Default: no (`browser`, today's behavior).**
3. **Should `fleetdeck omarchy install` also add a "Fleet Deck" launcher to the Omarchy app menu (and an icon for notifications)?** **Default: not now** (§6; the guide names `omarchy-webapp-install`).

## 8. Risk class: normal

Static files, one board component and its CSS, one Omarchy plugin file and its test, docs. No authentication, authorization, data, migration, cost, personal data, public API or command path. The new public files (icons, manifest) hold no data, as the other board assets (`assets.rs:9-11`). One `codex-run review` of the final branch; no adversary.

## 9. Build plan

Order: T1 first; then T2 and T3 in parallel (no shared file); then T4. Every heavy command runs through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <cmd>` (below: `heavy`). UI tasks follow `docs/v2/DESIGN-SYSTEM.md` (skill `fleet-design-system`).

### T1. The mark in the board (ux, about 160 lines)

- **Owned files:** `web/src/ui/brand.ts` (new), `web/src/ui/Mark.tsx`, `web/src/ui/brand.test.tsx` (new), `web/src/styles/base.css`, `web/e2e/brand-mark.spec.ts` (new), `docs/v2/DESIGN-SYSTEM.md`.
- **Changes:** §3.1 (brand.ts, exact text), §3.2 (Mark.tsx whole file; base.css lines 233-246 and the `.fd-wordmark` weight), §3.3 (DESIGN-SYSTEM.md: the new §2.6 after §2.5, line 244, one §7 row).
- **Tests:**
  - `brand.test.tsx` "the mark path uses absolute commands and stays in its box": the path matches `/^[MLQAHVZ0-9. -]+$/`, starts with `M`, and every number (except the arc flags of an `A`) is in `0..24`.
  - `brand.test.tsx` "Mark draws one even-odd path and no text": `renderToStaticMarkup(<Mark />)` holds one `<path`, `fill-rule="evenodd"`, `d="<MARK_PATH>"`, `width="24"`, `viewBox="0 0 24 24"`, `aria-hidden="true"`, and no `<text` and no `<rect`; `<Mark size={32} />` has `width="32"`.
  - `brand-mark.spec.ts` (phone project) "the login mark draws in the text color in light, dark and forced colors": a `FakeDeck` installed, `page.goto('/')` (the login page); for `emulateMedia({ colorScheme: 'light' })`, `{ colorScheme: 'dark' }` and `{ forcedColors: 'active', colorScheme: 'light' }`: the computed `fill` of `.fd-login .fd-mark path` equals the computed `color` of `.fd-login h1`.
- **Depends on:** none.
- **Acceptance:**
  - `cd web && heavy bun test src/ui/brand`
  - `cd web && bun run typecheck && bun run lint`
  - `sh tools/design-check.sh`
  - `cd web && heavy bun run build && FD_E2E_PORT=<free port> heavy ./node_modules/.bin/playwright test e2e/brand-mark.spec.ts`
  - Gallery look, both themes: `cd web && FD_GALLERY=1 FD_E2E_PORT=<free port> heavy ./node_modules/.bin/playwright test e2e/gallery.spec.ts -g "01 home"`; read the PNGs in `web/test-results/gallery/`: the brand row shows the new mark in `--text`, 24px, next to "Fleet Deck".
  - `heavy cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T2. Favicon, app icons, manifest, README art, social preview (ux, about 330 lines of text plus generated PNG files)

- **Owned files:** `tools/brand/render.mjs` (new); `web/public/favicon.svg`, `web/public/favicon.ico`, `web/public/apple-touch-icon.png`, `web/public/icon-192.png`, `web/public/icon-512.png`, `web/public/icon-maskable-512.png`, `web/public/manifest.webmanifest` (all new); `web/index.html`; `docs/v2/assets/brand/mark.svg`, `mark-dark.svg`, `icon.svg`, `lockup-light.png`, `lockup-dark.png`, `social-preview.png` (all new); `docs/v2/assets/fleet-deck-icon.png` (delete); `web/src/ui/brand_assets.test.ts` (new); `web/e2e/brand-assets.spec.ts` (new).
- **Changes:** write `render.mjs` (§3.6), run it, commit its output (§3.4, §3.7 social preview); `index.html` (§3.5); `manifest.webmanifest` (§3.5, exact text; the tool does not write it); `git rm docs/v2/assets/fleet-deck-icon.png`.
- **Tests (`brand_assets.test.ts`, reads files with `readFileSync` and `import.meta.dir` like `ui/palette.test.ts:232`):**
  - "the brand SVGs carry the mark path": `web/public/favicon.svg` and `docs/v2/assets/brand/{mark,mark-dark,icon}.svg` contain `d="<MARK_PATH>"` and `fill-rule="evenodd"`; `favicon.svg` contains `prefers-color-scheme:dark`.
  - "the manifest lists icons that exist at their sizes": the JSON has `name` "Fleet Deck", `display` "browser", `start_url` "./"; each PNG icon exists and its IHDR width and height (bytes 16-23, big-endian) equal its `sizes`; exactly one icon has `purpose` "maskable".
  - "the other PNG files have their sizes": `apple-touch-icon.png` 180×180, both lockups 640×128, `social-preview.png` 1280×640.
  - "favicon.ico holds 16, 32 and 48 px PNG images": type 1, count 3, entry widths 16, 32, 48, each image starts with the PNG signature `89 50 4E 47`.
  - "index.html links the icons and the manifest": the four `<link>` lines of §3.5.
  - "the old icon is gone": `docs/v2/assets/fleet-deck-icon.png` does not exist.
  - `brand-assets.spec.ts` (phone project) "the board serves its icons and manifest": after `page.goto('/')` with a `FakeDeck`, the `href` of `link[rel=icon][type="image/svg+xml"]` answers 200 with a content type that contains `svg`, and the `link[rel=manifest]` URL answers 200 with JSON whose `name` is "Fleet Deck".
- **Depends on:** T1 (`brand.ts`).
- **Acceptance:**
  - `cd web && node ../tools/brand/render.mjs` prints one `wrote` line per file, and a second run gives the same SVG text (`git diff --stat` shows no SVG change).
  - `cd web && heavy bun test src/ui/brand_assets`
  - `cd web && bun run typecheck && bun run lint`
  - `cd web && heavy bun run build`; `ls web/dist/favicon.svg web/dist/manifest.webmanifest`; `grep -c 'favicon.svg' web/dist/index.html` is 1 and the URL is relative (§3.5).
  - `cd web && FD_E2E_PORT=<free port> heavy ./node_modules/.bin/playwright test e2e/brand-assets.spec.ts`
  - Look: read `docs/v2/assets/brand/social-preview.png`, both lockups and `icon-maskable-512.png` with the Read tool.
  - `sh tools/design-check.sh`
  - `heavy cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T3. The Omarchy bar widget draws the mark (backend, about 50 lines)

- **Owned files:** `crates/fleetdeck-cli/assets/omarchy/plugin/Widget.qml`, `crates/fleetdeck-cli/src/omarchy/assets_qml_tests.rs`.
- **Changes:** §3.8 (the `Shape` block with the exact `MARK_PATH` of `web/src/ui/brand.ts`); `assets_qml_tests.rs:117`: `>= 2` becomes `>= 1`.
- **Tests (`assets_qml_tests.rs`):**
  - "the_widget_draws_the_board_mark": read `concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/ui/brand.ts")` with `std::fs::read_to_string`, take the text between the first `'` after `MARK_PATH =` and the next `'`, and assert that Widget.qml contains `PathSvg { path: "<that text>" }`, `import QtQuick.Shapes`, `fillColor: root.tint`, `fillRule: ShapePath.OddEvenFill`, and does not contain `\uf0e8`.
  - The menu golden test (`assets_tests.rs:117`) stays unchanged: the menu keeps U+F0E8.
- **Depends on:** T1.
- **Acceptance:**
  - `heavy cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'test(/omarchy::assets/)'`
  - `heavy cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - Live smoke (the architect, after the merge, on dev1): board → Settings → dev1 → Omarchy → **Install** (the integration shows "outdated"); take a monitor screenshot with the coordinator tool; the bar shows the mark in the foreground color, and in the urgent color while a session needs you.

### T4. README header and the operations guide (ux, about 25 lines)

- **Owned files:** `README.md`, `docs/v2/operations.md`.
- **Changes:** §3.7 (README line 1); §3.9 (operations §7.5).
- **Tests:** none (docs). Check the README on GitHub after the push is an owner step.
- **Depends on:** T2 (the lockup files).
- **Acceptance:** `test -f docs/v2/assets/brand/lockup-light.png && test -f docs/v2/assets/brand/lockup-dark.png`; `grep -n 'lockup-dark.png' README.md`; `grep -n 'apple-touch-icon.png' docs/v2/operations.md`.

## 10. The three paths (for owner question 1)

All on a 24-unit box, fill rule even-odd.

- **A. Strip deck:** `M8.25 3H15.75A1.25 1.25 0 0 1 17 4.25V5.25A1.25 1.25 0 0 1 15.75 6.5H8.25A1.25 1.25 0 0 1 7 5.25V4.25A1.25 1.25 0 0 1 8.25 3ZM6 8.5H18A1.5 1.5 0 0 1 19.5 10V11.5A1.5 1.5 0 0 1 18 13H6A1.5 1.5 0 0 1 4.5 11.5V10A1.5 1.5 0 0 1 6 8.5ZM3.5 15H4A1.5 1.5 0 0 1 5.5 16.5V20A1.5 1.5 0 0 1 4 21.5H3.5A1.5 1.5 0 0 1 2 20V16.5A1.5 1.5 0 0 1 3.5 15ZM8.75 15H20.25A1.75 1.75 0 0 1 22 16.75V19.75A1.75 1.75 0 0 1 20.25 21.5H8.75A1.75 1.75 0 0 1 7 19.75V16.75A1.75 1.75 0 0 1 8.75 15Z`
- **B. Angled deck:** the `MARK_PATH` of §3.1.
- **C. Formation:** `M10.5 2H13.5A2 2 0 0 1 15.5 4V7A2 2 0 0 1 13.5 9H10.5A2 2 0 0 1 8.5 7V4A2 2 0 0 1 10.5 2ZM5.6 10H7.9A1.6 1.6 0 0 1 9.5 11.6V13.9A1.6 1.6 0 0 1 7.9 15.5H5.6A1.6 1.6 0 0 1 4 13.9V11.6A1.6 1.6 0 0 1 5.6 10ZM16.1 10H18.4A1.6 1.6 0 0 1 20 11.6V13.9A1.6 1.6 0 0 1 18.4 15.5H16.1A1.6 1.6 0 0 1 14.5 13.9V11.6A1.6 1.6 0 0 1 16.1 10ZM2 17H4A1.5 1.5 0 0 1 5.5 18.5V20.5A1.5 1.5 0 0 1 4 22H2A1.5 1.5 0 0 1 0.5 20.5V18.5A1.5 1.5 0 0 1 2 17ZM20 17H22A1.5 1.5 0 0 1 23.5 18.5V20.5A1.5 1.5 0 0 1 22 22H20A1.5 1.5 0 0 1 18.5 20.5V18.5A1.5 1.5 0 0 1 20 17Z`

A and C use arcs (`A`): the path test of T1 allows `A`, and `PathSvg` reads arcs.

## 11. Decisions made in this draft

1. **Recommend concept B (Angled deck).** Rejected: A (reads as layers and collides with the Sessions tab icon), C (generic network look, weak at 16px).
2. **One path constant in `web/src/ui/brand.ts` is the source of every brand file.** Rejected: hand-made SVG and PNG files per use (they drift; a concept change would touch every file).
3. **The board mark is the bare mark in `--text`, no tile.** Rejected: keep the ink tile with the new mark inside (a second shape around the brand; the tile fill does not follow forced colors).
4. **Fill rule even-odd, filled shapes only (no strokes, masks or text).** Rejected: strokes and masks (QML `PathSvg` and favicons draw fills reliably; a stroke changes weight with the scale).
5. **One wordmark for all concepts: Mona Sans, width 115 %, weight 620, sentence case; `.fd-wordmark` goes from 600 to 620.** Rejected: a heavier or wider display wordmark (a second rule; the concept page first showed 120 %/660 for B).
6. **Lockup: mark box 1.7 times the font size, gap a third of the box, clear space half the box.** Rejected: the 1.33 ratio of the first concept page (it does not match the brand row).
7. **The favicon SVG is the bare mark with a color-scheme media query; the raster fallbacks are a dark tile.** Rejected: a tile favicon everywhere (the mark is two thirds of 16px inside a tile).
8. **The manifest uses `display: "browser"`.** Rejected: `standalone` (a behavior change and a second login on iOS; owner question 2).
9. **No `theme-color` meta.** Rejected: one fixed color (wrong in the light theme and in Omarchy palettes).
10. **PNGs come from the Playwright Chromium of `web/node_modules`, by `tools/brand/render.mjs`, and are committed; the ICO is written by the script.** Rejected: ImageMagick (`magick` is in Homebrew on this machine but not a project tool, and it cannot render the bundled variable font), generating at build time (the build would need Chromium).
11. **The README header is a `<picture>` of two lockup PNGs inside the H1.** Rejected: an SVG lockup (GitHub images cannot load Mona Sans, and no outline tool is on the machine), the plain `# Fleet Deck` with a separate image (the name twice).
12. **The social preview is a committed PNG; the owner uploads it in GitHub.** Rejected: an `og:image` in the board (the board is a private deck; no crawler reads it).
13. **The Omarchy bar widget draws the mark with `QtQuick.Shapes` + `PathSvg`, tinted with `root.tint`; the menu keeps its Nerd Font glyphs.** Rejected: an `Image` of an SVG file (a new installed file, a new `OmarchyAssetId`, and a tint effect), a custom icon font.
14. **No Fleet Deck launcher or notification icon in this build; the guide names `omarchy-webapp-install` with the explicit icon URL.** Rejected: a launcher written by the installer now (new data-home base and repair protocol change), the command without the icon URL (it asks a public favicon service for the board's address).
15. **Remove `docs/v2/assets/fleet-deck-icon.png`.** Rejected: keep it (two logos, an orange that is not in the design system).
16. **The brand files live in `web/public/` (served) and `docs/v2/assets/brand/` (repository and docs).** Rejected: one folder (the deck embeds only `web/dist`, and the README should not point into the web build).

## Decision log entries (to append)

## DL-460: The Fleet Deck logo is concept B, "Angled deck" (B16) (design agent, 2026-10-23)

- A carrier flight deck from above, bow up: the angled landing deck flares to port at the stern, its centre line is cut out, the island stands on the starboard edge. The owner chooses among A (Strip deck), B and C (Formation) in the artifact "Fleet Deck logo: 3 concepts"; the build uses B until then. Design `docs/v2/design/logo.md` §2.

## DL-461: One mark path in `web/src/ui/brand.ts` is the source of every brand file (B16) (design agent, 2026-10-23)

- `MARK_PATH` (24-unit box, even-odd fill, filled shapes only). `ui/Mark.tsx` draws it in `currentColor` with `color: var(--text)`, no tile; forced colors draw it in CanvasText. `tools/brand/render.mjs` makes the favicon, the app icons and the README and social images from it; the Omarchy widget carries the same string, and tests check the three places. Design §3.1-§3.2.

## DL-462: The wordmark is Mona Sans 115 %, weight 620, sentence case; raster in images (B16) (design agent, 2026-10-23)

- `.fd-wordmark` goes from weight 600 to 620 (the `.fd-wide` cut of DESIGN-SYSTEM §2.3). Lockup: mark box 1.7 times the font size, gap a third of the box. No outlined SVG wordmark: the README uses two lockup PNGs in a `<picture>`. Design §3.3, §3.7.

## DL-463: Favicon, app icons and a manifest with `display: "browser"` (B16) (design agent, 2026-10-23)

- `favicon.svg` (the bare mark with a color-scheme media query), `favicon.ico` (16, 32, 48 px tiles), `apple-touch-icon.png` (180), `icon-192.png`, `icon-512.png`, `icon-maskable-512.png`, `manifest.webmanifest` in `web/public/`, linked from `web/index.html` with relative URLs. No deck change: the content types and the CSP exist. Design §3.4-§3.5.

## DL-464: Brand PNGs come from the e2e Chromium and are committed (B16) (design agent, 2026-10-23)

- `tools/brand/render.mjs` uses the Playwright Chromium of `web/node_modules` and the bundled Mona Sans; it writes the ICO itself. No new dependency, no Chromium in the build. Design §3.6.

## DL-465: The Omarchy bar widget draws the mark with `PathSvg`; the menu keeps glyphs (B16) (design agent, 2026-10-23)

- `Widget.qml` replaces the U+F0E8 glyph with a `QtQuick.Shapes` `Shape` filled with `root.tint` (Omarchy ships `QtQuick.Shapes`). The Omarchy menu draws an extension icon only as a glyph, so its rows stay. Existing installs update through "outdated → Install". Design §3.8.

## DL-466: No Fleet Deck launcher or notification icon in B16 (design agent, 2026-10-23)

- The installer writes only under the config home, and a launcher needs data-home files and a new repair asset id. The operations guide names `omarchy-webapp-install "Fleet Deck" <board URL> <board URL>/apple-touch-icon.png` (with the icon URL, so that no public favicon service sees the board address). Design §3.9, §6.

## DL-467: The README header is the lockup; the social preview is a committed PNG (B16) (design agent, 2026-10-23)

- `docs/v2/assets/brand/lockup-{light,dark}.png` in a `<picture>` inside the README H1; `docs/v2/assets/brand/social-preview.png` (1280×640), uploaded by the owner in the GitHub settings. Design §3.7.

## DL-468: The old icon `docs/v2/assets/fleet-deck-icon.png` is removed (B16) (design agent, 2026-10-23)

- Nothing referred to it, and its orange is not a color of the design system. Design §3.4.
