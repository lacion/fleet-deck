# Small UI items: a restart marker, turns without a snapshot, the phone artifact version, older hover rules (B23)

- **Status:** Draft, revision 1 (2026-10-23), for the architect. Track T10 of the backlog program (`~/fleetdeck-briefs/backlog/PROGRAM.md`), decision-log range DL-500..DL-509.
- **Risk:** normal (board UI only: no Rust, no route, no schema, no stored data). One Codex review of the final branch. No adversary.
- **Owner request (BACKLOG.md B23, 2026-10-22):** four small items: (1) a clear marker on each setting that needs a restart; (2) a fold rule for turns without a snapshot; (3) a version selector of phone artifacts that is as clear as the board's; (4) the older `:hover` rules that do not follow `DESIGN-SYSTEM.md`.
- **Base:** v2 `497b2fbc`. The backlog item was written on `7333db5d` (2026-10-22 21:06). After it, the three rounds of the live UI audit (`50fb4211` … `1209121f`) changed all four areas. Section 1 gives the facts on `497b2fbc` and corrects the backlog.
- **Scratch:** `~/fleetdeck-briefs/backlog/small-ui/`: the exhaustive hover list (`hover-verdicts.tsv`, all 162 rules), its parsers (`hover-all.awk`, `tools/verdicts.py`), and the Playwright shots of the artifact viewer on the built board of `497b2fbc` (`pw/shots/`, spec `pw/versions.spec.ts`, config `pw/playwright.config.ts`).

## 1. Facts on `497b2fbc` (and backlog corrections)

### 1.1 The restart marker (item 1)

1. A save answer has `restart_needed: Array<string>`: the changed keys that apply at the next start (`web/src/gen/settings/SettingsSaved.ts`; computed old file against new file in `crates/fleetdeck-core/src/settings/writer.rs:346-351` and `diff.rs:34-37`).
2. The tab turns that list into one notice: `noticesOfSaved` (`web/src/settings/SettingsTab.tsx:121-141`): "1 setting applies after a restart." or "N settings apply after a restart.", tone `info`, with the action "Restart <name>…" (`SettingsTab.tsx:455-466`).
3. **Correction:** each restart field already has a marker. `badgesOf` (`web/src/settings/model.ts:197-208`, line 199) gives every field with `apply: "restart"` the neutral badge "Applies after a restart", on the control line (`SettingsFields.tsx:56-67`, `139-145`) and as hidden words in the hint (`SettingsFields.tsx:237-239`). This badge is a property of the field. It shows before and after a save, so it cannot tell which settings now wait for a restart. The owner wants a marker on the saved settings that wait.
4. **Defect found:** `applySaved` (`SettingsTab.tsx:274-284`) removes the `restart` notice and adds the notice of the last save only. Save A (a restart key) shows the notice; save B (a live key only) removes it, but the restart is still necessary.
5. The view has no "waits for a restart" fact: `SettingsView` is `values`, `fields`, `revision`, `overrides` (`web/src/gen/settings/SettingsView.ts`), read again from the file on each load (`crates/fleetdeck-core/src/settings/view.rs:56-91`). The running process values are not in it.
6. After a restart, the tab waits until the connection is live again, then loads the view (`SettingsTab.tsx:213-230`, the `step.done` branch at 217-222).
7. `.fd-badge[data-tone="info"]` exists (`web/src/styles/base.css:1194-1197`, `--focus-bg`/`--focus`); `.fd-badge` has `gap: 4px` for an icon (`base.css:1163-1177`). DS §4.7 (line 310) gives the field badges one place.

### 1.2 Turns without a snapshot (item 2)

1. **Correction:** `parseTurnSnapshot` and `turnSnapshot` (`web/src/session/TurnsPanel.tsx:345`, `review/placement.ts:9`) are the identity of a **review** snapshot (`turn:<from>..<to>`), not the file snapshot. Every turn that offers "Show the diff" has both commits: the station sets `diff_summary` only when both checkpoint sides are `available` (`crates/fleetdeck-station/src/store/turn_checkpoints.rs:457-472`), and the board offers the diff only with changes (`TurnsPanel.tsx:653`). So no turn with a diff lacks a review snapshot.
2. Audit round 3 (`dc7f44ce`) added two rules for turns without a snapshot: a folder outside Git shows one empty state (`TurnsPanel.tsx:584-597`, `checkpoint.ts:63-70`); a list where every turn lost its snapshot for one reason, with no change, shows one empty state or one shared note (`TurnsPanel.tsx:600-631`, `checkpoint.ts:39-56`).
3. Open: a **mixed** list. Each turn without a diff is a full `--raised` strip (`styles/session.css:1676-1686`), so a long session with many "No snapshot: background work was running" rows hides its few turns with diffs. No rule folds them.
4. **Defect found:** the row text checks only the end side (`TurnsPanel.tsx:640-650`). A turn whose **start** is `unavailable` (for example `provider_initiated`) and whose end is `available` has no `diff_summary` (fact 1), and the row says "No file changes". That is not known: the board has no start snapshot to compare. A turn with no checkpoint at all (an old event, `checkpoint: null`, `workspace.ts:294-302`) also says "No file changes".
5. The reason codes of the station (`crates/fleetdeck-station/src/workspace/boundary.rs:42-67`) include `checkpoint_missing` and `checkpoint_changed` (recovery). `CAUSE` (`web/src/session/checkpoint.ts:12-21`) has no words for them, so they read "No snapshot of the files for this turn".
6. `TurnsPanel.tsx` has 727 lines; the module guard is 800 lines (`crates/fleetdeck-cli/tests/module_size.rs:47-55`). `workspace.test.tsx` (758) and `settings.test.tsx` (798) are near the limit: new tests go in new files.

