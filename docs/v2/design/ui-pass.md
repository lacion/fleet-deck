# Board design pass (2026-10-15)

- **Status:** Audit and proposed direction. Nothing is built yet. The owner chooses the direction (§4) before work starts.
- **Method:** the board on `v2` at `fd6fa324`, captured with Playwright at 1440×900 and 390×844, dark and light, on the owner's real fleet (3 stations, 15 sessions). Read against `UI-DESIGN.md` (DL-141, the "1a Console" principles).

## 1. What the board is for

- **Subject:** a control deck for a fleet of AI coding agents that run on the owner's own machines.
- **Audience:** one developer (the owner), at a desk and on a phone, sometimes with guests on a share.
- **Primary job:** in two seconds, answer "does anything need me, and what is each agent doing?", then act (approve, steer, interrupt, resume) in one step.

The board already has the right bones: amber means "yours to act", data is mono, controls are dense on a desk and roomy on touch. The problems are in the layer above: hierarchy, density, type treatment and layout of the fleet.

## 2. Findings

Severity: **A** = hurts the primary job; **B** = looks unfinished or generic; **C** = polish.

### 2.1 Everywhere

| # | Sev | Finding | Opportunity |
| --- | --- | --- | --- |
| G1 | A | The hint bar "Press ? for the keys and a short guide." takes the top row of every page, also on the phone (56 px of a 844 px screen). | Show it once, then move it to a `?` button in the sidebar footer. |
| G2 | B | Tracked-out capitals everywhere: `FLEET DECK`, `DEV1`, `LIGHTING`, `STATION`, `QUEUED 1`, `NEEDS YOU`, `ALL CLEAR`, `CONFIGURED PROVIDERS`, `WHERE / AGENT / TASK`. The owner's own station names ("Lighting", "dev1", "mac-m4") are shouted in capitals and lose their identity. This is the most common sign of a generated UI. | Sentence-case labels in Plex Sans 500. Names stay as the owner wrote them. Capitals only for one thing (see §3). |
| G3 | B | Mono is used for prose and chrome too: filter chips, "Quiet so far.", "The session ended.", station headers, counts in sentences. Mono then stops meaning "data". | Mono only for data: paths, ids, models, times, costs, commands. |
| G4 | B | Middle-dot strings in many places: `15 sessions · 0 need you · 3 stations`, `Claude 2.1.300 · signed in`, `Opus 5 · v2`, `ENDED · 1`. | Separate facts by layout (columns, icons, spacing), not by dots. |
| G5 | A | Contrast: `--faint` text is about 3.3:1 on dark and about 2.9:1 on light. It is used for real information ("Ended", "30m", "default", times). | Raise `--faint` to at least 4.5:1 for text; keep the old value only for decoration. |
| G6 | B | Badge salad: one session header shows `working` (green), `Opus 5` (pink), `bypassPermissions` (red), `Claude` (pink): four tinted pills with four meanings at one weight. | One status (dot + word), model and provider as quiet mono text, a risky mode as a small hazard icon with a tooltip. |
| G7 | C | The wordmark is a stock lightning icon plus spaced capitals; the version line says `deck 0.0.0`. | A small, own mark (§3). Remove the version line until builds have an identity (backlog B5). |

### 2.2 Fleet (home)