### 1.3 The phone artifact version (item 3)

1. **Correction:** the chip text is no longer "Artifact: <title> v<n>". It is "Artifact · <title>" with a `.fd-badge[data-tone="muted"]` "v<n>" from version 2 on (`web/src/artifacts/ArtifactChips.tsx:18`, `33-39`).
2. The viewer shows the version with `VersionControl` (`web/src/artifacts/ArtifactViewer.tsx:240-272`): one version is a muted "v1" badge; more versions are a `select.fd-ghost.fd-sm` with "v3 (latest)", "v2", "v1". The page (`#/artifact/...`) and the session panel (the Artifacts tab, where a chat chip opens, `session/SessionView.tsx:380-383`) use it.
3. Audit rounds 2 and 3 (`1397a8d2`, `0e5b3be2`) made the phone **page** clear: the one version in the summary (`ArtifactViewer.tsx:413-428`, `artifacts.css:499-518`), the select label on the gutter (`page.css:173-189`), and the ghost buttons of the header at the field size of the select (`base.css:455-466`).
4. The **panel** has neither of the two last rules. The shots of the built board (`pw/shots/phone-2-panel-v2.png`, 390 px, touch) show: the select label "v2" at 16 px next to "Open in a new tab" at 12 px (two type sizes in one row), and the label 9 px in from the gutter of the title and "Back to the list". The desktop panel (`desktop-2-panel-v2.png`) and the phone page (`phone-4-page-v2.png`) do not have these faults. The open list is the board menu on both (`phone-3-panel-select-open.png`, `desktop-3-panel-select-open.png`).
5. On the chip, the muted badge has no fill and no padding (`base.css:1203-1208`), so "Shim test run report v2" reads as one title (`phone-1-chat-chips.png`). The artifact list shows the same fact as a neutral badge "apart from the words of the title" (`ArtifactList.tsx:126-131`, `artifacts.css:102-107`): two looks for one fact.

### 1.4 The older hover rules (item 4)

1. **Correction:** the area files have 162 rules with `:hover` (180 lines that contain `:hover`), not "about 148". Audit round 2 (`0e6c3600`) added the DS rule "every `:hover` rule sits in `@media (hover: hover)`" (DESIGN-SYSTEM §3, line 207) and the check 5e of `tools/design-check.sh` (lines 174-221) with a budget of older rules per file (line 209: `displays.css=1 home.css=14 integrations.css=9 page.css=2 settings.css=9 terminal.css=5`).
2. On `497b2fbc`: 122 rules are in a hover block and 40 are not (exactly the budget). The check passes.
3. Of the 122 guarded rules, 9 break another rule of the DS (§7.3 below): a row with a selected state that hovers the fill of its selected look (DS §2.1 line 54), a `--raised` hover on a `--raised` layer or a white light panel (DS §3 line 173, 195), a fill under 1.1:1 in light, a dead or a duplicate rule, and a primary with no hover.
4. Three rules use three fills for one job (the hover of a control whose rest fill is `--raised-hi` on a `--raised` layer): `session.css:1093` and `1409` (`--border`, a divider token) and `session.css:1948` (`color-mix(... --raised-hi 88%, --text)`). The DS has no token for it.

## 2. Questions and answers

B23 has no "Questions for the design" list. Each "Wanted" point leaves one question open.

1. **Which marker, and on which settings?** On each field whose saved value waits for a restart: the badge "Restart needed" in the `info` tone with a 12px `rotate-ccw` icon, in place of the neutral "Applies after a restart" of that field, until the restart ends (§3). Evidence: fact 1.1.3 (the static badge exists and cannot show the state), fact 1.1.4 (the notice can vanish too early).
2. **Which fold rule for turns without a snapshot?** Fold three or more turns in a row that offer nothing to open or to revert, into one closed row "Turns 4–9 · <why>"; never fold the last turn; also say "No snapshot" (not "No file changes") for a turn without a start snapshot (§4). Evidence: facts 1.2.3 and 1.2.4.
3. **What makes the phone version selector as clear as the board's?** The panel that a chip opens gets the two phone rules that the page has (one type size in the action row, the label on the gutter), and the chip shows its version as the neutral badge of the list (§5). Evidence: the shots of fact 1.3.4 and 1.3.5.
4. **Which hover rules break the DS, and what is the target?** 40 rules outside a hover block (move or split, one duplicate to delete) and 9 guarded rules with a wrong fill or no effect; the budget of check 5e goes to 0 (§6, the full list in `hover-verdicts.tsv`).

## 3. Design: the restart marker (DL-500, DL-501)

### 3.1 Data (board only)

New module `web/src/settings/restartPending.ts`:

```ts
import type { FieldMeta, SettingsSaved } from '../gen';
import type { Values } from './model';

/**
 * The saved keys that wait for a restart of this tab's process, each with the JSON of its value
 * before the first save that changed it (the value that the process runs with, as the board
 * knows it). Board state only: the settings view has no such fact (DL-501).
 */
export type Pending = Readonly<Record<string, string>>;

export const NO_PENDING: Pending = {};

const json = (v: unknown): string => JSON.stringify(v ?? null);

/**
 * The pending keys after a save. `before`: the values of the view before the save. A key of
 * `saved.restart_needed` that is not pending yet gets its value before the save. A pending key
 * whose saved value is again its first value leaves the list (the owner changed it back), except
 * a secret field: its view value (`{ "set": true }`) does not show a change of the secret.
 */
export function nextPending(
  pending: Pending,
  before: Values,
  saved: SettingsSaved,
  fields: readonly FieldMeta[],
): Pending {
  const out: Record<string, string> = { ...pending };
  for (const key of saved.restart_needed) {
    if (!(key in out)) out[key] = json(before[key]);
  }
  for (const key of Object.keys(out)) {
    const secret = fields.find((f) => f.key === key)?.secret === true;
    if (!secret && json(saved.values[key]) === out[key]) delete out[key];
  }
  return out;
}
```

(`saved.values` holds every value of the file after the save, so the check also covers a pending key that this save did not touch.)

### 3.2 Behavior

1. `SettingsTab` keeps `const [pending, setPending] = useState<Pending>(initial?.pending ?? NO_PENDING)`. `InitialTab` gets `readonly pending?: Pending` (tests).
2. `applySaved(saved, patch)` computes `const next = nextPending(pending, view.values, saved, view.fields)` from the view **before** `setView`, then `setPending(next)`, and builds the notices with `noticesOfSaved(saved, Object.keys(next).length)`.
3. `noticesOfSaved(saved, pendingCount = saved.restart_needed.length)`: the restart notice uses `pendingCount` (the text stays: "1 setting applies after a restart." / "N settings apply after a restart."). The old test (`settings.test.tsx:324-338`) keeps passing with the default. This fixes fact 1.1.4: a save of live keys keeps the notice while keys wait.
4. The wait after a restart (`SettingsTab.tsx:217-222`, `step.done`) calls `setPending(NO_PENDING)` before `load()`.
5. A `load()` for any other reason (Reload after "These settings changed since you opened them.", the first load) keeps `pending`.
6. `SettingsTab` passes `pending={Object.keys(pending)}` to `SettingsFields`. `FieldsProps` gets `readonly pending?: readonly string[] | undefined` (default `[]`).
7. `badgesOf(meta, overrides, pending = false)`: when `pending` is true and `meta.apply === 'restart'`, the first badge is `{ text: 'Restart needed', tone: 'info' }`; else `{ text: 'Applies after a restart' }` (no `tone` key, so the markup of today stays: `fieldCopy.test.tsx:34`, `fieldLayout.test.tsx:43-56` stay green). A pending key of a `live` field (impossible today) gets no badge. `Badge` becomes `{ readonly text: string; readonly tone?: 'info' }`.
8. `Badges` (`SettingsFields.tsx:56-67`) renders `<span className="fd-badge" data-tone={b.tone}>`, and for `tone === 'info'` a leading `<Icon of={RotateCcw} size={12} />` before the text. The hint words stay `` ` ${b.text}.` `` (so a screen reader hears "Restart needed." in the hint of the control).
9. A list field (`group`, under the label) gets the same badge in its place (`SettingsFields.tsx:110-114`).

### 3.3 UI text, look, a11y

- Badge: "Restart needed" (sentence case), `.fd-badge[data-tone="info"]`, a 12px `rotate-ccw` icon (the icon of "Continue after a station restart", `session/SessionSettings.tsx:257`). No new token, no CSS change.
- Not color alone: the words change ("Applies after a restart" → "Restart needed") and an icon is added. Forced colors: badges get a 1px `CanvasText` edge already (DS §6).
- The badge stays `aria-hidden` (the control keeps its name); the hint carries "Restart needed." The `role="status"` notice announces the count after a save.
- No change on a phone layout: the badge has its place of today.

### 3.4 Edges and errors

- A failed save, an elevation refusal, "changed" or "invalid": `applySaved` does not run; `pending` does not change.
- A failed restart (`would_stop`, `unsupported`, `failed`, a refused elevation): `pending` stays; the notice stays.
- The owner leaves the tab or reloads the page: the tab state goes, the marker and the notice go (the notice of today has the same lifetime). Accepted (DL-501, owner question 1).
- A restart from the command line or a crash: the board does not know it; the marker stays until the tab state goes. Accepted.
- A secret relay (`submitSecret` → `applySaved`, `SettingsTab.tsx:356-364`) adds `provider_instance` like a save.
- Older stations and decks: no change of any route or answer; the board reads `restart_needed` of today.

### 3.5 Out of scope

- A `pending_restart` list in `SettingsView` that the station computes from its boot values (rejected in DL-501: a schema change of `fleetdeck-proto`, the deck and every station).
- The Tailnet card (`tailnet/TailnetCard.tsx:71-76`, `342-343`) keeps its own badge and saved line.
- A mark in the section index.

## 4. Design: turns without a snapshot (DL-502, DL-503)

### 4.1 Data (board only)

New module `web/src/session/turnFolds.ts`:

```ts
import type { Capabilities } from '../gen';
import { hasChanges } from './changes';
import type { FinalizedTurn } from './workspace';

/** What the row of a turn says about its files. */
export type TurnFiles =
  | { readonly kind: 'changes' }
  | { readonly kind: 'no_changes' }
  | { readonly kind: 'no_snapshot'; readonly reason: string | null };

export function turnFiles(t: FinalizedTurn): TurnFiles {
  if (t.diff !== null && hasChanges(t.diff)) return { kind: 'changes' };
  if (t.end?.state === 'unavailable') return { kind: 'no_snapshot', reason: t.end.reason };
  if (t.start?.state === 'unavailable') return { kind: 'no_snapshot', reason: t.start.reason };
  if (t.start === null || t.end === null) return { kind: 'no_snapshot', reason: null };
  return { kind: 'no_changes' };
}

/**
 * A quiet turn offers nothing to open or to revert: no diff, not reverted, and no revert (no
 * rollback, or no start snapshot: its "Revert to here" is disabled).
 */
export function quietTurn(t: FinalizedTurn, caps: Capabilities | null | undefined): boolean {
  return (
    turnFiles(t).kind !== 'changes' &&
    !t.reverted &&
    (caps?.rollback !== true || t.start?.state !== 'available')
  );
}

/** The fewest quiet turns in a row that fold. */
export const FOLD_MIN = 3;

export type TurnEntry =
  | { readonly kind: 'turn'; readonly index: number; readonly turn: FinalizedTurn }
  | {
      readonly kind: 'fold';
      /** 0-based list positions of the first and the last folded turn. */
      readonly first: number;
      readonly last: number;
      readonly turns: readonly { readonly index: number; readonly turn: FinalizedTurn }[];
    };

/**
 * The rows of the turn list: a run of FOLD_MIN or more quiet turns is one fold. The last turn of
 * the list never folds (it is the newest work).
 */
export function foldTurns(
  turns: readonly FinalizedTurn[],
  caps: Capabilities | null | undefined,
): TurnEntry[] {
  const out: TurnEntry[] = [];
  let run: { index: number; turn: FinalizedTurn }[] = [];
  const flush = () => {
    if (run.length >= FOLD_MIN) {
      out.push({ kind: 'fold', first: run[0]!.index, last: run[run.length - 1]!.index, turns: run });
    } else {
      for (const r of run) out.push({ kind: 'turn', ...r });
    }
    run = [];
  };
  turns.forEach((turn, index) => {
    if (index < turns.length - 1 && quietTurn(turn, caps)) run.push({ index, turn });
    else {
      flush();
      out.push({ kind: 'turn', index, turn });
    }
  });
  flush();
  return out;
}

/**
 * The words after "Turns 4–9": one cause when every folded turn has it, else the kind.
 * `sharedBy`: the list says this reason once above (the row then says "No snapshot").
 */
export function foldText(
  turns: readonly FinalizedTurn[],
  sharedBy: (reason: string) => boolean,
  text: (reason: string) => string,
): string {
  const files = turns.map(turnFiles);
  if (files.every((f) => f.kind === 'no_changes')) return 'No file changes';
  if (files.every((f) => f.kind === 'no_snapshot')) {
    const reasons = new Set(files.map((f) => (f.kind === 'no_snapshot' ? f.reason : null)));
    const [only] = [...reasons];
    if (reasons.size === 1 && only !== null && only !== undefined) {
      return sharedBy(only) ? 'No snapshot' : text(only);
    }
    return 'No snapshot';
  }
  return 'No changes to show';
}
```

(`Capabilities` is the type that `workspace.ts:12` imports from `../gen` for `revertBlock`. `text` is `checkpointText` of `checkpoint.ts`. `provider_instance` is the one secret restart field today: `crates/fleetdeck-core/src/settings/fields.rs:200-207`.)

### 4.2 Behavior

1. **Row text** (`TurnsPanel.tsx:640-650`): from `turnFiles(t)`: `changes` → `TurnStat` as today; `no_snapshot` with a reason → `sharedBy(reason) ? 'No snapshot' : checkpointText(reason)` with `title={reason}`; `no_snapshot` without a reason → `checkpointText('')` ("No snapshot of the files for this turn"), no title; `no_changes` → "No file changes". This corrects fact 1.2.4.
2. **Causes** (`checkpoint.ts:12-21`): add `checkpoint_missing: 'the snapshot is gone'` and `checkpoint_changed: 'the snapshot was changed'`.
3. **Folds**: the `<ol>` (`TurnsPanel.tsx:632-725`) maps `foldTurns(turns, caps)`. A `turn` entry renders the row of today (moved into a local `row(t, i)` function, unchanged markup). A `fold` entry renders:

```tsx
<li key={`fold:${entry.turns[0].turn.turnId}`} className="fd-turn-fold" data-first={entry.first + 1} data-last={entry.last + 1}>
  <details>
    <summary>
      <span className="fd-turn-name">{`Turns ${entry.first + 1}–${entry.last + 1}`}</span>
      <span className="fd-turn-stat">{foldText(entry.turns.map((r) => r.turn), sharedBy, checkpointText)}</span>
    </summary>
    <ol>{entry.turns.map((r) => row(r.turn, r.index))}</ol>
  </details>
</li>
```

   The dash is U+2013. The fold is closed at the first render; its open state is the DOM state of `<details>` (the key is the first folded turn, so a new turn that extends the run keeps it).
4. The empty states and the shared note of today (`TurnsPanel.tsx:584-631`) stay and come first: a fold shows only in a list that renders rows.
5. A file request of the Properties panel and a comment chip target a turn with a diff (fact 1.2.1): never a folded turn.
6. `sharedReason` (`checkpoint.ts:39-56`) is not changed (it reads the end side only; DL-502).

### 4.3 Look (`styles/session.css`, after the `.fd-turns li` rules at 1676-1686)

```css
/* A fold of quiet turns (B23): no strip of its own; the summary is a 36px row with the details
   chevron, the range in --dim and the cause in --faint; the folded rows are strips under it. */
.fd-turns li.fd-turn-fold {
  display: block;
  min-height: 0;
  padding: 0;
  background: transparent;
  box-shadow: none;
}
.fd-turn-fold > details > summary {
  display: flex;
  align-items: center;
  gap: var(--sp-2) var(--sp-3);
  min-height: var(--row-h);
  padding: 0 var(--sp-2) 0 var(--sp-4);
  border-radius: var(--r-card);
  color: var(--dim);
}
.fd-turn-fold > details > summary > .fd-turn-name {
  font-size: var(--fs-ui);
  font-weight: 500;
}
.fd-turn-fold > details > ol {
  margin-top: 6px;
}
@media (hover: hover) {
  .fd-turn-fold > details > summary:hover {
    color: var(--text);
  }
}
@media (pointer: coarse) {
  .fd-turn-fold > details > summary {
    min-height: var(--tap-min);
  }
}
```