| # | Sev | Finding | Opportunity |
| --- | --- | --- | --- |
| F1 | A | The filter area is three wrapped rows of label + chips (Station, Provider, Repo, Group; Group is also in the sidebar), then a status row whose counts float in fixed columns, with "Resume all (0)" and "Clear (1)" hanging under "OFFLINE 1". 170 px before the first session. | One toolbar line: search, a "Filters" popover with the active filters as removable chips, group switch. Status counts become the strip headers (§3). |
| F2 | A | Cards are about 200 px wide and float in a grid of status columns inside station groups, so one card sits on the left, the next on the far right, and most of the screen is empty. The eye cannot scan. | The strip board (§3): one horizontal strip per session, aligned, scannable. |
| F3 | B | A card is a data dump: last command in mono, three file chips, mode badge, cost badge, time. | Strip shows: callsign, title, what it does now (one line), elapsed, needs-you edge. Files and cost on hover or in the session. |
| F4 | A | The "Needs you" rail is 320 px wide also when it is empty ("ALL CLEAR · Nothing waits for you."): 22 % of the screen for nothing. | Empty: a slim "All clear" line in the header. With items: the rail opens, amber edge. |
| F5 | B | The activity dock at the bottom (Activity / Mail & tasks / Assign / Usage + 8 filters) is open by default and says "Quiet so far." | Closed by default, one line with the newest event; opens on click. |

### 2.3 Sidebar

| # | Sev | Finding | Opportunity |
| --- | --- | --- | --- |
| S1 | B | Sessions, then 11 navigation items, then theme switch, version, "Add a device", "Log out": one long column; navigation sits below a list that grows. | Navigation at the top as 4 primary places (Fleet, Activity, Library = plans/automations/artifacts, Stations); the rest in a "More" menu; sessions below; account items in one footer menu. |
| S2 | C | "Archived (12)" sits in the middle of the list with the same weight as a session. | A quiet link at the end of the list. |

### 2.4 Session

| # | Sev | Finding | Opportunity |
| --- | --- | --- | --- |
| X1 | B | The Files panel is open by default and lists the whole repository, dotfiles first; it competes with the chat. | Closed by default; when open, "Changed files" first, the tree second. |
| X2 | B | The chat filter row (Reads, Edits, Commands, MCP, Other tools, Thinking, Notices, Compact tools) takes a full row; on the phone it overflows ("T…"). On and off chips look almost the same. | One "Show" menu with counts; the row disappears. |
| X3 | A | The composer has two equal secondary buttons ("Steer now", "Queue follow-up") and no primary. When the session ended, the phone still shows a large disabled composer. | One primary action that follows the state: Send (idle), Steer (working, with Queue as the second), Resume (ended or lost). |
| X4 | C | An unexplained "Y" avatar sits in the header. | Show presence only when another person is in the session. |
| X5 | B | Earlier history (new) renders as plain muted text with no shape. | Same message shapes as the live chat, under a quiet "Earlier, in the terminal" divider. |

### 2.5 Other pages

| # | Sev | Finding | Opportunity |
| --- | --- | --- | --- |
| P1 | B | Stations: each card shows the full station id (64 hex), the key fingerprint, "Dial-only (no stored address)", a `0.0.0` badge, a shell directory input with a placeholder path, and a red "Remove station" at full weight. | Card headline: name, link health, last seen, load (`1 / 12` as a small bar), providers with sign-in state. Ids, keys and shells under "Details". Remove in the overflow menu. |
| P2 | B | Settings › Tailnet: the "Now" and "With tailnet" rows are identical; booleans mix checkboxes and an amber toggle (amber on a setting breaks the amber rule). | Show the difference only when there is one; one switch style for every boolean, not amber. |
| P3 | B | New session: a long stacked form (Station, Directory, Workspace, Provider "claude · claude · 2.1.300", Permission mode, Model, Title, First prompt); the prompt, the main thing, is last. | Prompt first, as in the composer; where and agent as compact pickers in one line under it; advanced options folded. |
| P4 | C | Dialogs: the title ("Add station") is the same size as the body. | A clear dialog title size and spacing. |
| P5 | A | Phone home: the session list has no "needs you" summary; group headers are spaced capitals; most of the screen is empty. | Phone home opens with needs-you items, then working, then the rest, as strips. |

## 3. Proposed direction: "Flight strips"

The board already speaks the language of a flight deck: sessions have callsigns (`falcon-899q`), stations, a dispatch of work, and things that need the controller. Air traffic controllers track each flight on a **flight progress strip**: a narrow horizontal strip with the callsign big on the left, the state on the edge, and the few live facts in fixed columns. That is the board's one memorable element.