The chevron is the `<details>` marker of `base.css` (DS §3 line 205). Focus: the summary keeps the focus ring of `base.css`. Forced colors: no fill to keep. The folded rows keep the strip look of `.fd-turns li` (the selector matches the nested `li`).

### 4.4 Out of scope

- A fold in the "Whole session" view, the wall or Compare. A change of `sharedReason`. A server change of the checkpoints.

## 5. Design: the phone artifact version (DL-504, DL-505)

### 5.1 The panel bar on a touch screen (`styles/artifacts.css`)

1. One type size in the action row (the rule of `base.css:455-466`, for the panel bar):

```css
@media (pointer: coarse) {
  .fd-art-bar:has(> .fd-art-version > select.fd-ghost) > .fd-art-action {
    font-size: var(--fs-field);
  }
}
```

   (`.fd-art-action` is Open in a new tab, Download and Share…: `ArtifactViewer.tsx:342-367`. Back to the list stays 12 px: it is on a row of its own.)

2. The select label on the gutter in a narrow bar, where the version starts the action row (`artifacts.css:423-438`), as `page.css:180-182` does in a page:

```css
@container fd-art (max-width: 640px) {
  .fd-art-bar > .fd-art-version:has(> select.fd-ghost.fd-sm) {
    margin-inline-start: -8px;
  }
}
```

No TSX change. The select, its options and its list stay (`ArtifactViewer.tsx:240-272`).

### 5.2 The chip version badge (`ArtifactChips.tsx:33-39`, `artifacts.css:655-679`)

1. The badge becomes the neutral version badge of the list: `<span className="fd-badge fd-art-ver">{`v${chip.version}`}</span>` (no `data-tone`). Version 1 has no badge (as today).
2. On the `--raised` chip the dark `--badge-bg` is `--raised-hi` (1.1:1). The chip gives it the edge of a key chip on a raised fill (DS §3 `.fd-kbd`, line 193):

```css
.fd-art-chip > .fd-badge {
  box-shadow: inset 0 0 0 1px var(--btn-edge-raised);
}
```

3. The chip text and the accessible name stay ("Artifact · Report v2").

### 5.3 Out of scope

- The publish age in the options ("v2 · 1 h ago") and a "Show the latest" action (owner question 3).
- A version select in the chip.

## 6. Design: the hover rules (DL-506)

### 6.1 The rule of each kind

The full list, one row per rule with its declarations, its verdict and its target, is `~/fleetdeck-briefs/backlog/small-ui/hover-verdicts.tsv` (162 rows: 111 `ok`, 51 with a change). The kinds:

| Code | Count | Target |
| --- | --- | --- |
| `H1` | 28 | Move the rule into `@media (hover: hover) { … }` at the same place in its file. No other change. |
| `H1-split` | 10 | The selector list mixes a state and its `:hover` (`.fd-x, .fd-x:hover`). Keep the non-hover selectors outside; repeat the same declarations for the `:hover` selectors in a hover block right after it (same specificity, same order). In a `forced-colors` block: `@media (forced-colors: active) and (hover: hover)`. |
| `H1+D1` | 1 | `integrations.css:471`: move, and fill `--hover-bg` (the list has an open row, a selected state). |
| `H2-delete` | 1 | `home.css:2086` `.fd-menu-switch:hover`: a copy of `shell.css:811` (guarded). Delete. (`home.css:2093`, the disabled reset, moves: `H1`.) |
| `D1` | 3 | A row with a selected state that hovers `--raised` (in light the fill of `--selected-bg`): `views.css:501` (`.fd-achan`), `views.css:571` (`.fd-achan-ended`, the Ended row as the sidebar Ended row, `shell.css:664`), `shell.css:948` (list variant `.fd-srow`): fill `--hover-bg`. |
| `D2` | 2 | `chat.css:229` (reasoning and tool group summary): `--surface` is 1.04:1 on the light `--bg`, fill `--raised` (the tool summary hover, `chat.css:283`). `page.css:834` (folder rows): add `@media (hover: hover) { :where(.fd-panel, .fd-dialog, [role="dialog"]) .fd-folder-list button:hover:not(:disabled) { background: var(--badge-bg); } }` (the Automations and Projects forms are `.fd-panel`, white in light: `automations/ScheduleForm.tsx:100`, `integrations/ProjectForm.tsx:218`; Add a home is a dialog: `integrations/ProjectHomes.tsx:155`). The spawn rule `spawn.css:127` stays. |
| `D3-delete` | 2 | `shell.css:938` (dead: `shell.css:948` has the same selector later), `tailnet.css:465` (the same declaration as `base.css:320`). Delete. |
| `D4` | 1 | `session.css:352` `.fd-sh-confirm > .fd-primary:hover`: the primary has no hover. Fill `color-mix(in srgb, var(--ink-fill) 88%, var(--bg))` (`base.css:341`); remove `justify-content` from the hover rule. Done in T2 (it owns `session.css`). |
| `G1-keep` | 3 | `session.css:1093`, `1409`, `1948`: no change (fact 1.4.4). A DS decision, not a hover fix. |

### 6.2 The 40 rules outside a hover block (by file)

- `displays.css:389`.
- `home.css:167`, `170` (in `@container fleet (max-width: 1100px)`: a hover block inside the container block), `361`, `457`, `694`, `723`, `1003`, `1041`, `1498`, `1939`, `2025`, `2086` (delete), `2093`, `2128`.
- `integrations.css:471` (+ `--hover-bg`), `497`, `504` (split), `691`, `699`, `713`, `834`, `924`, `971`.
- `page.css:518`, `573`.
- `settings.css:32` (split), `80`, `83` (split), `88` (split), `104`, `111`, `115` (split, forced colors), `794`, `1044`.
- `terminal.css:145` (split), `193` (split), `239`, `261`, `287` (split).

### 6.3 The check

`tools/design-check.sh:209` becomes `hover_budget=''`. Every file then has the budget 0: a new rule outside a hover block fails. DS §3 line 207 stays true.

### 6.4 Out of scope

- The `G1-keep` rules. The bare `button` hover of `base.css:320` in a dialog (light `--raised-hi` on `--raised` is 1.07:1, a primitive question).

## 7. Risk class

**Normal.** Board UI only. No authentication, no deletion, no migration, no stored cost, no personal data, no route or schema, no new command path, no process proof. The restart marker changes no call. The fold hides only rows that offer no action. The hover changes are CSS moves (a mobile browser stops showing a sticky hover; a pointer sees the same look).

## 8. Build plan

Four tasks can run in parallel (no shared file); T5 follows. Every task: base `v2` at `497b2fbc` or later; heavy commands only through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <cmd>`; UI tasks load the `fleet-design-system` skill.

### T1. The restart marker (ux, about 160 lines)

- **Owned files:** `web/src/settings/restartPending.ts` (new), `web/src/settings/restartPending.test.tsx` (new), `web/src/settings/model.ts`, `web/src/settings/SettingsFields.tsx`, `web/src/settings/SettingsTab.tsx`.
- **Changes:** §3.1 and §3.2 exactly (`nextPending` with one `if` for the drop rule; `Badge.tone`; `badgesOf(meta, overrides, pending = false)`; `Badges` with `data-tone` and the `RotateCcw` icon; `FieldsProps.pending`; `InitialTab.pending`; `noticesOfSaved(saved, pendingCount)`; `applySaved` order; clear after the restart wait).
- **Tests** (`restartPending.test.tsx`):
  - `nextPending adds each restart key of a save with its value before the save`: `{}` + save `restart_needed: ['listen']` → `{ listen: JSON of the old value }`.
  - `a later save of live keys keeps the pending keys`: pending `{listen}` + save `restart_needed: []`, `applied: ['host']` → still `{listen}`.
  - `a key changed back to its first value leaves the list`: pending `{listen: '"a"'}` + save with `values.listen = 'a'` → `{}`.
  - `a secret field never leaves the list on an equal value`: `provider_instance` with `secret: true`, same view value → stays.
  - `badgesOf gives "Restart needed" with the info tone only to a pending restart field`; `a live field gets no badge when pending`.
  - `noticesOfSaved counts the pending keys, not the keys of the last save`: `noticesOfSaved(saved({restart_needed: []}), 2)` → text "2 settings apply after a restart.".
  - `the tab marks a pending field and keeps the other restart fields neutral`: `renderToStaticMarkup(<SettingsTab … initial={{ view: view(), pending: { listen: '[]' } }} />)` contains `<span class="fd-badge" data-tone="info">` with "Restart needed" on the `listen` row and the neutral "Applies after a restart" on another restart field, and the hint text " Restart needed.".
- **Depends on:** none.
- **Acceptance:** `cd web && sh <v2>/tools/heavy.sh bun run test settings`; `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; `sh <v2>/tools/heavy.sh cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`.

### T2. Turns without a snapshot (ux, about 260 lines)

- **Owned files:** `web/src/session/turnFolds.ts` (new), `web/src/session/turnFolds.test.tsx` (new), `web/src/session/TurnsPanel.tsx`, `web/src/session/checkpoint.ts`, `web/src/styles/session.css`.
- **Changes:** §4.1 to §4.3 exactly; and `session.css:352` (`D4` of §6.1). `TurnsPanel.tsx` stays under 800 lines (about 750).
- **Tests** (`turnFolds.test.tsx`; copy the local helpers `ok`, `gone`, `finalized`, `codex` (`workspace.test.tsx:43-80`) and `view` (`workspace.test.tsx:86`), which that file does not export):
  - `turnFiles: a missing start snapshot is "no snapshot", not "no changes"`; `a turn without a checkpoint is "no snapshot" without a reason`.
  - `three quiet turns in a row fold; two do not`.
  - `the last turn never folds`: 4 quiet turns → one fold of turns 1–3 and the row of turn 4.
  - `a turn with a diff, a reverted turn and a turn with an enabled revert break a run`.
  - `foldText: one cause, the shared "No snapshot", "No file changes", "No changes to show"`.
  - `the list renders "Turns 2–5" in a closed details with the rows inside` (static render of `TurnsPanel` with 6 turns: 1 with a diff, 4 `background_writers`, 1 last quiet): contains `<li class="fd-turn-fold" data-first="2" data-last="5"><details><summary><span class="fd-turn-name">Turns 2–5</span>` and `No snapshot: background work was running`, and `Turn 6` outside the fold.
  - `a turn without a start snapshot says why in its row` (row text `No snapshot: the agent started the turn on its own`, `title="provider_initiated"`).