**Color** (dark first; light mirrors it):

| Name | Hex | Role |
| --- | --- | --- |
| Night | `#0d1117` | canvas |
| Strip | `#151b24` | strip and panel surface |
| Rule | `#273041` | strip borders, dividers |
| Ink | `#e6ebf2` | primary text |
| Signal amber | `#f0a63c` | yours to act: needs-you edge, primary action, focus (kept from DL-141) |
| Runway green | `#5fbf8f` | running / healthy |
| Hazard | `#ff6b54` | failure, conflict, risky mode |

The model hues stay for the model text only, not as filled pills.

**Type:**
- IBM Plex Sans (UI and prose), as today.
- **IBM Plex Sans Condensed 600 for callsigns and strip titles.** A condensed face is the voice of a strip. It is bundled offline like the others (`@fontsource/ibm-plex-sans-condensed`, a new dependency that needs the owner's approval).
- IBM Plex Mono only for data in strip columns.
- Scale: 12 / 13 / 15 / 18 / 24 px; labels in sentence case, 500 weight.

**Layout (fleet, desktop):**

```
┌ sidebar ┐┌ Fleet ── 2 need you ── [search……] [Filters ▾] [Group: station ▾] ┐
│ Fleet   ││ Needs you                                                       │
│ Activity││ ▌falcon-899q  Fix flaky test   asks: run cargo test?   2m  [Approve]│
│ Library ││ Lighting                                                        │
│ Stations││ ▏architect    Fleet Deck arch.  Bash: cat > /tmp/…    now  Opus 5 │
│ ─────── ││ ▏heron-12ab   Docs pass         Edit: README.md       4m  Sonnet │
│ sessions││ dev1                                                            │
│ …       ││ ○ falcon-6e7a  ended                                       30m │
└─────────┘└──────────────────────────────────────────────────────────────────┘
```

- Strips are full width, left aligned, in fixed columns: state edge · callsign · title · now · elapsed · model · one action.
- The left edge carries the state: amber bar = needs you, green hairline = working, hollow = ended.
- The needs-you group is always first and is the only place with an inline primary action.
- Phone: the same strip, two lines (callsign + now; title + elapsed), swipe-free, the action button at the right.

**Principles:**
1. The strip is the one bold element; everything around it is quiet.
2. Capitals nowhere; the condensed callsign does the shouting.
3. One primary action per screen, and it follows the state.
4. Facts by position, not by punctuation or pills.
5. Empty space only when there is nothing, never as a side effect of the grid.

**Review against the generic default:** a generic "agent dashboard" would be a grid of rounded cards with status pills, a dark canvas and one neon accent, or a cream page with a serif. The strip board comes from the subject (callsigns, dispatch, controllers), keeps the board's established amber language, and removes the card grid and the pill salad instead of restyling them.

## 4. Owner decisions

1. **Direction:** "Flight strips" (§3), or keep today's card layout and fix only the findings.
2. **The condensed font:** add `@fontsource/ibm-plex-sans-condensed` (new dependency), or use Plex Sans 600 with tighter tracking.
3. **Scope of the first wave:** the global items (G1–G7) + Fleet (F1–F5) + phone home (P5) first, then session (X1–X5), then the other pages.

## 5. Delivery plan (after the decisions)

- **Wave 0 (tokens and type):** tokens, contrast fix, label and mono rules, wordmark, hint bar. Touches `tokens.css`, `base.css` and the label call sites.
- **Wave 1 (fleet strips):** the strip component, the fleet page, the needs-you behavior, the toolbar, the phone home.
- **Wave 2 (session):** header, composer, Show menu, files panel, history shapes.
- **Wave 3 (pages):** stations, settings, new session, dialogs.

Each wave is one `ux-dev` worker with exact specs, e2e screenshots before and after at 1440 and 390 px in both themes, and one Codex review per wave. `UI-DESIGN.md` gets a §8 with the new rules, so every later brief copies them.