- **Depends on:** none.
- **Acceptance:** `cd web && sh <v2>/tools/heavy.sh bun run test session`; `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; the guards (as T1); the gallery shot `FD_GALLERY=1 … playwright test e2e/gallery.spec.ts -g "04"` (the Changes shot, both themes) through `heavy.sh` after `bun run build`.

### T3. The phone artifact version (ux, about 120 lines)

- **Owned files:** `web/src/artifacts/ArtifactChips.tsx`, `web/src/artifacts/chips.test.tsx`, `web/src/styles/artifacts.css`, `web/e2e/artifact-versions.spec.ts` (new).
- **Changes:** §5.1 and §5.2 exactly. `chips.test.tsx:57` asserts `<span class="fd-badge fd-art-ver">v2</span>` (the old muted markup changes on purpose, DL-505).
- **Tests:**
  - `chips.test.tsx`: `the published chip shows its version as the neutral badge of the list` (the new markup); version 1 has no badge.
  - `e2e/artifact-versions.spec.ts` (phone project; a `FakeDeck` with a session whose chat has `artifact.published` v1, v2, v3 of an `html` artifact with three versions; copy the scene of `~/fleetdeck-briefs/backlog/small-ui/pw/versions.spec.ts`): after a tap on the v2 chip, the panel select has the value `2`; the computed `font-size` of `.fd-art-bar > .fd-art-action` equals the one of the select (16px); the left edge of the select label (`select` box x + 8) is within 1px of the left edge of `.fd-art-name`; the page `#/artifact/st-alpha/art-1/2` keeps its summary and select.
- **Depends on:** none.
- **Acceptance:** `cd web && sh <v2>/tools/heavy.sh bun run test artifacts`; `cd web && bun run typecheck && bun run lint`; `sh tools/design-check.sh`; `cd web && sh <v2>/tools/heavy.sh bun run build`, then `FD_E2E_PORT=<free port> sh <v2>/tools/heavy.sh ./node_modules/.bin/playwright test e2e/artifact-versions.spec.ts e2e/chat-fit.spec.ts --project phone`; the guards; the gallery shots `-g "06|27"` (both themes, phone and desktop).

### T4. The hover sweep (ux, about 300 lines, CSS only)

- **Owned files:** `web/src/styles/chat.css`, `displays.css`, `home.css`, `integrations.css`, `page.css`, `settings.css`, `shell.css`, `tailnet.css`, `terminal.css`, `views.css`, `tools/design-check.sh`.
- **Changes:** every row of `hover-verdicts.tsv` with a verdict other than `ok`, `D4` and `G1-keep`, as §6.1 says; then `hover_budget=''` (§6.3). Copy the verdict file into the brief. Do not change a declaration that the verdict does not name.
- **Tests:** no unit test. `sh tools/design-check.sh` with the budget 0 is the test (it fails on any rule outside a block). The desktop e2e spec (`e2e/desktop.spec.ts`, it reads the settings tabs and the terminal dock) stays green.
- **Depends on:** none.
- **Acceptance:** `sh tools/design-check.sh`; `cd web && bun run lint`; `cd web && sh <v2>/tools/heavy.sh bun run build`; `FD_E2E_PORT=<free port> sh <v2>/tools/heavy.sh ./node_modules/.bin/playwright test e2e/desktop.spec.ts --project desktop`; the gallery shots of the touched pages, both themes, desktop and phone (`-g "01 |02 |03 |07 |10 |11 |12 |13 |16 |18 |21 |26 "`: home, fleet, chat, terminal dock, Add station, the two settings tabs, Activity, Automations, Projects, Terminals, project), compared with the shots of the base; the guards.

### T5. The design system text (ux, about 30 lines, docs only)

- **Owned files:** `docs/v2/DESIGN-SYSTEM.md`.
- **Changes:**
  1. §3 `select.fd-ghost` (line 174): in "the line that holds one takes the field size (…)" add "the actions of the artifact panel bar (`.fd-art-bar`)".
  2. §4.5, after the Tabs bullet: "Changes, By turn: three or more turns in a row that offer no diff and no revert fold into one closed row "Turns 4–9" and their cause (`--dim`, 36px, the details chevron); the last turn never folds. A turn without a start or an end snapshot says "No snapshot: <cause>", never "No file changes"."
  3. §4.7 Fields (line 310), after the badges sentence: "After a save, a field whose value waits for a restart shows "Restart needed" (`.fd-badge[data-tone=info]` with the 12px `rotate-ccw` icon) in place of "Applies after a restart", until the restart of the tab ends; the notice counts these fields."
  4. §2.1 `--hover-bg` (line 54): add "the Activity channels, the project lists".
  5. §7: one line for each of DL-500, DL-503, DL-505, DL-506 (date, name, why, where).
- **Depends on:** T1, T2, T3, T4.
- **Acceptance:** `sh tools/design-check.sh`.

## 9. Decisions made in this draft

1. The marker shows the **state** (saved, waits for a restart), not the field property: "Restart needed" replaces "Applies after a restart" on that field. Rejected: a second badge next to the static one (two badges say one fact), and a colored left edge on the row (a new pattern for one place).
2. The `info` tone (blue) for the marker. Rejected: `--act` yellow (only "needs you" and the decision primary, DS §2.2); `--hazard` (nothing is wrong).
3. The `rotate-ccw` icon. Rejected: `power` (Stop in the session header), `refresh-cw` (Refresh and Try again).
4. The pending set is board state with the lifetime of the notice. Rejected: a `pending_restart` field in `SettingsView` computed by the station from its boot values (a proto schema change for deck and stations: high risk class, out of the small-item scope).
5. A pending key changed back to its first value leaves the list, except a secret field. Rejected: never remove (a stale marker after an undo), and remove secrets too (a replaced secret has the same view value).
6. The restart notice counts the pending set (fixes fact 1.1.4). Rejected: keep "last save only".
7. The fold unit is a "quiet" turn: no diff, not reverted, no enabled revert. Rejected: fold every turn without a snapshot (it would hide a usable "Revert to here").
8. The fold threshold is 3 turns in a row. Rejected: 2 (one row saved for one summary row of chrome, DS §4.6 "a toggle row that reveals one row is chrome").
9. The last turn never folds. Rejected: fold it too (the newest turn is the one the owner checks).
10. A reverted turn is never folded. Rejected: fold it (the revert history goes out of sight).
11. The fold is a closed `<details>` with the rows inside. Rejected: a summary row with no way to the rows (the `title` with the reason code of each row is lost).
12. A turn without a start snapshot says "No snapshot: <cause of the start>" (and a turn with no checkpoint "No snapshot of the files for this turn"). Rejected: keep "No file changes" (a claim the board cannot make).
13. `sharedReason` is not changed. Rejected: read the start side there too (it changes the empty-state rule that round 3 just set; the folds now cover the case).
14. Words for `checkpoint_missing` and `checkpoint_changed`. Rejected: leave the generic text.
15. Phone artifact parity is the panel bar (field-size actions, label on the gutter). Rejected: a new control (a segmented control or a sheet of versions: a second control for a choice that a `<select>` holds, DS §3).
16. The chip version badge is the neutral badge of the list with the `--btn-edge-raised` ring. Rejected: keep the muted badge (it reads as part of the title), a badge with no ring (1.1:1 on the dark chip).
17. Hover rules that mix a state and its hover are split, with the declarations repeated. Rejected: rewrite them to one rule (it changes the cascade of the settings tabs and the terminal tabs).
18. The three `--raised-hi` hover fills stay. Rejected: a new token in this track (a DS expansion with a look change in three areas; the architect decides it apart).
19. `session.css:352` (the confirm primary hover) is fixed in T2, which owns `session.css`. Rejected: a T4 edit of `session.css` (two workers on one file).
20. The DS text is one task after the others. Rejected: each task edits `DESIGN-SYSTEM.md` (four workers on one file).

## 10. Owner questions (taste or policy; the build goes on with the default)

1. Keep the restart marker after a page reload or a tab switch? **Default: no** (the same lifetime as the notice of today). A later item can add the fact to the settings view.
2. The fold starts at how many quiet turns in a row? **Default: 3.**
3. Show the age of each version in the version list ("v2 · 1 h ago") and a "Show the latest" action on an older version, on the board and the phone? **Default: no.**
4. The words of the marker: "Restart needed" or "Waits for a restart"? **Default: "Restart needed".**

## Decision log entries (to append)

### DL-500: A marker on each setting that waits for a restart (B23, 2026-10-23)

- After a save, a field of `restart_needed` shows the badge "Restart needed" (`.fd-badge[data-tone=info]`, 12px `rotate-ccw`) in place of its neutral "Applies after a restart", until the restart wait of the tab ends. The notice "N settings apply after a restart." counts these fields; a save of live keys no longer removes it. Design `docs/v2/design/small-ui.md` §3.

### DL-501: The pending restart set is board state (B23, 2026-10-23)

- `restartPending.ts` keeps the pending keys with their first value in the tab state; a key changed back to that value leaves the set (not a secret field). It ends with the tab state, like the notice. Rejected: a `pending_restart` list in `SettingsView` (a schema change of the proto, the deck and every station).

### DL-502: The row of a turn without a start snapshot (B23, 2026-10-23)

- A turn whose start or end side is `unavailable`, or that has no checkpoint, says "No snapshot: <cause>" (or "No snapshot of the files for this turn"); "No file changes" only when both sides are available and the diff has no change. New causes: `checkpoint_missing`, `checkpoint_changed`.

### DL-503: Folds of quiet turns (B23, 2026-10-23)

- In the By turn list, three or more turns in a row that have no diff, are not reverted and offer no enabled revert fold into one closed `<details>` row "Turns <a>–<b>" with one cause, "No snapshot", "No file changes" or "No changes to show". The last turn never folds. The empty states of round 3 stay first.

### DL-504: The artifact panel bar on a touch screen (B23, 2026-10-23)

- The actions of the panel bar take the field size of the version select (the page header rule of `base.css`), and in a narrow bar the select label sits on the gutter (the page rule of `page.css`). The phone panel that a chat chip opens then reads as the board's.

### DL-505: The chip version is the neutral version badge (B23, 2026-10-23)

- The chat chip shows "v<n>" (from v2) as `.fd-badge.fd-art-ver`, the badge of the artifact list, with a 1px inset `--btn-edge-raised` ring on the `--raised` chip. It replaces the muted badge, which read as a part of the title.

### DL-506: Every hover rule in a hover block (B23, 2026-10-23)

- The 40 older rules outside `@media (hover: hover)` move into it (10 split, 1 duplicate removed); the budget of check 5e is 0. Nine guarded rules get the fill of the DS: `--hover-bg` for rows with a selected state, `--raised` for the chat folds, `--badge-bg` for folder rows on a panel or a dialog, the primary hover for the session header confirm; two dead rules go. The three hover fills over `--raised-hi` (`session.css:1093`, `1409`, `1948`) stay for a separate DS decision. List: `~/fleetdeck-briefs/backlog/small-ui/hover-verdicts.tsv`.
